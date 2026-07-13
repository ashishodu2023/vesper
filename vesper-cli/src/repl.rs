use anyhow::Result;
use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;
use vesper_agent::{Agent, AgentOptions, SessionMode};
use vesper_config::Config;
use vesper_llm::LlmClient;

use crate::{print_event, prompt_approval};

pub async fn run_repl<C: LlmClient>(agent: &mut Agent<C>, cfg: &Config) -> Result<()> {
    print_banner(agent, cfg);

    let mut rl = DefaultEditor::new()?;
    let history = history_path();
    if let Some(parent) = history.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = rl.load_history(&history);

    loop {
        let prompt = format!("vesper ({}) › ", agent.mode.as_str());
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

fn needs_prompt(mode: SessionMode, call: &vesper_agent::ToolCall) -> bool {
    if call.is_readonly() {
        return false;
    }
    match mode {
        SessionMode::Plan | SessionMode::Ask => true,
        SessionMode::Auto => call.is_destructive(),
    }
}

fn print_banner<C: LlmClient>(agent: &Agent<C>, cfg: &Config) {
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
    println!("\nType a task, or /help. Ctrl-D to exit.\n");
}

async fn handle_slash<C: LlmClient>(
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
  /summarize [focus]    walk key files and summarize codebase
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
        "/summarize" => {
            let focus = if rest.is_empty() { None } else { Some(rest) };
            match agent
                .summarize_codebase(focus, &mut |ev| print_event(&ev))
                .await
            {
                Ok(r) => eprintln!("[vesper] summarized {} files", r.steps),
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

fn history_path() -> std::path::PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".vesper")
        .join("history")
}
