use crate::{scan_project, ProjectInfo, Workspace};
use anyhow::Result;
use std::path::{Path, PathBuf};

const MAX_FILE_CHARS: usize = 2_500;
const MAX_TOTAL_CHARS: usize = 28_000;
const MAX_FILES: usize = 24;

#[derive(Debug, Clone)]
pub struct CodeFileSnippet {
    pub path: String,
    pub content: String,
    pub bytes: usize,
}

#[derive(Debug, Clone)]
pub struct CodebaseDigest {
    pub info: ProjectInfo,
    pub files: Vec<CodeFileSnippet>,
    pub skipped: Vec<String>,
}

impl CodebaseDigest {
    pub fn prompt_block(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "Project scan: {}\nLanguages: {}\nKey manifests: {}\n\n",
            self.info.summary,
            self.info.languages.join(", "),
            self.info.key_files.join(", ")
        ));
        for f in &self.files {
            out.push_str(&format!(
                "===== FILE: {} ({} bytes, excerpt) =====\n{}\n\n",
                f.path, f.bytes, f.content
            ));
        }
        if !self.skipped.is_empty() {
            out.push_str("Also noted but not fully inlined:\n");
            for s in &self.skipped {
                out.push_str(&format!("- {s}\n"));
            }
        }
        out
    }
}

/// Deterministically walk the repo and collect the most important source files
/// (Claude-style orientation), without relying on the LLM to discover paths.
pub async fn gather_codebase(
    ws: &Workspace,
    on_file: &mut dyn FnMut(&str),
) -> Result<CodebaseDigest> {
    let info = scan_project(ws.root());
    let mut files = Vec::new();
    let mut skipped = Vec::new();
    let mut total = 0usize;

    let mut candidates: Vec<PathBuf> = Vec::new();

    // Manifests / docs first
    for name in [
        "README.md",
        "README",
        "Cargo.toml",
        "package.json",
        "pyproject.toml",
        "go.mod",
        "CLAUDE.md",
        "AGENTS.md",
    ] {
        let p = ws.root().join(name);
        if p.is_file() {
            candidates.push(PathBuf::from(name));
        }
    }

    // Workspace crate roots from Cargo.toml members if present
    if let Ok(cargo) = tokio::fs::read_to_string(ws.root().join("Cargo.toml")).await {
        for line in cargo.lines() {
            let t = line.trim();
            if let Some(rest) = t.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
                if !rest.contains('*') && !rest.contains('/') && rest != "members" {
                    // skip bare words; members are usually "vesper-cli"
                }
            }
            // naive: "vesper-cli",
            if let Some(m) = t.strip_prefix('"').and_then(|s| s.strip_suffix("\",").or_else(|| s.strip_suffix('"')))
            {
                if !m.contains(' ') && ws.root().join(m).is_dir() {
                    push_crate_entrypoints(ws.root(), m, &mut candidates);
                }
            }
        }
    }

    // Top-level directories that look like packages
    if let Ok(mut rd) = tokio::fs::read_dir(ws.root()).await {
        while let Ok(Some(ent)) = rd.next_entry().await {
            let name = ent.file_name().to_string_lossy().into_owned();
            if skip_dir(&name) {
                continue;
            }
            if ent.file_type().await?.is_dir() {
                push_crate_entrypoints(ws.root(), &name, &mut candidates);
            } else if is_source_name(&name) {
                candidates.push(PathBuf::from(name));
            }
        }
    }

    // Dedup while preserving order
    let mut seen = std::collections::HashSet::new();
    candidates.retain(|p| seen.insert(p.display().to_string()));

    for rel in candidates {
        if files.len() >= MAX_FILES || total >= MAX_TOTAL_CHARS {
            skipped.push(rel.display().to_string());
            continue;
        }
        let rel_s = rel.display().to_string();
        on_file(&rel_s);
        match ws.read_file(&rel).await {
            Ok(content) => {
                let bytes = content.len();
                let excerpt = truncate_smart(&content, MAX_FILE_CHARS);
                total = total.saturating_add(excerpt.len());
                files.push(CodeFileSnippet {
                    path: rel_s,
                    content: excerpt,
                    bytes,
                });
            }
            Err(_) => skipped.push(rel_s),
        }
    }

    // If still thin, pull a few more .rs/.py/.ts files via shallow walk
    if files.len() < 8 {
        let mut extras = Vec::new();
        shallow_sources(ws.root(), ws.root(), 0, 3, &mut extras).await?;
        for rel in extras {
            let rel_s = rel.display().to_string();
            if files.iter().any(|f| f.path == rel_s) {
                continue;
            }
            if files.len() >= MAX_FILES || total >= MAX_TOTAL_CHARS {
                skipped.push(rel_s);
                break;
            }
            on_file(&rel_s);
            if let Ok(content) = ws.read_file(&rel).await {
                let bytes = content.len();
                let excerpt = truncate_smart(&content, MAX_FILE_CHARS);
                total = total.saturating_add(excerpt.len());
                files.push(CodeFileSnippet {
                    path: rel_s,
                    content: excerpt,
                    bytes,
                });
            }
        }
    }

    Ok(CodebaseDigest {
        info,
        files,
        skipped,
    })
}

fn push_crate_entrypoints(root: &Path, crate_name: &str, out: &mut Vec<PathBuf>) {
    for rel in [
        format!("{crate_name}/Cargo.toml"),
        format!("{crate_name}/package.json"),
        format!("{crate_name}/src/lib.rs"),
        format!("{crate_name}/src/main.rs"),
        format!("{crate_name}/src/mod.rs"),
        format!("{crate_name}/lib.rs"),
        format!("{crate_name}/main.rs"),
        format!("{crate_name}/index.ts"),
        format!("{crate_name}/index.js"),
        format!("{crate_name}/__init__.py"),
    ] {
        if root.join(&rel).is_file() {
            out.push(PathBuf::from(rel));
        }
    }
}

fn skip_dir(name: &str) -> bool {
    matches!(
        name,
        "target"
            | ".git"
            | "node_modules"
            | "dist"
            | "build"
            | ".vesper"
            | ".lydia"
            | "__pycache__"
            | ".venv"
            | "venv"
            | ".idea"
            | ".cursor"
    ) || name.starts_with('.')
}

fn is_source_name(name: &str) -> bool {
    name.ends_with(".rs")
        || name.ends_with(".py")
        || name.ends_with(".ts")
        || name.ends_with(".tsx")
        || name.ends_with(".js")
        || name.ends_with(".go")
        || name.ends_with(".java")
        || name.ends_with(".toml")
        || name.ends_with(".md")
}

fn truncate_smart(content: &str, max: usize) -> String {
    if content.len() <= max {
        return content.to_string();
    }
    let head = max * 3 / 4;
    let tail = max.saturating_sub(head + 32);
    let start_end = floor_boundary(content, head);
    let end_start = floor_boundary(content, content.len().saturating_sub(tail));
    format!(
        "{}\n\n...[truncated]...\n\n{}",
        &content[..start_end],
        &content[end_start..]
    )
}

fn floor_boundary(s: &str, index: usize) -> usize {
    if index >= s.len() {
        return s.len();
    }
    let mut i = index;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

async fn shallow_sources(
    root: &Path,
    dir: &Path,
    depth: usize,
    max_depth: usize,
    out: &mut Vec<PathBuf>,
) -> Result<()> {
    if depth > max_depth || out.len() >= 40 {
        return Ok(());
    }
    let mut rd = tokio::fs::read_dir(dir).await?;
    while let Some(ent) = rd.next_entry().await? {
        let name = ent.file_name().to_string_lossy().into_owned();
        if skip_dir(&name) {
            continue;
        }
        let path = ent.path();
        let ft = ent.file_type().await?;
        if ft.is_dir() {
            Box::pin(shallow_sources(root, &path, depth + 1, max_depth, out)).await?;
        } else if ft.is_file() && is_source_name(&name) {
            if let Ok(rel) = path.strip_prefix(root) {
                // Prefer entrypoints over tests
                let s = rel.to_string_lossy();
                if s.contains("/tests/") || s.contains("test_") {
                    continue;
                }
                out.push(rel.to_path_buf());
            }
        }
    }
    Ok(())
}
