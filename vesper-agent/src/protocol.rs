use serde::Deserialize;
use serde_json::Value;
use vesper_config::SessionMode;

pub fn tool_catalog(mode: SessionMode) -> String {
    let mut tools = String::from(
        r#"- list_dir(path?)
- read_file(path, start_line?, end_line?)
- find_files(pattern)           # e.g. "*.rs" or "main"
- grep(pattern, path?)
- git_status()
- git_diff()
- update_todos(items: string[]) # live checklist for multi-step work
- remember(fact)                # persist a project fact
- spawn_subagents(goals: string[], max_steps?)  # run parallel plan-mode explorers
"#,
    );
    if mode != SessionMode::Plan {
        tools.push_str(
            r#"- write_file(path, content)
- str_replace(path, old_string, new_string)
- multi_str_replace(path, edits: [{old_string,new_string}, ...])
- delete_file(path)
- run_shell(command)
- git_add(paths?)
- git_commit(message)
- git_push()
"#,
        );
    }
    tools
}

#[derive(Debug, Clone)]
pub struct ToolCall {
    pub name: String,
    pub args: Value,
}

impl ToolCall {
    pub fn is_readonly(&self) -> bool {
        matches!(
            self.name.as_str(),
            "list_dir"
                | "read_file"
                | "grep"
                | "find_files"
                | "git_status"
                | "git_diff"
                | "update_todos"
                | "remember"
                | "spawn_subagents"
        )
    }

    /// Always confirm even in auto mode.
    pub fn is_destructive(&self) -> bool {
        matches!(
            self.name.as_str(),
            "delete_file" | "git_push"
        ) || (self.name == "run_shell"
            && self
                .arg_str("command")
                .or_else(|| self.arg_str("cmd"))
                .map(|c| is_dangerous_shell(&c))
                .unwrap_or(false))
    }

    pub fn allowed_in(&self, mode: SessionMode) -> bool {
        match mode {
            SessionMode::Plan => self.is_readonly(),
            SessionMode::Ask | SessionMode::Auto => true,
        }
    }

    pub fn arg_str(&self, key: &str) -> Option<String> {
        self.args
            .get(key)
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    }

    pub fn arg_u64(&self, key: &str) -> Option<u64> {
        self.args.get(key).and_then(|v| {
            v.as_u64()
                .or_else(|| v.as_i64().map(|i| i as u64))
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        })
    }
}

pub fn is_dangerous_shell(command: &str) -> bool {
    let lower = command.to_lowercase();
    [
        "rm -rf",
        "rm -fr",
        "mkfs",
        "dd if=",
        ":(){",
        "shutdown",
        "reboot",
        "sudo ",
        "git push --force",
        "git push -f",
        "curl ",
        "wget ",
        "| sh",
        "|bash",
        "> /dev/sd",
    ]
    .iter()
    .any(|b| lower.contains(b))
}

#[derive(Debug, Clone)]
pub enum AgentAction {
    Tool(ToolCall),
    Final { message: String },
}

#[derive(Debug, Deserialize)]
struct RawAction {
    action: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    args: Option<Value>,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    text: Option<String>,
}

pub fn parse_action(raw: &str) -> AgentAction {
    if let Some(json) = extract_json_object(raw) {
        if let Ok(parsed) = serde_json::from_str::<RawAction>(&json) {
            let action = parsed.action.to_lowercase();
            if matches!(action.as_str(), "tool" | "call" | "function") {
                if let Some(name) = parsed.name {
                    return AgentAction::Tool(ToolCall {
                        name,
                        args: parsed.args.unwrap_or(Value::Object(Default::default())),
                    });
                }
            }
            if matches!(action.as_str(), "final" | "answer" | "done") {
                let message = parsed
                    .message
                    .or(parsed.content)
                    .or(parsed.text)
                    .unwrap_or_else(|| raw.trim().to_string());
                return AgentAction::Final { message };
            }
        }
        if let Ok(v) = serde_json::from_str::<Value>(&json) {
            if let Some(name) = v.get("name").and_then(|n| n.as_str()) {
                let args = v
                    .get("arguments")
                    .or_else(|| v.get("args"))
                    .or_else(|| v.get("parameters"))
                    .cloned()
                    .unwrap_or(Value::Object(Default::default()));
                if v.get("action").and_then(|a| a.as_str()) != Some("final") {
                    return AgentAction::Tool(ToolCall {
                        name: name.to_string(),
                        args,
                    });
                }
            }
        }
    }
    AgentAction::Final {
        message: raw.trim().to_string(),
    }
}

fn extract_json_object(s: &str) -> Option<String> {
    if let Some(start) = s.find("```") {
        let after = &s[start + 3..];
        let after = after
            .strip_prefix("json")
            .or_else(|| after.strip_prefix("JSON"))
            .unwrap_or(after)
            .trim_start_matches('\n');
        if let Some(end) = after.find("```") {
            let block = after[..end].trim();
            if block.starts_with('{') {
                return Some(block.to_string());
            }
        }
    }
    let bytes = s.as_bytes();
    let mut start = None;
    let mut depth = 0i32;
    let mut in_str = false;
    let mut escape = false;
    for (i, &b) in bytes.iter().enumerate() {
        if in_str {
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == b'"' {
                in_str = false;
            }
            continue;
        }
        match b {
            b'"' => in_str = true,
            b'{' => {
                if depth == 0 {
                    start = Some(i);
                }
                depth += 1;
            }
            b'}' => {
                if depth > 0 {
                    depth -= 1;
                    if depth == 0 {
                        if let Some(s0) = start {
                            return Some(s[s0..=i].to_string());
                        }
                    }
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_tool() {
        let raw = "Let me.\n{\"action\":\"tool\",\"name\":\"list_dir\",\"args\":{\"path\":\".\"}}\n";
        match parse_action(raw) {
            AgentAction::Tool(c) => assert_eq!(c.name, "list_dir"),
            _ => panic!(),
        }
    }
}
