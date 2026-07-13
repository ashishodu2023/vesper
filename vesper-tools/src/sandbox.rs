use anyhow::{bail, Context, Result};
use regex::RegexBuilder;
use std::path::{Component, Path, PathBuf};
use tokio::process::Command;

const MAX_READ_BYTES: usize = 120_000;
const MAX_GREP_MATCHES: usize = 80;
const MAX_GREP_FILE_BYTES: u64 = 1_000_000;
const MAX_FIND_RESULTS: usize = 200;

#[derive(Debug, Clone)]
pub struct Workspace {
    root: PathBuf,
}

impl Workspace {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into().canonicalize().context("invalid workspace root")?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn resolve(&self, rel: impl AsRef<Path>) -> Result<PathBuf> {
        let rel = normalize_rel(rel.as_ref())?;
        let candidate = self.root.join(&rel);
        if !candidate.exists() {
            bail!("path not found: {}", rel.display());
        }
        let canonical = candidate
            .canonicalize()
            .with_context(|| format!("canonicalize {}", candidate.display()))?;
        self.ensure_inside(&canonical)?;
        Ok(canonical)
    }

    pub fn resolve_for_write(&self, rel: impl AsRef<Path>) -> Result<PathBuf> {
        let rel = normalize_rel(rel.as_ref())?;
        let candidate = self.root.join(&rel);
        let mut ancestor = candidate.clone();
        let mut missing = Vec::new();
        while !ancestor.exists() {
            let name = ancestor
                .file_name()
                .map(|s| s.to_os_string())
                .ok_or_else(|| anyhow::anyhow!("invalid path {}", rel.display()))?;
            missing.push(name);
            ancestor = ancestor
                .parent()
                .ok_or_else(|| anyhow::anyhow!("path escapes workspace: {}", rel.display()))?
                .to_path_buf();
        }
        let mut canonical = ancestor.canonicalize()?;
        self.ensure_inside(&canonical)?;
        for part in missing.into_iter().rev() {
            canonical.push(part);
        }
        if !canonical.starts_with(&self.root) {
            bail!("path escapes workspace: {}", canonical.display());
        }
        Ok(canonical)
    }

    fn ensure_inside(&self, path: &Path) -> Result<()> {
        if !path.starts_with(&self.root) {
            bail!("path escapes workspace: {}", path.display());
        }
        Ok(())
    }

    pub fn rel_display(&self, path: &Path) -> String {
        path.strip_prefix(&self.root)
            .unwrap_or(path)
            .display()
            .to_string()
    }

    pub async fn read_file(&self, rel: impl AsRef<Path>) -> Result<String> {
        let path = self.resolve(rel)?;
        let meta = tokio::fs::metadata(&path).await?;
        if meta.len() > MAX_READ_BYTES as u64 {
            let bytes = tokio::fs::read(&path).await?;
            let truncated = String::from_utf8_lossy(&bytes[..MAX_READ_BYTES]);
            return Ok(format!(
                "{truncated}\n\n...[truncated at {MAX_READ_BYTES} bytes; file is {} bytes]",
                meta.len()
            ));
        }
        tokio::fs::read_to_string(&path)
            .await
            .with_context(|| format!("read {}", path.display()))
    }

    pub async fn read_file_range(
        &self,
        rel: impl AsRef<Path>,
        start_line: usize,
        end_line: usize,
    ) -> Result<String> {
        let content = self.read_file(rel).await?;
        let start = start_line.max(1);
        let end = end_line.max(start);
        Ok(content
            .lines()
            .enumerate()
            .filter(|(i, _)| *i + 1 >= start && *i + 1 <= end)
            .map(|(i, line)| format!("{:>4}|{}", i + 1, line))
            .collect::<Vec<_>>()
            .join("\n"))
    }

    pub async fn write_file(&self, rel: impl AsRef<Path>, contents: &str) -> Result<()> {
        let path = self.resolve_for_write(rel)?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&path, contents).await?;
        Ok(())
    }

    pub async fn str_replace(
        &self,
        rel: impl AsRef<Path>,
        old: &str,
        new: &str,
    ) -> Result<String> {
        if old.is_empty() {
            bail!("old_string must not be empty");
        }
        let path = self.resolve(rel.as_ref())?;
        let content = tokio::fs::read_to_string(&path).await?;
        let matches = content.matches(old).count();
        if matches == 0 {
            bail!("old_string not found in {}", rel.as_ref().display());
        }
        if matches > 1 {
            bail!(
                "old_string matched {matches} times in {}; make it unique",
                rel.as_ref().display()
            );
        }
        let updated = content.replacen(old, new, 1);
        tokio::fs::write(&path, &updated).await?;
        Ok(format!("updated {}", rel.as_ref().display()))
    }

    pub async fn multi_str_replace(
        &self,
        rel: impl AsRef<Path>,
        edits: &[(String, String)],
    ) -> Result<String> {
        let path = self.resolve(rel.as_ref())?;
        let mut content = tokio::fs::read_to_string(&path).await?;
        for (old, new) in edits {
            if old.is_empty() {
                bail!("old_string must not be empty");
            }
            let matches = content.matches(old.as_str()).count();
            if matches != 1 {
                bail!(
                    "edit for '{}' matched {matches} times (need exactly 1)",
                    truncate(old, 40)
                );
            }
            content = content.replacen(old, new, 1);
        }
        tokio::fs::write(&path, &content).await?;
        Ok(format!(
            "applied {} edits to {}",
            edits.len(),
            rel.as_ref().display()
        ))
    }

    pub async fn delete_file(&self, rel: impl AsRef<Path>) -> Result<String> {
        let path = self.resolve(rel.as_ref())?;
        if path.is_dir() {
            bail!("refusing to delete directory via delete_file: use shell carefully");
        }
        tokio::fs::remove_file(&path).await?;
        Ok(format!("deleted {}", rel.as_ref().display()))
    }

    pub async fn list_dir(&self, rel: impl AsRef<Path>) -> Result<Vec<String>> {
        let path = if rel.as_ref().as_os_str().is_empty() || rel.as_ref() == Path::new(".") {
            self.root.clone()
        } else {
            self.resolve(rel)?
        };
        let mut entries = tokio::fs::read_dir(&path).await?;
        let mut names = Vec::new();
        while let Some(entry) = entries.next_entry().await? {
            let name = entry.file_name().to_string_lossy().into_owned();
            if skip_name(&name) {
                continue;
            }
            let suffix = if entry.file_type().await?.is_dir() {
                "/"
            } else {
                ""
            };
            names.push(format!("{name}{suffix}"));
        }
        names.sort();
        Ok(names)
    }

    pub async fn find_files(&self, pattern: &str) -> Result<String> {
        let mut out = Vec::new();
        self.find_walk(&self.root, pattern, &mut out).await?;
        if out.is_empty() {
            Ok("(no matches)".into())
        } else {
            Ok(out.join("\n"))
        }
    }

    async fn find_walk(&self, dir: &Path, pattern: &str, out: &mut Vec<String>) -> Result<()> {
        if out.len() >= MAX_FIND_RESULTS {
            return Ok(());
        }
        let mut entries = tokio::fs::read_dir(dir).await?;
        let pat = pattern.to_lowercase();
        while let Some(entry) = entries.next_entry().await? {
            if out.len() >= MAX_FIND_RESULTS {
                out.push(format!("...[truncated at {MAX_FIND_RESULTS}]"));
                break;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if skip_name(&name) {
                continue;
            }
            let path = entry.path();
            let ft = entry.file_type().await?;
            let rel = self.rel_display(&path);
            if ft.is_dir() {
                Box::pin(self.find_walk(&path, pattern, out)).await?;
            } else {
                let name_l = name.to_lowercase();
                let rel_l = rel.to_lowercase();
                let matched = if let Some(ext) = pat.strip_prefix("*.") {
                    name_l.ends_with(&format!(".{ext}"))
                } else {
                    name_l.contains(&pat) || rel_l.contains(&pat) || globish(&rel_l, &pat)
                };
                if matched {
                    out.push(rel);
                }
            }
        }
        Ok(())
    }

    pub async fn grep(&self, pattern: &str, path: Option<&str>) -> Result<String> {
        let re = RegexBuilder::new(pattern)
            .case_insensitive(true)
            .build()
            .with_context(|| format!("invalid regex: {pattern}"))?;
        let start = match path {
            Some(p) if !p.is_empty() && p != "." => self.resolve(p)?,
            _ => self.root.clone(),
        };
        let mut matches = Vec::new();
        self.grep_walk(&start, &re, &mut matches).await?;
        if matches.is_empty() {
            Ok("(no matches)".into())
        } else {
            Ok(matches.join("\n"))
        }
    }

    async fn grep_walk(
        &self,
        dir: &Path,
        re: &regex::Regex,
        out: &mut Vec<String>,
    ) -> Result<()> {
        if out.len() >= MAX_GREP_MATCHES {
            return Ok(());
        }
        let mut entries = tokio::fs::read_dir(dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            if out.len() >= MAX_GREP_MATCHES {
                out.push(format!("...[truncated at {MAX_GREP_MATCHES} matches]"));
                break;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if skip_name(&name) {
                continue;
            }
            let path = entry.path();
            let ft = entry.file_type().await?;
            if ft.is_dir() {
                Box::pin(self.grep_walk(&path, re, out)).await?;
            } else if ft.is_file() {
                let meta = tokio::fs::metadata(&path).await?;
                if meta.len() > MAX_GREP_FILE_BYTES {
                    continue;
                }
                let Ok(content) = tokio::fs::read_to_string(&path).await else {
                    continue;
                };
                let rel = self.rel_display(&path);
                for (i, line) in content.lines().enumerate() {
                    if re.is_match(line) {
                        out.push(format!("{rel}:{}:{}", i + 1, line.trim_end()));
                        if out.len() >= MAX_GREP_MATCHES {
                            break;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    pub async fn run_shell(&self, command: &str) -> Result<CommandOutput> {
        if command.trim().is_empty() {
            bail!("empty command");
        }
        let output = Command::new("bash")
            .arg("-lc")
            .arg(command)
            .current_dir(&self.root)
            .output()
            .await
            .context("failed to spawn shell")?;

        let mut stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let mut stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        const CAP: usize = 40_000;
        if stdout.len() > CAP {
            stdout = format!("{}...\n[stdout truncated]", &stdout[..CAP]);
        }
        if stderr.len() > CAP {
            stderr = format!("{}...\n[stderr truncated]", &stderr[..CAP]);
        }
        Ok(CommandOutput {
            status: output.status.code().unwrap_or(-1),
            stdout,
            stderr,
        })
    }

    pub async fn git_status(&self) -> Result<String> {
        Ok(self.run_shell("git status --short").await?.combined())
    }

    pub async fn git_diff(&self) -> Result<String> {
        Ok(self.run_shell("git diff --no-color").await?.combined())
    }

    pub async fn git_add(&self, paths: &str) -> Result<String> {
        let paths = if paths.trim().is_empty() {
            "."
        } else {
            paths.trim()
        };
        // Only allow relative simple paths
        for p in paths.split_whitespace() {
            if p.starts_with('-') && p != "." {
                // allow flags like -A only if exactly -A or -u
                if p != "-A" && p != "-u" && p != "--all" {
                    bail!("unsupported git add arg: {p}");
                }
            }
        }
        Ok(self
            .run_shell(&format!("git add {paths}"))
            .await?
            .combined())
    }

    pub async fn git_commit(&self, message: &str) -> Result<String> {
        if message.trim().is_empty() {
            bail!("commit message required");
        }
        let escaped = message.replace('\'', "'\\''");
        Ok(self
            .run_shell(&format!("git commit -m '{escaped}'"))
            .await?
            .combined())
    }

    pub async fn git_push(&self) -> Result<String> {
        Ok(self.run_shell("git push").await?.combined())
    }
}

fn skip_name(name: &str) -> bool {
    matches!(
        name,
        "target"
            | ".git"
            | "node_modules"
            | "dist"
            | ".vesper"
            | ".lydia"
            | "__pycache__"
            | ".venv"
            | "venv"
    )
}

fn normalize_rel(rel: &Path) -> Result<PathBuf> {
    if rel.is_absolute() {
        bail!("absolute paths are not allowed: {}", rel.display());
    }
    let mut out = PathBuf::new();
    for comp in rel.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    bail!("path escapes workspace via ..: {}", rel.display());
                }
            }
            Component::Normal(s) => out.push(s),
            Component::RootDir | Component::Prefix(_) => {
                bail!("invalid path component in {}", rel.display());
            }
        }
    }
    Ok(out)
}

fn globish(text: &str, pat: &str) -> bool {
    // very small * matcher
    if !pat.contains('*') {
        return false;
    }
    let parts: Vec<&str> = pat.split('*').collect();
    if parts.is_empty() {
        return true;
    }
    let mut rest = text;
    if !parts[0].is_empty() {
        if let Some(i) = rest.find(parts[0]) {
            rest = &rest[i + parts[0].len()..];
        } else {
            return false;
        }
    }
    for (i, part) in parts.iter().enumerate().skip(1) {
        if part.is_empty() {
            continue;
        }
        if i == parts.len() - 1 {
            return rest.ends_with(part) || rest.contains(part);
        }
        if let Some(idx) = rest.find(part) {
            rest = &rest[idx + part.len()..];
        } else {
            return false;
        }
    }
    true
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        format!("{}…", &s[..n])
    }
}

#[derive(Debug, Clone)]
pub struct CommandOutput {
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

impl CommandOutput {
    pub fn combined(&self) -> String {
        let mut out = String::new();
        if !self.stdout.is_empty() {
            out.push_str(&self.stdout);
        }
        if !self.stderr.is_empty() {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&self.stderr);
        }
        if out.is_empty() {
            format!("(exit {})", self.status)
        } else if self.status != 0 {
            format!("{out}\n(exit {})", self.status)
        } else {
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[tokio::test]
    async fn sandbox_blocks_escape() {
        let dir = tempfile_dir();
        let ws = Workspace::new(&dir).unwrap();
        assert!(ws.resolve("../etc/passwd").is_err());
        let _ = fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn write_and_replace() {
        let dir = tempfile_dir();
        let ws = Workspace::new(&dir).unwrap();
        ws.write_file("a.txt", "hello world").await.unwrap();
        ws.str_replace("a.txt", "world", "vesper").await.unwrap();
        assert_eq!(ws.read_file("a.txt").await.unwrap(), "hello vesper");
        let _ = fs::remove_dir_all(dir);
    }

    fn tempfile_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "vesper-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }
}
