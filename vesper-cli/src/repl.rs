use anyhow::{Context, Result};
use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;
use std::path::PathBuf;
use vesper_agent::{Agent, AgentOptions, SessionMode};
use vesper_config::{Config, Config as VesperConfig};
use vesper_llm::LlmClient;
use vesper_tools::{list_checkpoints, undo_last, Workspace};

use crate::{print_event, prompt_approval};

pub async fn run_repl<C: LlmClient + Clone>(agent: &mut Agent<C>, cfg: &Config) -> Result<()> {
    print_banner(agent, cfg);

    let mut rl = DefaultEditor::new()?;
    let history = history_path();
    if let Some(parent) = history.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = rl.load_history(&history);

    loop {
        let short_ws = short_workspace(agent.workspace().root());
        let prompt = format!("vesper ({}/{}) › ", agent.mode.as_str(), short_ws);
        let line = match rl.readline(&prompt) {
            Ok(l) => l,
            Err(ReadlineError::Interrupted) => continue,
            Err(ReadlineError::Eof) => break,
            Err(err) => return Err(err.into()),
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let _ = rl.add_history_entry(line);

        // Catch shell commands pasted into the REPL
        if looks_like_shell_command(line) {
            eprintln!(
                "[vesper] that looks like a shell command.\n\
                 Inside this chat, use:\n\
                   /workspace /path/to/repo\n\
                   /summarize\n\
                 Or exit (/exit) and run in your terminal:\n\
                   {line}"
            );
            continue;
        }

        if line.starts_with('/') {
            if handle_slash(agent, cfg, line).await? {
                break;
            }
            continue;
        }

        let mode = agent.mode;
        let verify = agent
            .verify_command
            .clone()
            .or_else(|| cfg.verify_command.clone());
        let max_steps = cfg.max_steps;
        let options = AgentOptions::for_mode(
            mode,
            max_steps,
            verify,
            Box::new(move |call, preview| {
                if needs_prompt(mode, call) {
                    prompt_approval(call, preview)
                } else {
                    true
                }
            }),
        );

        match agent.run(line, options, |ev| print_event(&ev)).await {
            Ok(r) => {
                if r.truncated {
                    eprintln!("[vesper] hit max steps ({})", r.steps);
                }
            }
            Err(e) => eprintln!("[vesper] error: {e:#}"),
        }
    }

    let _ = rl.save_history(&history);
    println!("bye.");
    Ok(())
}

fn looks_like_shell_command(line: &str) -> bool {
    let t = line.trim();
    t.starts_with("vesper ")
        || t.starts_with("vesper\t")
        || t.starts_with("cargo ")
        || t.starts_with("cd ")
        || t.starts_with("ollama ")
}

fn short_workspace(path: &std::path::Path) -> String {
    path.file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(".")
        .to_string()
}

fn needs_prompt(mode: SessionMode, call: &vesper_agent::ToolCall) -> bool {
    if call.is_readonly() {
        return false;
    }
    match mode {
        SessionMode::Plan | SessionMode::Ask => true,
        SessionMode::Auto => call.is_destructive(),
    }
}

fn print_banner<C: LlmClient + Clone>(agent: &Agent<C>, cfg: &Config) {
    println!(
        r#"
 ██╗   ██╗███████╗███████╗██████╗ ███████╗██████╗
 ██║   ██║██╔════╝██╔════╝██╔══██╗██╔════╝██╔══██╗
 ██║   ██║█████╗  ███████╗██████╔╝█████╗  ██████╔╝
 ╚██╗ ██╔╝██╔══╝  ╚════██║██╔═══╝ ██╔══╝  ██╔══██╗
  ╚████╔╝ ███████╗███████║██║     ███████╗██║  ██║
   ╚═══╝  ╚══════╝╚══════╝╚═╝     ╚══════╝╚═╝  ╚═╝
"#
    );
    println!("  model     {}", cfg.model);
    println!("  workspace {}", agent.workspace().root().display());
    println!(
        "  mode      {}  ( /mode plan|ask|auto )",
        agent.mode.as_str()
    );
    println!(
        "  verify    {}",
        agent
            .verify_command
            .as_deref()
            .or(cfg.verify_command.as_deref())
            .unwrap_or("(none — run: vesper init)")
    );
    println!("\nType a task, or /help. Use /workspace <path> to switch repos.\n");
}

async fn handle_slash<C: LlmClient + Clone>(
    agent: &mut Agent<C>,
    cfg: &Config,
    line: &str,
) -> Result<bool> {
    let mut parts = line.splitn(2, char::is_whitespace);
    let cmd = parts.next().unwrap_or("");
    let rest = parts.next().unwrap_or("").trim();

    match cmd {
        "/help" | "/h" => {
            println!(
                r#"Commands:
  /help                 this help
  /workspace [path]     show or switch project root
  /summarize [focus]    walk key files and summarize codebase
  /undo                 restore last edit checkpoint
  /checkpoints          list edit checkpoints
  /mode [plan|ask|auto] show or set session mode
  /new                  clear conversation
  /remember <fact>      persist project fact
  /memory               list facts
  /forget <n>           drop fact
  /context              workspace snapshot
  /exit                 quit
"#
            );
        }
        "/undo" => match undo_last(agent.workspace().root()) {
            Ok(msg) => println!("{msg}"),
            Err(e) => println!("{e}"),
        },
        "/checkpoints" => {
            match list_checkpoints(agent.workspace().root()) {
                Ok(items) if items.is_empty() => println!("(no checkpoints)"),
                Ok(items) => {
                    for c in items {
                        println!("#{}  {}  ({} files)", c.id, c.label, c.files.len());
                    }
                }
                Err(e) => println!("{e}"),
            }
        }
        "/workspace" | "/ws" => {
            if rest.is_empty() {
                println!("workspace = {}", agent.workspace().root().display());
            } else {
                let path = expand_path(rest);
                let ws = Workspace::new(&path)
                    .with_context(|| format!("invalid workspace: {}", path.display()))?;
                // Reload project verify hint if present
                let layered = VesperConfig::load_layered(ws.root()).unwrap_or_else(|_| cfg.clone());
                agent.verify_command = layered.verify_command.clone();
                agent.set_mode(layered.mode);
                let root = ws.root().display().to_string();
                agent.set_workspace(ws);
                println!("workspace → {root}");
                println!("(session cleared; mode={})", agent.mode.as_str());
            }
        }
        "/summarize" => {
            let focus = if rest.is_empty() { None } else { Some(rest) };
            match agent
                .summarize_codebase(focus, &mut |ev| print_event(&ev))
                .await
            {
                Ok(r) => eprintln!(
                    "[vesper] summarized {} files under {}",
                    r.steps,
                    agent.workspace().root().display()
                ),
                Err(e) => eprintln!("[vesper] error: {e:#}"),
            }
        }
        "/mode" => {
            if rest.is_empty() {
                println!("mode = {} (plan|ask|auto)", agent.mode.as_str());
            } else if let Some(m) = SessionMode::parse(rest) {
                agent.set_mode(m);
                println!("mode → {}", m.as_str());
            } else {
                println!("unknown mode: {rest}");
            }
        }
        "/new" => {
            agent.clear_session();
            println!("conversation cleared.");
        }
        "/remember" => {
            if rest.is_empty() {
                println!("usage: /remember <fact>");
            } else {
                agent.project_memory_mut().add(rest);
                agent.project_memory().save(agent.workspace().root())?;
                println!("remembered.");
            }
        }
        "/memory" => {
            let facts = &agent.project_memory().facts;
            if facts.is_empty() {
                println!("(no facts)");
            } else {
                for (i, f) in facts.iter().enumerate() {
                    println!("{i}: {f}");
                }
            }
        }
        "/forget" => {
            let idx: usize = rest.parse().unwrap_or(usize::MAX);
            match agent.project_memory_mut().forget(idx) {
                Ok(f) => {
                    agent.project_memory().save(agent.workspace().root())?;
                    println!("forgot: {f}");
                }
                Err(e) => println!("{e}"),
            }
        }
        "/context" => {
            println!("{}", agent.status_context().await?);
        }
        "/model" => {
            println!(
                "model = {} — change with: vesper --model <name>  or  vesper config set model <name>",
                cfg.model
            );
        }
        "/exit" | "/quit" | "/q" => return Ok(true),
        other => println!("unknown command: {other} (try /help)"),
    }
    Ok(false)
}

fn expand_path(raw: &str) -> PathBuf {
    let raw = raw.trim().trim_matches('"').trim_matches('\'');
    if let Some(rest) = raw.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    if raw == "~" {
        if let Some(home) = dirs::home_dir() {
            return home;
        }
    }
    PathBuf::from(raw)
}

fn history_path() -> std::path::PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".vesper")
        .join("history")
}
