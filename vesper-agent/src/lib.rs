mod protocol;
mod runtime;

pub use protocol::{parse_action, tool_catalog, AgentAction, ToolCall};
pub use runtime::{AgentEvent, AgentOptions, RunResult, TodoState};
pub use vesper_config::SessionMode;

use anyhow::Result;
use runtime::{
    execute_tool, is_mutating, needs_approval, preview_tool, AgentEvent as Ev, AgentOptions as Opts,
    ToolContext,
};
use vesper_config::SessionMode as Mode;
use vesper_llm::{ChatMessage, LlmClient};
use vesper_memory::{ProjectMemory, SessionMemory};
use vesper_tools::{gather_codebase, scan_project, Workspace};

const CHAT_SYSTEM: &str = r#"You are Vesper, a local coding agent.
Be concise, precise, and practical. Prefer actionable steps over fluff.
"#;

fn agent_system(
    workspace: &str,
    mode: Mode,
    project_summary: &str,
    memory_block: &str,
    verify: Option<&str>,
    listing: &str,
) -> String {
    let verify_line = verify
        .map(|c| {
            format!(
                "After code changes, verification runs automatically: `{c}`. Fix failures before finishing."
            )
        })
        .unwrap_or_else(|| {
            "If you change code, run project tests via run_shell before finishing.".into()
        });

    format!(
        r#"You are Vesper, a local-first coding agent (Rust). Nothing leaves this machine.
Workspace: {workspace}
Mode: {mode_str} ({mode_help})
Project: {project_summary}
Top-level entries (already known — do NOT assume src/main.rs exists):
{listing}
{memory_block}
{verify_line}

On EVERY turn output EXACTLY one JSON object (no markdown fences, no prose outside JSON):

1) Call a tool:
{{"action":"tool","name":"list_dir","args":{{"path":"."}}}}

2) Finish:
{{"action":"final","message":"your answer to the user"}}

Available tools:
{tools}

Rules:
- Answer the USER's question. Do not narrate tool failures as the final answer.
- NEVER retry the exact same failing tool call. Pick a different path or finish.
- Paths must come from list_dir / find_files / grep results — never invent src/main.rs.
- For vague questions ("what's happening?", "what is this?"), read README.md if present, then give a short project overview and finish.
- Prefer find_files/grep/read_file before editing.
- Prefer str_replace / multi_str_replace over write_file for existing files.
- Use update_todos for multi-step work.
- Never invent file contents — read them.
- Keep shell non-destructive unless required.
- After 2–4 informative tool results, prefer action=final unless you still need a specific file.
- README.md / project listing are ALREADY in context when provided — do NOT re-read README.md.
- Be brief. Prefer short final answers.
"#,
        mode_str = mode.as_str(),
        mode_help = match mode {
            Mode::Plan => "research only — mutating tools disabled",
            Mode::Ask => "mutating tools require user approval",
            Mode::Auto => "routine edits auto-approved; destructive still confirmed",
        },
        tools = tool_catalog(mode),
    )
}

pub struct Agent<C: LlmClient> {
    llm: C,
    workspace: Workspace,
    session: SessionMemory,
    project_memory: ProjectMemory,
    pub mode: Mode,
    pub verify_command: Option<String>,
    todos: TodoState,
}

impl<C: LlmClient> Agent<C> {
    pub fn new(llm: C, workspace: Workspace) -> Self {
        let project_memory = ProjectMemory::load(workspace.root()).unwrap_or_default();
        Self {
            llm,
            workspace,
            session: SessionMemory::new(),
            project_memory,
            mode: Mode::Ask,
            verify_command: None,
            todos: TodoState::default(),
        }
    }

    pub fn workspace(&self) -> &Workspace {
        &self.workspace
    }

    pub fn project_memory(&self) -> &ProjectMemory {
        &self.project_memory
    }

    pub fn project_memory_mut(&mut self) -> &mut ProjectMemory {
        &mut self.project_memory
    }

    pub fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
    }

    pub fn cycle_mode(&mut self) -> Mode {
        self.mode = self.mode.cycle();
        self.mode
    }

    pub fn clear_session(&mut self) {
        self.session.clear();
    }

    pub async fn ask(&mut self, prompt: &str) -> Result<String> {
        let mut messages = vec![ChatMessage::system(CHAT_SYSTEM)];
        let info = scan_project(self.workspace.root());
        let listing = self
            .workspace
            .list_dir(".")
            .await
            .unwrap_or_default()
            .join("\n");
        messages.push(ChatMessage::system(format!(
            "Workspace: {}\nProject: {}\nTop-level:\n{}\n{}",
            self.workspace.root().display(),
            info.summary,
            listing,
            self.project_memory.prompt_block()
        )));
        for turn in self.session.recent(4) {
            messages.push(ChatMessage {
                role: turn.role.clone(),
                content: turn.content.clone(),
            });
        }
        messages.push(ChatMessage::user(prompt));
        let reply = self.llm.chat(&messages).await?;
        self.session.push("user", prompt);
        self.session.push("assistant", &reply);
        Ok(reply)
    }

    /// Walk key codebase files (deterministic), then synthesize a summary with the LLM.
    pub async fn summarize_codebase(
        &mut self,
        focus: Option<&str>,
        on_event: &mut impl FnMut(Ev),
    ) -> Result<RunResult> {
        on_event(Ev::Thinking { step: 1 });
        let mut read_count = 0u32;
        let digest = gather_codebase(self.workspace(), &mut |path| {
            read_count += 1;
            on_event(Ev::ToolStart {
                step: read_count,
                call: ToolCall {
                    name: "read_file".into(),
                    args: serde_json::json!({ "path": path }),
                },
            });
            on_event(Ev::ToolEnd {
                step: read_count,
                ok: true,
                output: format!("loaded {path}"),
            });
        })
        .await?;

        on_event(Ev::Thinking { step: read_count + 1 });

        let focus_line = focus
            .filter(|s| !s.trim().is_empty())
            .map(|s| format!("User focus: {s}\n"))
            .unwrap_or_default();

        let system = format!(
            r#"You are Vesper. You just walked this repository's important files.
Write a clear codebase summary like a senior engineer onboarding a teammate.

Structure:
1) What this project is (1 short paragraph)
2) Architecture / crates or packages (bullets)
3) Key entrypoints and what they do
4) How to run / verify (if evident)
5) Notable design choices or risks

Be concrete — name real files and symbols. No JSON. No fluff.
{focus_line}"#
        );

        let messages = vec![
            ChatMessage::system(system),
            ChatMessage::user(digest.prompt_block()),
        ];
        let reply = self.llm.chat(&messages).await?;
        let message = match parse_action(&reply) {
            AgentAction::Final { message } => message,
            AgentAction::Tool(_) => reply.trim().to_string(),
        };

        on_event(Ev::Final {
            message: message.clone(),
        });
        self.session.push(
            "user",
            focus.unwrap_or("summarize this codebase"),
        );
        self.session.push("assistant", &message);
        Ok(RunResult {
            message,
            steps: read_count,
            truncated: false,
        })
    }

    /// Single-shot answer using already-known project context (no tools).
    async fn fast_answer(
        &mut self,
        prompt: &str,
        on_event: &mut impl FnMut(Ev),
    ) -> Result<RunResult> {
        on_event(Ev::Thinking { step: 1 });
        let info = scan_project(self.workspace.root());
        let listing = self
            .workspace
            .list_dir(".")
            .await
            .unwrap_or_default()
            .join("\n");
        let readme = self
            .workspace
            .read_file("README.md")
            .await
            .ok()
            .map(|r| r.chars().take(600).collect::<String>())
            .unwrap_or_default();

        let system = format!(
            "You are Vesper. Answer briefly (4–8 sentences max) about this local project.\n\
             Workspace: {}\nProject: {}\nTop-level:\n{}\n{}\nREADME excerpt:\n{}\n\
             No tools. Plain text only — no JSON.",
            self.workspace.root().display(),
            info.summary,
            listing,
            self.project_memory.prompt_block(),
            readme
        );
        let messages = vec![
            ChatMessage::system(system),
            ChatMessage::user(prompt),
        ];
        let reply = self.llm.chat(&messages).await?;
        let message = match parse_action(&reply) {
            AgentAction::Final { message } => message,
            AgentAction::Tool(_) => reply.trim().to_string(),
        };
        on_event(Ev::Final {
            message: message.clone(),
        });
        self.session.push("user", prompt);
        self.session.push("assistant", &message);
        Ok(RunResult {
            message,
            steps: 0,
            truncated: false,
        })
    }

    pub async fn run(
        &mut self,
        prompt: &str,
        options: Opts,
        mut on_event: impl FnMut(Ev),
    ) -> Result<RunResult> {
        // Overview small-talk only — real "summarize the codebase" uses summarize_codebase().
        if is_fast_question(prompt) && !wants_codebase_summary(prompt) {
            return self.fast_answer(prompt, &mut on_event).await;
        }
        if wants_codebase_summary(prompt) {
            return self.summarize_codebase(Some(prompt), &mut on_event).await;
        }

        let info = scan_project(self.workspace.root());
        let listing = self
            .workspace
            .list_dir(".")
            .await
            .unwrap_or_default()
            .join("\n");

        let mut messages = vec![ChatMessage::system(agent_system(
            &self.workspace.root().display().to_string(),
            options.mode,
            &info.summary,
            &self.project_memory.prompt_block(),
            options.verify_command.as_deref(),
            &listing,
        ))];

        // Short README seed only — enough for orientation, not a full re-read later.
        if let Ok(readme) = self.workspace.read_file("README.md").await {
            let excerpt: String = readme.chars().take(700).collect();
            messages.push(ChatMessage::system(format!(
                "README.md (excerpt — already loaded, do not read_file again):\n{excerpt}"
            )));
        }

        for turn in self.session.recent(4) {
            messages.push(ChatMessage {
                role: turn.role.clone(),
                content: turn.content.clone(),
            });
        }

        messages.push(ChatMessage::user(prompt));
        self.session.push("user", prompt);

        let mut steps = 0u32;
        let mut tool_trace = Vec::new();
        let mut mutations = 0u32;
        let mut failed_calls: Vec<String> = Vec::new();
        let mut informative_ok: u32 = 0;

        loop {
            if steps >= options.max_steps {
                let msg = format!(
                    "Stopped after {} tool steps.\n{}",
                    options.max_steps,
                    tool_trace.join("\n")
                );
                on_event(Ev::Final {
                    message: msg.clone(),
                });
                self.session.push("assistant", &msg);
                return Ok(RunResult {
                    message: msg,
                    steps,
                    truncated: true,
                });
            }

            on_event(Ev::Thinking { step: steps + 1 });
            let raw = self.llm.chat(&messages).await?;
            messages.push(ChatMessage::assistant(&raw));

            match parse_action(&raw) {
                AgentAction::Final { message } => {
                    if mutations > 0 {
                        if let Some(cmd) = options.verify_command.clone() {
                            let out = self.workspace.run_shell(&cmd).await;
                            let (ok, text) = match out {
                                Ok(o) => (o.status == 0, o.combined()),
                                Err(e) => (false, format!("{e:#}")),
                            };
                            on_event(Ev::Verify {
                                command: cmd.clone(),
                                output: truncate_for_event(&text),
                                ok,
                            });
                            if !ok && steps + 1 < options.max_steps {
                                mutations = 0;
                                messages.push(ChatMessage::user(format!(
                                    "VERIFY_FAILED command=`{cmd}`\n{text}\nFix the failures, then finish."
                                )));
                                continue;
                            }
                        }
                    }
                    on_event(Ev::Final {
                        message: message.clone(),
                    });
                    self.session.push("assistant", &message);
                    return Ok(RunResult {
                        message,
                        steps,
                        truncated: false,
                    });
                }
                AgentAction::Tool(call) => {
                    steps += 1;
                    let fingerprint = tool_fingerprint(&call);

                    on_event(Ev::ToolStart {
                        step: steps,
                        call: call.clone(),
                    });

                    if failed_calls.iter().any(|f| f == &fingerprint) {
                        let denied = format!(
                            "Refusing repeated failing call `{fingerprint}`. \
                             Use a path from prior list_dir/find_files results, or emit action=final now."
                        );
                        on_event(Ev::ToolEnd {
                            step: steps,
                            ok: false,
                            output: denied.clone(),
                        });
                        messages.push(ChatMessage::user(format!(
                            "TOOL_RESULT name={} ok=false\n{denied}",
                            call.name
                        )));
                        continue;
                    }

                    if !call.allowed_in(options.mode) {
                        let denied = format!(
                            "Tool `{}` blocked in {} mode.",
                            call.name,
                            options.mode.as_str()
                        );
                        on_event(Ev::ToolEnd {
                            step: steps,
                            ok: false,
                            output: denied.clone(),
                        });
                        messages.push(ChatMessage::user(format!(
                            "TOOL_RESULT name={} ok=false\n{denied}",
                            call.name
                        )));
                        continue;
                    }

                    let preview = preview_tool(&self.workspace, &call)
                        .await
                        .unwrap_or_default();
                    if !preview.is_empty() && !call.is_readonly() {
                        on_event(Ev::DiffPreview {
                            path: call.arg_str("path").unwrap_or_default(),
                            diff: preview.clone(),
                        });
                    }

                    let allowed = if needs_approval(options.mode, &call) {
                        on_event(Ev::AwaitingApproval {
                            call: call.clone(),
                            preview: preview.clone(),
                        });
                        (options.approve)(&call, &preview)
                    } else {
                        true
                    };

                    if !allowed {
                        let denied = format!("Tool `{}` denied by user/policy.", call.name);
                        on_event(Ev::ToolEnd {
                            step: steps,
                            ok: false,
                            output: denied.clone(),
                        });
                        messages.push(ChatMessage::user(format!(
                            "TOOL_RESULT name={} ok=false\n{denied}",
                            call.name
                        )));
                        continue;
                    }

                    let mut ctx = ToolContext {
                        workspace: &self.workspace,
                        memory: &mut self.project_memory,
                        todos: &self.todos,
                    };
                    let result = execute_tool(&mut ctx, &call).await;
                    let (ok, output) = match result {
                        Ok(text) => (true, text),
                        Err(err) => (false, format!("error: {err:#}")),
                    };

                    if !ok {
                        failed_calls.push(fingerprint);
                    } else if matches!(
                        call.name.as_str(),
                        "list_dir" | "find_files" | "grep" | "read_file" | "git_status"
                    ) {
                        informative_ok += 1;
                    }

                    if call.name == "update_todos" {
                        on_event(Ev::Todos {
                            items: self.todos.snapshot(),
                        });
                    }

                    on_event(Ev::ToolEnd {
                        step: steps,
                        ok,
                        output: truncate_for_event(&output),
                    });
                    tool_trace.push(format!(
                        "{} {} → {}",
                        if ok { "ok" } else { "err" },
                        call.name,
                        summarize(&output)
                    ));

                    if ok && is_mutating(&call.name) {
                        mutations += 1;
                    }

                    let mut result_msg = format!(
                        "TOOL_RESULT name={} ok={ok}\n{output}",
                        call.name
                    );
                    if informative_ok >= 2 && call.is_readonly() {
                        result_msg.push_str(
                            "\n\nHINT: You have enough context for many questions. \
                             Prefer {\"action\":\"final\",\"message\":\"...\"} now unless you need one specific file.",
                        );
                    }
                    messages.push(ChatMessage::user(result_msg));

                    if ok
                        && matches!(
                            call.name.as_str(),
                            "write_file" | "str_replace" | "multi_str_replace" | "delete_file"
                        )
                    {
                        if let Some(cmd) = options.verify_command.clone() {
                            let out = self.workspace.run_shell(&cmd).await;
                            let (vok, text) = match out {
                                Ok(o) => (o.status == 0, o.combined()),
                                Err(e) => (false, format!("{e:#}")),
                            };
                            on_event(Ev::Verify {
                                command: cmd.clone(),
                                output: truncate_for_event(&text),
                                ok: vok,
                            });
                            messages.push(ChatMessage::user(format!(
                                "AUTO_VERIFY command=`{cmd}` ok={vok}\n{text}"
                            )));
                            if vok {
                                mutations = 0;
                            }
                        }
                    }
                }
            }
        }
    }

    pub async fn status_context(&self) -> Result<String> {
        let info = scan_project(self.workspace.root());
        let status = self.workspace.git_status().await.unwrap_or_default();
        let listing = self.workspace.list_dir(".").await.unwrap_or_default();
        Ok(format!(
            "project: {}\nmode: {}\nverify: {}\n\ngit status:\n{status}\n\nentries:\n{}\n\nmemory:\n{}",
            info.summary,
            self.mode.as_str(),
            self.verify_command.as_deref().unwrap_or("(none)"),
            listing.join("\n"),
            self.project_memory.prompt_block()
        ))
    }
}

fn truncate_for_event(s: &str) -> String {
    const N: usize = 2_500;
    if s.len() <= N {
        s.to_string()
    } else {
        format!("{}… [{} bytes total]", &s[..N], s.len())
    }
}

fn summarize(s: &str) -> String {
    let line = s.lines().next().unwrap_or("").trim();
    if line.len() > 80 {
        format!("{}…", &line[..80])
    } else if line.is_empty() {
        "(empty)".into()
    } else {
        line.to_string()
    }
}

fn tool_fingerprint(call: &ToolCall) -> String {
    format!("{}:{}", call.name, call.args)
}

fn is_fast_question(prompt: &str) -> bool {
    let p = prompt.to_lowercase();
    let p = p.trim();
    const KEYS: &[&str] = &["who are you", "what are you", "hello", "hi vesper"];
    KEYS.iter().any(|k| p.contains(k))
}

fn wants_codebase_summary(prompt: &str) -> bool {
    let p = prompt.to_lowercase();
    const KEYS: &[&str] = &[
        "summarize",
        "summary",
        "overview",
        "codebase",
        "walk the",
        "go through",
        "explain the repo",
        "explain this repo",
        "explain the project",
        "explain this project",
        "what does this project",
        "what is this",
        "what's this",
        "whats this",
        "whats happening",
        "what's happening",
        "what is happening",
        "whats going on",
        "what's going on",
        "architecture",
        "how is this structured",
        "map the repo",
        "tour the code",
        "onboard",
        "describe this",
    ];
    KEYS.iter().any(|k| p.contains(k))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_final() {
        let a = parse_action(r#"{"action":"final","message":"done"}"#);
        match a {
            AgentAction::Final { message } => assert_eq!(message, "done"),
            _ => panic!(),
        }
    }
}
