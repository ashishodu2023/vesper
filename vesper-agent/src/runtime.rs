use crate::protocol::{is_dangerous_shell, ToolCall};
use anyhow::{bail, Result};
use serde_json::Value;
use std::sync::Mutex;
use vesper_config::SessionMode;
use vesper_memory::ProjectMemory;
use vesper_tools::{backup_file, create_checkpoint, unified_diff, Workspace};

pub type Approver = Box<dyn Fn(&ToolCall, &str) -> bool + Send + Sync>;

pub struct AgentOptions {
    pub max_steps: u32,
    pub mode: SessionMode,
    pub verify_command: Option<String>,
    pub approve: Approver,
}

impl Default for AgentOptions {
    fn default() -> Self {
        Self {
            max_steps: 16,
            mode: SessionMode::Ask,
            verify_command: None,
            approve: Box::new(|_, _| false),
        }
    }
}

impl AgentOptions {
    pub fn for_mode(
        mode: SessionMode,
        max_steps: u32,
        verify_command: Option<String>,
        approve: Approver,
    ) -> Self {
        Self {
            max_steps,
            mode,
            verify_command,
            approve,
        }
    }
}

#[derive(Debug, Clone)]
pub enum AgentEvent {
    Thinking {
        step: u32,
    },
    ToolStart {
        step: u32,
        call: ToolCall,
    },
    DiffPreview {
        path: String,
        diff: String,
    },
    AwaitingApproval {
        call: ToolCall,
        preview: String,
    },
    ToolEnd {
        step: u32,
        ok: bool,
        output: String,
    },
    Todos {
        items: Vec<String>,
    },
    Verify {
        command: String,
        output: String,
        ok: bool,
    },
    Final {
        message: String,
    },
    StreamToken {
        token: String,
    },
    Checkpoint {
        id: usize,
        label: String,
    },
}

#[derive(Debug, Clone)]
pub struct RunResult {
    pub message: String,
    pub steps: u32,
    pub truncated: bool,
}

#[derive(Default)]
pub struct TodoState {
    items: Mutex<Vec<String>>,
}

impl TodoState {
    pub fn set(&self, items: Vec<String>) {
        *self.items.lock().unwrap() = items;
    }

    pub fn snapshot(&self) -> Vec<String> {
        self.items.lock().unwrap().clone()
    }
}

pub struct ToolContext<'a> {
    pub workspace: &'a Workspace,
    pub memory: &'a mut ProjectMemory,
    pub todos: &'a TodoState,
}

/// Build a human preview (diff) for approval without mutating yet.
pub async fn preview_tool(ws: &Workspace, call: &ToolCall) -> Result<String> {
    match call.name.as_str() {
        "write_file" => {
            let path = call
                .arg_str("path")
                .ok_or_else(|| anyhow::anyhow!("path required"))?;
            let content = call
                .arg_str("content")
                .or_else(|| call.arg_str("contents"))
                .unwrap_or_default();
            let before = ws.read_file(&path).await.unwrap_or_default();
            Ok(unified_diff(&path, &before, &content))
        }
        "str_replace" => {
            let path = call
                .arg_str("path")
                .ok_or_else(|| anyhow::anyhow!("path required"))?;
            let old = call
                .arg_str("old_string")
                .or_else(|| call.arg_str("old"))
                .unwrap_or_default();
            let new = call
                .arg_str("new_string")
                .or_else(|| call.arg_str("new"))
                .unwrap_or_default();
            let before = ws.read_file(&path).await.unwrap_or_default();
            let after = before.replacen(&old, &new, 1);
            Ok(unified_diff(&path, &before, &after))
        }
        "multi_str_replace" => {
            let path = call
                .arg_str("path")
                .ok_or_else(|| anyhow::anyhow!("path required"))?;
            let before = ws.read_file(&path).await.unwrap_or_default();
            Ok(format!(
                "multi_str_replace on {path} ({} edits)\n--- current file starts ---\n{}",
                call.args
                    .get("edits")
                    .and_then(|e| e.as_array())
                    .map(|a| a.len())
                    .unwrap_or(0),
                before.chars().take(800).collect::<String>()
            ))
        }
        "delete_file" => Ok(format!(
            "DELETE {}",
            call.arg_str("path").unwrap_or_default()
        )),
        "run_shell" => Ok(format!(
            "$ {}",
            call.arg_str("command")
                .or_else(|| call.arg_str("cmd"))
                .unwrap_or_default()
        )),
        "git_commit" => Ok(format!(
            "git commit -m {:?}",
            call.arg_str("message").unwrap_or_default()
        )),
        "git_push" => Ok("git push".into()),
        other => Ok(other.into()),
    }
}

pub fn needs_approval(mode: SessionMode, call: &ToolCall) -> bool {
    // External plugins always confirm — unknown side effects.
    if call.name.starts_with("mcp_") {
        return true;
    }
    if call.is_readonly() {
        return false;
    }
    match mode {
        SessionMode::Plan => true, // shouldn't run anyway
        SessionMode::Ask => true,
        SessionMode::Auto => call.is_destructive(),
    }
}

pub async fn execute_tool(ctx: &mut ToolContext<'_>, call: &ToolCall) -> Result<String> {
    let ws = ctx.workspace;
    match call.name.as_str() {
        "list_dir" => {
            let path = call.arg_str("path").unwrap_or_else(|| ".".into());
            Ok(ws.list_dir(path).await?.join("\n"))
        }
        "read_file" => {
            let path = call
                .arg_str("path")
                .ok_or_else(|| anyhow::anyhow!("read_file requires path"))?;
            match (call.arg_u64("start_line"), call.arg_u64("end_line")) {
                (Some(s), Some(e)) => ws.read_file_range(path, s as usize, e as usize).await,
                _ => ws.read_file(path).await,
            }
        }
        "find_files" => {
            let pattern = call
                .arg_str("pattern")
                .or_else(|| call.arg_str("glob"))
                .or_else(|| call.arg_str("query"))
                .ok_or_else(|| anyhow::anyhow!("find_files requires pattern"))?;
            ws.find_files(&pattern).await
        }
        "write_file" => {
            let path = call
                .arg_str("path")
                .ok_or_else(|| anyhow::anyhow!("write_file requires path"))?;
            let content = call
                .arg_str("content")
                .or_else(|| call.arg_str("contents"))
                .ok_or_else(|| anyhow::anyhow!("write_file requires content"))?;
            let _ = create_checkpoint(ws.root(), &format!("before write {path}"), &[path.clone()]);
            if let Ok(abs) = ws.resolve_for_write(&path) {
                let _ = backup_file(ws.root(), &abs)?;
            }
            ws.write_file(&path, &content).await?;
            Ok(format!("wrote {path} ({} bytes)", content.len()))
        }
        "str_replace" => {
            let path = call
                .arg_str("path")
                .ok_or_else(|| anyhow::anyhow!("str_replace requires path"))?;
            let old = call
                .arg_str("old_string")
                .or_else(|| call.arg_str("old"))
                .ok_or_else(|| anyhow::anyhow!("str_replace requires old_string"))?;
            let new = call
                .arg_str("new_string")
                .or_else(|| call.arg_str("new"))
                .ok_or_else(|| anyhow::anyhow!("str_replace requires new_string"))?;
            let _ = create_checkpoint(ws.root(), &format!("before edit {path}"), &[path.clone()]);
            match ws.str_replace(&path, &old, &new).await {
                Ok(msg) => Ok(msg),
                Err(err) => {
                    // Edit retry: re-read file and try a softened unique match.
                    let content = ws.read_file(&path).await.unwrap_or_default();
                    if let Some(fixed_old) = soften_old_string(&content, &old) {
                        let msg = ws.str_replace(&path, &fixed_old, &new).await?;
                        Ok(format!("{msg} (auto-retried with normalized match)"))
                    } else {
                        let excerpt: String = content.chars().take(1200).collect();
                        Err(anyhow::anyhow!(
                            "{err:#}\n\nFILE_EXCERPT {path}:\n{excerpt}\n\nRe-read and craft a unique old_string from this file."
                        ))
                    }
                }
            }
        }
        "multi_str_replace" => {
            let path = call
                .arg_str("path")
                .ok_or_else(|| anyhow::anyhow!("multi_str_replace requires path"))?;
            let edits = parse_edits(&call.args)?;
            let _ = create_checkpoint(ws.root(), &format!("before multi-edit {path}"), &[path.clone()]);
            if let Ok(abs) = ws.resolve(&path) {
                let _ = backup_file(ws.root(), &abs)?;
            }
            ws.multi_str_replace(&path, &edits).await
        }
        "delete_file" => {
            let path = call
                .arg_str("path")
                .ok_or_else(|| anyhow::anyhow!("delete_file requires path"))?;
            let _ = create_checkpoint(ws.root(), &format!("before delete {path}"), &[path.clone()]);
            if let Ok(abs) = ws.resolve(&path) {
                let _ = backup_file(ws.root(), &abs)?;
            }
            ws.delete_file(path).await
        }
        "grep" => {
            let pattern = call
                .arg_str("pattern")
                .or_else(|| call.arg_str("query"))
                .ok_or_else(|| anyhow::anyhow!("grep requires pattern"))?;
            let path = call.arg_str("path");
            ws.grep(&pattern, path.as_deref()).await
        }
        "run_shell" => {
            let command = call
                .arg_str("command")
                .or_else(|| call.arg_str("cmd"))
                .ok_or_else(|| anyhow::anyhow!("run_shell requires command"))?;
            if is_dangerous_shell(&command) {
                // still allowed if approved, but refuse absolute catastrophes
                if command.to_lowercase().contains("rm -rf /") {
                    bail!("refusing catastrophic command");
                }
            }
            Ok(ws.run_shell(&command).await?.combined())
        }
        "git_status" => ws.git_status().await,
        "git_diff" => ws.git_diff().await,
        "git_add" => {
            let paths = call.arg_str("paths").unwrap_or_else(|| ".".into());
            ws.git_add(&paths).await
        }
        "git_commit" => {
            let message = call
                .arg_str("message")
                .ok_or_else(|| anyhow::anyhow!("git_commit requires message"))?;
            ws.git_commit(&message).await
        }
        "git_push" => ws.git_push().await,
        "update_todos" => {
            let items = parse_todo_items(&call.args)?;
            ctx.todos.set(items.clone());
            Ok(format!("todos updated ({} items)", items.len()))
        }
        "remember" => {
            let fact = call
                .arg_str("fact")
                .or_else(|| call.arg_str("content"))
                .ok_or_else(|| anyhow::anyhow!("remember requires fact"))?;
            ctx.memory.add(&fact);
            ctx.memory.save(ws.root())?;
            Ok(format!("remembered: {fact}"))
        }
        other => bail!("unknown tool: {other}"),
    }
}

fn parse_edits(args: &Value) -> Result<Vec<(String, String)>> {
    let arr = args
        .get("edits")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow::anyhow!("multi_str_replace requires edits[]"))?;
    let mut out = Vec::new();
    for e in arr {
        let old = e
            .get("old_string")
            .or_else(|| e.get("old"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("edit missing old_string"))?;
        let new = e
            .get("new_string")
            .or_else(|| e.get("new"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("edit missing new_string"))?;
        out.push((old.to_string(), new.to_string()));
    }
    Ok(out)
}

fn parse_todo_items(args: &Value) -> Result<Vec<String>> {
    if let Some(arr) = args.get("items").and_then(|v| v.as_array()) {
        return Ok(arr
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect());
    }
    if let Some(s) = args.get("items").and_then(|v| v.as_str()) {
        return Ok(s.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect());
    }
    bail!("update_todos requires items")
}

pub fn is_mutating(name: &str) -> bool {
    matches!(
        name,
        "write_file"
            | "str_replace"
            | "multi_str_replace"
            | "delete_file"
            | "run_shell"
            | "git_add"
            | "git_commit"
            | "git_push"
    )
}

/// Normalize whitespace / try unique line-based match when exact old_string fails.
fn soften_old_string(content: &str, old: &str) -> Option<String> {
    if content.contains(old) {
        return Some(old.to_string());
    }
    let compact = |s: &str| {
        s.chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>()
    };
    let old_c = compact(old);
    if old_c.len() < 8 {
        return None;
    }
    // Find a unique window in content whose compacted form contains old_c or equals it.
    for line in content.lines() {
        if compact(line) == old_c {
            // ensure unique
            let hits = content
                .lines()
                .filter(|l| compact(l) == old_c)
                .count();
            if hits == 1 {
                return Some(line.to_string());
            }
        }
    }
    // Try multiline: collapse whitespace in file and map back is hard — skip.
    None
}
