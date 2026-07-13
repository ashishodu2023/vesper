use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Turn {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Default, Clone)]
pub struct SessionMemory {
    turns: Vec<Turn>,
}

impl SessionMemory {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, role: impl Into<String>, content: impl Into<String>) {
        self.turns.push(Turn {
            role: role.into(),
            content: content.into(),
        });
    }

    pub fn turns(&self) -> &[Turn] {
        &self.turns
    }

    pub fn clear(&mut self) {
        self.turns.clear();
    }

    pub fn recent(&self, n: usize) -> Vec<&Turn> {
        let len = self.turns.len();
        let start = len.saturating_sub(n);
        self.turns[start..].iter().collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProjectMemory {
    pub facts: Vec<String>,
}

impl ProjectMemory {
    pub fn path(workspace: &Path) -> PathBuf {
        workspace.join(".vesper").join("memory.json")
    }

    pub fn load(workspace: &Path) -> Result<Self> {
        let path = Self::path(workspace);
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = fs::read_to_string(&path)?;
        Ok(serde_json::from_str(&raw).context("parse memory.json")?)
    }

    pub fn save(&self, workspace: &Path) -> Result<()> {
        let path = Self::path(workspace);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    pub fn add(&mut self, fact: impl Into<String>) {
        let fact = fact.into().trim().to_string();
        if !fact.is_empty() && !self.facts.iter().any(|f| f == &fact) {
            self.facts.push(fact);
        }
    }

    pub fn forget(&mut self, index: usize) -> Result<String> {
        if index >= self.facts.len() {
            anyhow::bail!("no fact at index {index}");
        }
        Ok(self.facts.remove(index))
    }

    pub fn prompt_block(&self) -> String {
        if self.facts.is_empty() {
            return String::new();
        }
        let mut out = String::from("Remembered project facts:\n");
        for (i, f) in self.facts.iter().enumerate() {
            out.push_str(&format!("  {}. {}\n", i, f));
        }
        out
    }
}
