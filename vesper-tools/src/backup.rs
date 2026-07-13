use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupEntry {
    pub id: usize,
    pub original: String,
    pub backup_path: String,
    pub timestamp: u64,
}

pub fn backups_dir(workspace: &Path) -> PathBuf {
    workspace.join(".vesper").join("backups")
}

pub fn backup_file(workspace: &Path, abs_path: &Path) -> Result<Option<PathBuf>> {
    if !abs_path.exists() || !abs_path.is_file() {
        return Ok(None);
    }
    let dir = backups_dir(workspace);
    fs::create_dir_all(&dir)?;
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let rel = abs_path
        .strip_prefix(workspace)
        .unwrap_or(abs_path)
        .to_string_lossy()
        .replace('/', "__");
    let dest = dir.join(format!("{ts}_{rel}"));
    fs::copy(abs_path, &dest)
        .with_context(|| format!("backup {}", abs_path.display()))?;
    Ok(Some(dest))
}

pub fn list_backups(workspace: &Path) -> Result<Vec<BackupEntry>> {
    let dir = backups_dir(workspace);
    if !dir.exists() {
        return Ok(vec![]);
    }
    let mut entries = Vec::new();
    for (id, ent) in fs::read_dir(&dir)?.enumerate() {
        let ent = ent?;
        let name = ent.file_name().to_string_lossy().into_owned();
        let (ts, rest) = name
            .split_once('_')
            .map(|(a, b)| (a.parse::<u64>().unwrap_or(0), b.replace("__", "/")))
            .unwrap_or((0, name.clone()));
        entries.push(BackupEntry {
            id,
            original: rest,
            backup_path: ent.path().display().to_string(),
            timestamp: ts,
        });
    }
    entries.sort_by_key(|e| e.timestamp);
    // re-number after sort
    for (i, e) in entries.iter_mut().enumerate() {
        e.id = i;
    }
    Ok(entries)
}

pub fn restore_backup(workspace: &Path, id: usize) -> Result<String> {
    let entries = list_backups(workspace)?;
    let entry = entries
        .into_iter()
        .find(|e| e.id == id)
        .ok_or_else(|| anyhow::anyhow!("no backup id {id}"))?;
    let dest = workspace.join(&entry.original);
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(&entry.backup_path, &dest)?;
    Ok(format!("restored {} from backup #{id}", entry.original))
}
