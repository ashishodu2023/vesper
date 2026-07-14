use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SessionMode {
    Plan,
    #[default]
    Ask,
    Auto,
}

impl SessionMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Plan => "plan",
            Self::Ask => "ask",
            Self::Auto => "auto",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "plan" => Some(Self::Plan),
            "ask" => Some(Self::Ask),
            "auto" => Some(Self::Auto),
            _ => None,
        }
    }

    pub fn cycle(self) -> Self {
        match self {
            Self::Plan => Self::Ask,
            Self::Ask => Self::Auto,
            Self::Auto => Self::Plan,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct McpServerConfig {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: std::collections::HashMap<String, String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

impl Default for McpServerConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            command: String::new(),
            args: Vec::new(),
            env: Default::default(),
            enabled: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default = "default_ollama")]
    pub ollama_host: String,
    #[serde(default)]
    pub mode: SessionMode,
    #[serde(default)]
    pub verify_command: Option<String>,
    #[serde(default = "default_max_steps")]
    pub max_steps: u32,
    #[serde(default = "default_temperature")]
    pub temperature: f32,
    #[serde(default = "default_num_ctx")]
    pub num_ctx: u32,
    #[serde(default = "default_num_predict")]
    pub num_predict: i32,
    #[serde(default = "default_keep_alive")]
    pub keep_alive: String,
    /// MCP stdio plugin servers (tools exposed as `mcp_<server>_<tool>`).
    #[serde(default)]
    pub mcp_servers: Vec<McpServerConfig>,
    #[serde(default = "default_watch_interval")]
    pub watch_interval: u64,
}

fn default_model() -> String {
    "qwen2.5-coder:14b".into()
}
fn default_ollama() -> String {
    "http://127.0.0.1:11434".into()
}
fn default_max_steps() -> u32 {
    16
}
fn default_temperature() -> f32 {
    0.2
}
fn default_num_ctx() -> u32 {
    4096
}
fn default_num_predict() -> i32 {
    640
}
fn default_keep_alive() -> String {
    "30m".into()
}
fn default_watch_interval() -> u64 {
    5
}

impl Default for Config {
    fn default() -> Self {
        Self {
            model: default_model(),
            ollama_host: default_ollama(),
            mode: SessionMode::Ask,
            verify_command: None,
            max_steps: default_max_steps(),
            temperature: default_temperature(),
            num_ctx: default_num_ctx(),
            num_predict: default_num_predict(),
            keep_alive: default_keep_alive(),
            mcp_servers: Vec::new(),
            watch_interval: default_watch_interval(),
        }
    }
}

impl Config {
    pub fn global_path() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".vesper")
            .join("config.json")
    }

    pub fn project_dir(workspace: &Path) -> PathBuf {
        workspace.join(".vesper")
    }

    pub fn project_path(workspace: &Path) -> PathBuf {
        Self::project_dir(workspace).join("config.json")
    }

    pub fn load(workspace: &Path) -> Result<Self> {
        let mut cfg = Self::default();
        if let Ok(raw) = fs::read_to_string(Self::global_path()) {
            let g: Config = serde_json::from_str(&raw).context("parse global config")?;
            cfg.merge(g);
        }
        let proj = Self::project_path(workspace);
        if let Ok(raw) = fs::read_to_string(&proj) {
            let p: Config = serde_json::from_str(&raw).context("parse project config")?;
            cfg.merge(p);
        }
        // Env overrides
        if let Ok(m) = std::env::var("VESPER_MODEL") {
            cfg.model = m;
        }
        if let Ok(u) = std::env::var("VESPER_OLLAMA_URL") {
            cfg.ollama_host = u;
        }
        Ok(cfg)
    }

    fn merge(&mut self, other: Config) {
        // other wins for set-like fields; we always replace since both are full structs from file
        // For partial files, serde defaults fill missing — so merge by re-reading with Option fields
        // would be better. For MVP: project/global files are full overlays of whatever keys present
        // via a PartialConfig approach:
        *self = other; // caller should merge properly — see load_partial
    }

    pub fn save_project(&self, workspace: &Path) -> Result<()> {
        let dir = Self::project_dir(workspace);
        fs::create_dir_all(&dir)?;
        let path = Self::project_path(workspace);
        fs::write(&path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    pub fn save_global(&self) -> Result<()> {
        let path = Self::global_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    pub fn suggest_verify(workspace: &Path) -> Option<String> {
        if workspace.join("Cargo.toml").exists() {
            Some("cargo test".into())
        } else if workspace.join("pyproject.toml").exists() || workspace.join("pytest.ini").exists()
        {
            Some("pytest -q".into())
        } else if workspace.join("package.json").exists() {
            Some("npm test".into())
        } else if workspace.join("go.mod").exists() {
            Some("go test ./...".into())
        } else {
            None
        }
    }
}

/// Partial overlay so project config can set only some keys.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConfigOverlay {
    pub model: Option<String>,
    pub ollama_host: Option<String>,
    pub mode: Option<SessionMode>,
    pub verify_command: Option<String>,
    pub max_steps: Option<u32>,
    pub temperature: Option<f32>,
    pub num_ctx: Option<u32>,
    pub num_predict: Option<i32>,
    pub keep_alive: Option<String>,
    pub mcp_servers: Option<Vec<McpServerConfig>>,
    pub watch_interval: Option<u64>,
}

impl Config {
    pub fn load_layered(workspace: &Path) -> Result<Self> {
        let mut cfg = Self::default();
        apply_file(&mut cfg, &Self::global_path())?;
        apply_file(&mut cfg, &Self::project_path(workspace))?;
        if let Ok(m) = std::env::var("VESPER_MODEL") {
            cfg.model = m;
        }
        if let Ok(u) = std::env::var("VESPER_OLLAMA_URL") {
            cfg.ollama_host = u;
        }
        Ok(cfg)
    }
}

fn apply_file(cfg: &mut Config, path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let raw = fs::read_to_string(path)
        .with_context(|| format!("read {}", path.display()))?;
    let o: ConfigOverlay = serde_json::from_str(&raw)
        .with_context(|| format!("parse {}", path.display()))?;
    if let Some(v) = o.model {
        cfg.model = v;
    }
    if let Some(v) = o.ollama_host {
        cfg.ollama_host = v;
    }
    if let Some(v) = o.mode {
        cfg.mode = v;
    }
    if let Some(v) = o.verify_command {
        cfg.verify_command = Some(v);
    }
    if let Some(v) = o.max_steps {
        cfg.max_steps = v;
    }
    if let Some(v) = o.temperature {
        cfg.temperature = v;
    }
    if let Some(v) = o.num_ctx {
        cfg.num_ctx = v;
    }
    if let Some(v) = o.num_predict {
        cfg.num_predict = v;
    }
    if let Some(v) = o.keep_alive {
        cfg.keep_alive = v;
    }
    if let Some(v) = o.mcp_servers {
        cfg.mcp_servers = v;
    }
    if let Some(v) = o.watch_interval {
        cfg.watch_interval = v;
    }
    Ok(())
}

pub fn set_key(workspace: &Path, key: &str, value: &str, project: bool) -> Result<()> {
    let path = if project {
        let dir = Config::project_dir(workspace);
        fs::create_dir_all(&dir)?;
        Config::project_path(workspace)
    } else {
        let p = Config::global_path();
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent)?;
        }
        p
    };
    let mut overlay: ConfigOverlay = if path.exists() {
        serde_json::from_str(&fs::read_to_string(&path)?)?
    } else {
        ConfigOverlay::default()
    };
    match key {
        "model" => overlay.model = Some(value.into()),
        "ollama_host" | "ollama_url" => overlay.ollama_host = Some(value.into()),
        "mode" => {
            overlay.mode = Some(
                SessionMode::parse(value).context("mode must be plan|ask|auto")?,
            );
        }
        "verify_command" => overlay.verify_command = Some(value.into()),
        "max_steps" => overlay.max_steps = Some(value.parse()?),
        "temperature" => overlay.temperature = Some(value.parse()?),
        "num_ctx" => overlay.num_ctx = Some(value.parse()?),
        "num_predict" => overlay.num_predict = Some(value.parse()?),
        "keep_alive" => overlay.keep_alive = Some(value.into()),
        "watch_interval" => overlay.watch_interval = Some(value.parse()?),
        other => anyhow::bail!("unknown config key: {other}"),
    }
    fs::write(&path, serde_json::to_string_pretty(&overlay)?)?;
    Ok(())
}
