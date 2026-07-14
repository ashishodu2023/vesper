use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::backup::{backup_file, backups_dir};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    pub id: usize,
    pub label: String,
    pub timestamp: u64,
    pub files: Vec<CheckpointFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointFile {
    pub path: String,
    pub backup: String,
}

fn checkpoints_path(workspace: &Path) -> PathBuf {
    workspace.join(".vesper").join("checkpoints.json")
}

pub fn list_checkpoints(workspace: &Path) -> Result<Vec<Checkpoint>> {
    let path = checkpoints_path(workspace);
    if !path.exists() {
        return Ok(vec![]);
    }
    let raw = fs::read_to_string(&path)?;
    Ok(serde_json::from_str(&raw).unwrap_or_default())
}

fn save_all(workspace: &Path, items: &[Checkpoint]) -> Result<()> {
    let path = checkpoints_path(workspace);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serde_json::to_string_pretty(items)?)?;
    Ok(())
}

/// Snapshot current versions of touched relative paths before a mutating edit.
pub fn create_checkpoint(
    workspace: &Path,
    label: &str,
    rel_paths: &[String],
) -> Result<Checkpoint> {
    let mut files = Vec::new();
    for rel in rel_paths {
        let abs = workspace.join(rel);
        if !abs.is_file() {
            continue;
        }
        if let Some(backup) = backup_file(workspace, &abs)? {
            files.push(CheckpointFile {
                path: rel.clone(),
                backup: backup.display().to_string(),
            });
        }
    }
    if files.is_empty() {
        // still record empty checkpoint metadata so undo UX is consistent
    }
    let mut all = list_checkpoints(workspace)?;
    let id = all.len();
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let cp = Checkpoint {
        id,
        label: label.to_string(),
        timestamp: ts,
        files,
    };
    all.push(cp.clone());
    // keep last 30
    if all.len() > 30 {
        let drop = all.len() - 30;
        all.drain(0..drop);
        for (i, c) in all.iter_mut().enumerate() {
            c.id = i;
        }
    }
    save_all(workspace, &all)?;
    let _ = backups_dir(workspace);
    Ok(cp)
}

pub fn restore_checkpoint(workspace: &Path, id: usize) -> Result<String> {
    let all = list_checkpoints(workspace)?;
    let cp = all
        .into_iter()
        .find(|c| c.id == id)
        .ok_or_else(|| anyhow::anyhow!("no checkpoint #{id}"))?;
    let mut restored = 0usize;
    for f in &cp.files {
        let dest = workspace.join(&f.path);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(&f.backup, &dest)
            .with_context(|| format!("restore {} from {}", f.path, f.backup))?;
        restored += 1;
    }
    Ok(format!(
        "restored checkpoint #{id} ({}) — {restored} file(s)",
        cp.label
    ))
}

pub fn undo_last(workspace: &Path) -> Result<String> {
    let all = list_checkpoints(workspace)?;
    let last = all
        .last()
        .ok_or_else(|| anyhow::anyhow!("no checkpoints yet"))?;
    restore_checkpoint(workspace, last.id)
}
