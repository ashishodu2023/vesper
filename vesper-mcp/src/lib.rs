//! Minimal MCP stdio host: initialize → tools/list → tools/call.

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;
use vesper_config::McpServerConfig;
use vesper_llm::{ToolFunctionSpec, ToolSpec};

#[derive(Debug, Clone)]
pub struct McpToolInfo {
    pub server: String,
    pub name: String,
    pub qualified: String,
    pub description: String,
    pub input_schema: Value,
}

pub struct McpHub {
    sessions: Vec<McpSession>,
}

impl McpHub {
    pub fn empty() -> Self {
        Self {
            sessions: Vec::new(),
        }
    }

    pub async fn connect_all(configs: &[McpServerConfig]) -> Self {
        let mut sessions = Vec::new();
        for cfg in configs.iter().filter(|c| c.enabled) {
            match McpSession::connect(cfg).await {
                Ok(s) => {
                    tracing::info!(
                        server = %cfg.name,
                        tools = s.tools.len(),
                        "mcp connected"
                    );
                    sessions.push(s);
                }
                Err(err) => {
                    tracing::warn!(server = %cfg.name, error = %err, "mcp connect failed");
                    eprintln!("[vesper] mcp `{}` failed: {err:#}", cfg.name);
                }
            }
        }
        Self { sessions }
    }

    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    pub fn tools(&self) -> Vec<McpToolInfo> {
        self.sessions
            .iter()
            .flat_map(|s| s.tools.iter().cloned())
            .collect()
    }

    pub fn catalog_lines(&self) -> String {
        let tools = self.tools();
        if tools.is_empty() {
            return String::new();
        }
        let mut out = String::from("MCP tools (plugins):\n");
        for t in &tools {
            out.push_str(&format!(
                "- {} — {}\n",
                t.qualified,
                if t.description.is_empty() {
                    t.name.as_str()
                } else {
                    t.description.as_str()
                }
            ));
        }
        out
    }

    pub fn ollama_specs(&self) -> Vec<ToolSpec> {
        self.tools()
            .into_iter()
            .map(|t| ToolSpec {
                type_: "function",
                function: ToolFunctionSpec {
                    name: t.qualified,
                    description: format!("[MCP:{}] {}", t.server, t.description),
                    parameters: if t.input_schema.is_null() {
                        json!({"type":"object","properties":{}})
                    } else {
                        t.input_schema
                    },
                },
            })
            .collect()
    }

    pub fn is_mcp_tool(&self, name: &str) -> bool {
        self.tools().iter().any(|t| t.qualified == name)
    }

    pub async fn call(&self, qualified: &str, arguments: &Value) -> Result<String> {
        for session in &self.sessions {
            if let Some(tool) = session.tools.iter().find(|t| t.qualified == qualified) {
                return session.call_tool(&tool.name, arguments).await;
            }
        }
        Err(anyhow!("unknown MCP tool: {qualified}"))
    }

    pub fn summary(&self) -> String {
        if self.sessions.is_empty() {
            return "(no MCP servers connected)".into();
        }
        self.sessions
            .iter()
            .map(|s| format!("{} ({} tools)", s.server_name, s.tools.len()))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

struct McpSession {
    server_name: String,
    tools: Vec<McpToolInfo>,
    inner: Mutex<SessionIo>,
    next_id: AtomicU64,
}

struct SessionIo {
    _child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl McpSession {
    async fn connect(cfg: &McpServerConfig) -> Result<Self> {
        let mut cmd = Command::new(&cfg.command);
        cmd.args(&cfg.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        for (k, v) in &cfg.env {
            cmd.env(k, v);
        }
        let mut child = cmd
            .spawn()
            .with_context(|| format!("spawn MCP server `{}`", cfg.name))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("no stdin for MCP `{}`", cfg.name))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("no stdout for MCP `{}`", cfg.name))?;
        let mut io = SessionIo {
            _child: child,
            stdin,
            stdout: BufReader::new(stdout),
        };
        let next_id = AtomicU64::new(1);
        let id = next_id.fetch_add(1, Ordering::SeqCst);
        let init = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": { "tools": {} },
                "clientInfo": { "name": "vesper", "version": "0.1.0" }
            }
        });
        let _ = request(&mut io, init).await?;
        notify(
            &mut io,
            json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized"
            }),
        )
        .await?;

        let id = next_id.fetch_add(1, Ordering::SeqCst);
        let listed = request(
            &mut io,
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "tools/list",
                "params": {}
            }),
        )
        .await?;
        let tools_raw = listed
            .get("result")
            .and_then(|r| r.get("tools"))
            .and_then(|t| t.as_array())
            .cloned()
            .unwrap_or_default();

        let mut tools = Vec::new();
        for t in tools_raw {
            let name = t
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if name.is_empty() {
                continue;
            }
            let description = t
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let input_schema = t
                .get("inputSchema")
                .cloned()
                .unwrap_or_else(|| json!({"type":"object","properties":{}}));
            let qualified = qualify(&cfg.name, &name);
            tools.push(McpToolInfo {
                server: cfg.name.clone(),
                name,
                qualified,
                description,
                input_schema,
            });
        }

        Ok(Self {
            server_name: cfg.name.clone(),
            tools,
            inner: Mutex::new(io),
            next_id,
        })
    }

    async fn call_tool(&self, name: &str, arguments: &Value) -> Result<String> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let req = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {
                "name": name,
                "arguments": arguments
            }
        });
        let mut io = self.inner.lock().await;
        let resp = request(&mut io, req).await?;
        if let Some(err) = resp.get("error") {
            return Err(anyhow!("MCP tool error: {err}"));
        }
        let result = resp.get("result").cloned().unwrap_or(Value::Null);
        if let Some(content) = result.get("content").and_then(|c| c.as_array()) {
            let mut texts = Vec::new();
            for block in content {
                if let Some(t) = block.get("text").and_then(|v| v.as_str()) {
                    texts.push(t.to_string());
                } else {
                    texts.push(block.to_string());
                }
            }
            if !texts.is_empty() {
                return Ok(texts.join("\n"));
            }
        }
        Ok(result.to_string())
    }
}

fn qualify(server: &str, tool: &str) -> String {
    let clean = |s: &str| {
        s.chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect::<String>()
    };
    format!("mcp_{}_{}", clean(server), clean(tool))
}

async fn notify(io: &mut SessionIo, msg: Value) -> Result<()> {
    let line = serde_json::to_string(&msg)?;
    io.stdin.write_all(line.as_bytes()).await?;
    io.stdin.write_all(b"\n").await?;
    io.stdin.flush().await?;
    Ok(())
}

async fn request(io: &mut SessionIo, msg: Value) -> Result<Value> {
    let expect_id = msg.get("id").cloned();
    let line = serde_json::to_string(&msg)?;
    io.stdin.write_all(line.as_bytes()).await?;
    io.stdin.write_all(b"\n").await?;
    io.stdin.flush().await?;

    let mut buf = String::new();
    loop {
        buf.clear();
        let n = io
            .stdout
            .read_line(&mut buf)
            .await
            .context("read MCP stdout")?;
        if n == 0 {
            return Err(anyhow!("MCP server closed stdout"));
        }
        let trimmed = buf.trim();
        if trimmed.is_empty() {
            continue;
        }
        let parsed: Value = serde_json::from_str(trimmed)
            .with_context(|| format!("bad MCP JSON: {trimmed}"))?;
        // Skip notifications / unmatched ids.
        if parsed.get("method").is_some() && parsed.get("id").is_none() {
            continue;
        }
        if expect_id.is_some() && parsed.get("id") != expect_id.as_ref() {
            continue;
        }
        return Ok(parsed);
    }
}
