use vesper_config::SessionMode;
use vesper_llm::{ToolFunctionSpec, ToolSpec};

pub fn ollama_tool_specs(mode: SessionMode) -> Vec<ToolSpec> {
    let mut tools = vec![
        tool(
            "list_dir",
            "List files in a directory under the workspace",
            obj(&[("path", str_prop("relative directory, default ."))], &[]),
        ),
        tool(
            "read_file",
            "Read a file (optional line range)",
            obj(
                &[
                    ("path", str_prop("relative file path")),
                    ("start_line", num_prop("optional")),
                    ("end_line", num_prop("optional")),
                ],
                &["path"],
            ),
        ),
        tool(
            "find_files",
            "Find files by name/pattern",
            obj(&[("pattern", str_prop("e.g. *.rs or main"))], &["pattern"]),
        ),
        tool(
            "grep",
            "Search file contents with regex",
            obj(
                &[
                    ("pattern", str_prop("regex")),
                    ("path", str_prop("optional subdirectory")),
                ],
                &["pattern"],
            ),
        ),
        tool("git_status", "Short git status", obj(&[], &[])),
        tool("git_diff", "Unstaged git diff", obj(&[], &[])),
        tool(
            "update_todos",
            "Update live checklist",
            obj(&[("items", arr_str("todo strings"))], &["items"]),
        ),
        tool(
            "remember",
            "Persist a project fact",
            obj(&[("fact", str_prop("fact text"))], &["fact"]),
        ),
    ];

    if mode != SessionMode::Plan {
        tools.extend([
            tool(
                "write_file",
                "Create or overwrite a file",
                obj(
                    &[
                        ("path", str_prop("path")),
                        ("content", str_prop("full contents")),
                    ],
                    &["path", "content"],
                ),
            ),
            tool(
                "str_replace",
                "Replace one unique string in a file",
                obj(
                    &[
                        ("path", str_prop("path")),
                        ("old_string", str_prop("exact unique old text")),
                        ("new_string", str_prop("replacement")),
                    ],
                    &["path", "old_string", "new_string"],
                ),
            ),
            tool(
                "run_shell",
                "Run a bash command in the workspace",
                obj(&[("command", str_prop("bash command"))], &["command"]),
            ),
            tool(
                "git_commit",
                "Create a git commit",
                obj(&[("message", str_prop("commit message"))], &["message"]),
            ),
        ]);
    }
    tools
}

fn tool(name: &str, description: &str, parameters: serde_json::Value) -> ToolSpec {
    ToolSpec {
        type_: "function",
        function: ToolFunctionSpec {
            name: name.into(),
            description: description.into(),
            parameters,
        },
    }
}

fn str_prop(desc: &str) -> serde_json::Value {
    serde_json::json!({"type":"string","description":desc})
}
fn num_prop(desc: &str) -> serde_json::Value {
    serde_json::json!({"type":"integer","description":desc})
}
fn arr_str(desc: &str) -> serde_json::Value {
    serde_json::json!({"type":"array","description":desc,"items":{"type":"string"}})
}
fn obj(props: &[(&str, serde_json::Value)], required: &[&str]) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for (k, v) in props {
        map.insert((*k).into(), v.clone());
    }
    serde_json::json!({
        "type": "object",
        "properties": map,
        "required": required,
    })
}
