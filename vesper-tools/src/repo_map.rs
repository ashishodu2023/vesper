use crate::Workspace;
use anyhow::Result;
use std::path::{Path, PathBuf};

/// Ranked file map for agent bootstrap (Claude-like orientation).
pub async fn build_repo_map(ws: &Workspace, query: &str) -> Result<String> {
    let root = ws.root();
    let mut files: Vec<(i32, String)> = Vec::new();

    for p in important_paths(root) {
        files.push((20, p.display().to_string()));
    }

    collect(root, root, 0, 4, &mut files).await?;

    let q = query.to_lowercase();
    let tokens: Vec<&str> = q
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() > 2)
        .collect();

    for (score, path) in files.iter_mut() {
        let lower = path.to_lowercase();
        for t in &tokens {
            if lower.contains(t) {
                *score += 8;
            }
        }
        if lower.ends_with("readme.md") || lower.ends_with("cargo.toml") {
            *score += 5;
        }
        if lower.contains("/src/") && (lower.ends_with("main.rs") || lower.ends_with("lib.rs")) {
            *score += 6;
        }
        if lower.contains("test") {
            *score -= 2;
        }
    }

    files.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    files.dedup_by(|a, b| a.1 == b.1);
    files.truncate(40);

    let mut out = String::from("Repo map (ranked paths):\n");
    for (i, (score, path)) in files.iter().enumerate() {
        out.push_str(&format!("{:>2}. [{score}] {path}\n", i + 1));
    }
    Ok(out)
}

async fn collect(
    root: &Path,
    dir: &Path,
    depth: usize,
    max_depth: usize,
    out: &mut Vec<(i32, String)>,
) -> Result<()> {
    if depth > max_depth || out.len() > 400 {
        return Ok(());
    }
    let mut rd = match tokio::fs::read_dir(dir).await {
        Ok(r) => r,
        Err(_) => return Ok(()),
    };
    while let Some(ent) = rd.next_entry().await? {
        let name = ent.file_name().to_string_lossy().into_owned();
        if skip(&name) {
            continue;
        }
        let path = ent.path();
        let ft = ent.file_type().await?;
        if ft.is_dir() {
            Box::pin(collect(root, &path, depth + 1, max_depth, out)).await?;
        } else if ft.is_file() && is_codeish(&name) {
            if let Ok(rel) = path.strip_prefix(root) {
                out.push((0, rel.display().to_string()));
            }
        }
    }
    Ok(())
}

fn skip(name: &str) -> bool {
    matches!(
        name,
        "target"
            | "node_modules"
            | ".git"
            | "dist"
            | "build"
            | ".vesper"
            | "__pycache__"
            | ".venv"
            | "venv"
            | ".idea"
            | ".cursor"
    ) || name.starts_with('.')
}

fn is_codeish(name: &str) -> bool {
    [
        ".rs", ".py", ".ts", ".tsx", ".js", ".go", ".java", ".toml", ".md", ".yaml", ".yml",
        ".json",
    ]
    .iter()
    .any(|ext| name.ends_with(ext))
}

pub fn important_paths(root: &Path) -> Vec<PathBuf> {
    let mut v = Vec::new();
    for name in ["README.md", "Cargo.toml", "package.json", "pyproject.toml"] {
        let p = root.join(name);
        if p.is_file() {
            v.push(PathBuf::from(name));
        }
    }
    v
}
