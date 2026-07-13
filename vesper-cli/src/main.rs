mod repl;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::io::{self, Write};
use std::path::PathBuf;
use vesper_agent::{Agent, AgentEvent, AgentOptions, SessionMode, ToolCall};
use vesper_config::{set_key, Config};
use vesper_llm::{ChatOptions, LlmClient, OllamaClient};
use vesper_tools::{list_backups, restore_backup, scan_project, Workspace};

#[derive(Parser, Debug)]
#[command(
    name = "vesper",
    about = "Vesper — local coding agent that stays on your machine",
    version,
    propagate_version = true
)]
struct Cli {
    #[arg(long, global = true, env = "VESPER_WORKSPACE")]
    workspace: Option<PathBuf>,

    #[arg(long, global = true, env = "VESPER_OLLAMA_URL")]
    ollama_url: Option<String>,

    #[arg(long, global = true, env = "VESPER_MODEL")]
    model: Option<String>,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Interactive agent REPL (default if no subcommand)
    Chat,
    /// Chat without tools
    Ask {
        prompt: Vec<String>,
    },
    /// One-shot tool-calling agent
    Run {
        prompt: Vec<String>,
        #[arg(long, short = 'y')]
        yes: bool,
        #[arg(long)]
        read_only: bool,
        #[arg(long)]
        max_steps: Option<u32>,
        #[arg(long, short = 'q')]
        quiet: bool,
    },
    /// Diagnose and fix build/test failures
    Fix {
        hint: Vec<String>,
        #[arg(long, short = 'y')]
        yes: bool,
        #[arg(long)]
        max_steps: Option<u32>,
    },
    /// Walk key source files and summarize the codebase
    Summarize {
        /// Optional focus (e.g. "agent loop", "CLI")
        focus: Vec<String>,
    },
    /// Project summary
    Analyze,
    /// List Ollama models
    Models,
    /// Create .vesper/ project config
    Init,
    /// Show or set config
    Config {
        #[command(subcommand)]
        action: ConfigCmd,
    },
    /// Project memory facts
    Memory {
        #[command(subcommand)]
        action: MemoryCmd,
    },
    /// File backups from edits
    Restore {
        #[command(subcommand)]
        action: RestoreCmd,
    },
    /// Workspace snapshot
    Context,
    /// Health check
    Doctor,
}

#[derive(Subcommand, Debug)]
enum ConfigCmd {
    Show,
    Set {
        key: String,
        value: String,
        #[arg(long)]
        project: bool,
    },
}

#[derive(Subcommand, Debug)]
enum MemoryCmd {
    List,
    Add { fact: Vec<String> },
    Forget { index: usize },
}

#[derive(Subcommand, Debug)]
enum RestoreCmd {
    List,
    Apply { id: usize },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_target(false)
        .init();

    let cli = Cli::parse();
    let root = cli
        .workspace
        .unwrap_or(std::env::current_dir().context("current_dir")?);
    let workspace = Workspace::new(&root)?;
    let mut cfg = Config::load_layered(workspace.root())?;
    if let Some(m) = cli.model {
        cfg.model = m;
    }
    if let Some(u) = cli.ollama_url {
        cfg.ollama_host = u;
    }

    let llm = OllamaClient::new(&cfg.ollama_host, &cfg.model).with_options(ChatOptions {
        temperature: cfg.temperature,
        num_ctx: cfg.num_ctx,
        num_predict: cfg.num_predict,
        keep_alive: cfg.keep_alive.clone(),
    });
    let mut agent = Agent::new(llm, workspace);
    agent.set_mode(cfg.mode);
    agent.verify_command = cfg.verify_command.clone();

    match cli.command.unwrap_or(Commands::Chat) {
        Commands::Chat => {
            repl::run_repl(&mut agent, &cfg).await?;
        }
        Commands::Ask { prompt } => {
            let prompt = prompt.join(" ");
            if prompt.trim().is_empty() {
                anyhow::bail!("usage: vesper ask \"question\"");
            }
            println!("{}", agent.ask(&prompt).await?);
        }
        Commands::Run {
            prompt,
            yes,
            read_only,
            max_steps,
            quiet,
        } => {
            let prompt = prompt.join(" ");
            if prompt.trim().is_empty() {
                anyhow::bail!("usage: vesper run \"task\"");
            }
            let mode = if read_only {
                SessionMode::Plan
            } else if yes {
                SessionMode::Auto
            } else {
                cfg.mode
            };
            let options = build_options(mode, max_steps.unwrap_or(cfg.max_steps), &cfg, yes);
            let result = agent
                .run(&prompt, options, |ev| {
                    if !quiet {
                        print_event(&ev);
                    }
                })
                .await?;
            if quiet {
                println!("{}", result.message);
            } else {
                eprintln!("\n[vesper] done in {} steps", result.steps);
            }
        }
        Commands::Fix {
            hint,
            yes,
            max_steps,
        } => {
            let hint = hint.join(" ");
            let prompt = if hint.trim().is_empty() {
                "Investigate this workspace. Run the best build/test command, diagnose failures, \
                 fix with minimal diffs, re-verify until green or explain blockers."
                    .into()
            } else {
                format!("Fix this problem using tools and verify:\n\n{hint}")
            };
            let mode = if yes { SessionMode::Auto } else { cfg.mode };
            let options = build_options(mode, max_steps.unwrap_or(cfg.max_steps), &cfg, yes);
            let result = agent
                .run(&prompt, options, |ev| print_event(&ev))
                .await?;
            eprintln!("\n[vesper] fix finished ({} steps)", result.steps);
            let _ = result;
        }
        Commands::Summarize { focus } => {
            let focus = focus.join(" ");
            let focus = if focus.trim().is_empty() {
                None
            } else {
                Some(focus.as_str())
            };
            let result = agent
                .summarize_codebase(focus, &mut |ev| print_event(&ev))
                .await?;
            eprintln!(
                "\n[vesper] summarized {} files",
                result.steps
            );
        }
        Commands::Analyze => {
            let info = scan_project(agent.workspace().root());
            println!("project_type : {}", info.project_type);
            println!("languages    : {}", info.languages.join(", "));
            println!("key_files    : {}", info.key_files.join(", "));
            println!(
                "verify       : {}",
                info.suggested_verify.unwrap_or_else(|| "(none)".into())
            );
            println!("summary      : {}", info.summary);
        }
        Commands::Models => {
            let client = OllamaClient::new(&cfg.ollama_host, &cfg.model);
            match client.list_models().await {
                Ok(models) => {
                    if models.is_empty() {
                        println!("(no models — run: ollama pull qwen2.5-coder:7b)");
                    } else {
                        for m in models {
                            let mark = if m == cfg.model { "*" } else { " " };
                            println!("{mark} {m}");
                        }
                    }
                }
                Err(e) => {
                    eprintln!("failed to list models: {e}");
                    std::process::exit(1);
                }
            }
        }
        Commands::Init => {
            let mut overlay = cfg.clone();
            if overlay.verify_command.is_none() {
                overlay.verify_command = Config::suggest_verify(agent.workspace().root());
            }
            overlay.save_project(agent.workspace().root())?;
            // also ensure memory file exists
            agent.project_memory().save(agent.workspace().root())?;
            println!(
                "initialized {}",
                Config::project_path(agent.workspace().root()).display()
            );
            if let Some(v) = &overlay.verify_command {
                println!("verify_command = {v}");
            }
        }
        Commands::Config { action } => match action {
            ConfigCmd::Show => {
                println!("{}", serde_json::to_string_pretty(&cfg)?);
            }
            ConfigCmd::Set {
                key,
                value,
                project,
            } => {
                set_key(agent.workspace().root(), &key, &value, project)?;
                println!("set {key} = {value} ({})", if project { "project" } else { "global" });
            }
        },
        Commands::Memory { action } => match action {
            MemoryCmd::List => {
                let facts = &agent.project_memory().facts;
                if facts.is_empty() {
                    println!("(no facts)");
                } else {
                    for (i, f) in facts.iter().enumerate() {
                        println!("{i}: {f}");
                    }
                }
            }
            MemoryCmd::Add { fact } => {
                let fact = fact.join(" ");
                agent.project_memory_mut().add(&fact);
                agent.project_memory().save(agent.workspace().root())?;
                println!("remembered: {fact}");
            }
            MemoryCmd::Forget { index } => {
                let removed = agent.project_memory_mut().forget(index)?;
                agent.project_memory().save(agent.workspace().root())?;
                println!("forgot: {removed}");
            }
        },
        Commands::Restore { action } => match action {
            RestoreCmd::List => {
                let entries = list_backups(agent.workspace().root())?;
                if entries.is_empty() {
                    println!("(no backups)");
                } else {
                    for e in entries {
                        println!("#{}  {}  (ts={})", e.id, e.original, e.timestamp);
                    }
                }
            }
            RestoreCmd::Apply { id } => {
                println!("{}", restore_backup(agent.workspace().root(), id)?);
            }
        },
        Commands::Context => {
            println!("{}", agent.status_context().await?);
        }
        Commands::Doctor => {
            println!("Vesper doctor");
            println!("  workspace : {}", agent.workspace().root().display());
            println!("  ollama    : {}", cfg.ollama_host);
            println!("  model     : {}", cfg.model);
            println!("  mode      : {}", cfg.mode.as_str());
            println!(
                "  verify    : {}",
                cfg.verify_command.as_deref().unwrap_or("(none)")
            );
            let client = OllamaClient::new(&cfg.ollama_host, &cfg.model);
            match client
                .chat(&[vesper_llm::ChatMessage::user(
                    r#"Reply with exactly: {"action":"final","message":"ok"}"#,
                )])
                .await
            {
                Ok(reply) => println!(
                    "  llm       : ok ({})",
                    reply.trim().chars().take(60).collect::<String>()
                ),
                Err(err) => {
                    println!("  llm       : FAILED");
                    println!("  error     : {err}");
                    println!("\nInstall Ollama, then: ollama pull {}", cfg.model);
                    std::process::exit(1);
                }
            }
        }
    }

    Ok(())
}

fn build_options(
    mode: SessionMode,
    max_steps: u32,
    cfg: &Config,
    yes: bool,
) -> AgentOptions {
    let verify = cfg.verify_command.clone();
    if yes || mode == SessionMode::Auto {
        return AgentOptions::for_mode(
            if mode == SessionMode::Plan {
                SessionMode::Plan
            } else {
                SessionMode::Auto
            },
            max_steps,
            verify,
            Box::new(|call, _| {
                // still block catastrophic in autopilot path via is_destructive prompt?
                // Auto mode: approve non-destructive automatically; destructive still needs
                // interactive — but in -y we approve all except we could refuse catastrophic
                let _ = call;
                true
            }),
        );
    }
    if mode == SessionMode::Plan {
        return AgentOptions::for_mode(
            SessionMode::Plan,
            max_steps,
            verify,
            Box::new(|_, _| false),
        );
    }
    AgentOptions::for_mode(
        SessionMode::Ask,
        max_steps,
        verify,
        Box::new(|call, preview| prompt_approval(call, preview)),
    )
}

pub(crate) fn prompt_approval(call: &ToolCall, preview: &str) -> bool {
    if !preview.is_empty() {
        eprintln!("\n──── preview ────\n{preview}\n────────────────");
    }
    eprint!(
        "[vesper] approve `{}`? [y/N] ",
        call.name
    );
    let _ = io::stderr().flush();
    let mut line = String::new();
    if io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim().to_lowercase().as_str(), "y" | "yes")
}

pub(crate) fn print_event(ev: &AgentEvent) {
    match ev {
        AgentEvent::Thinking { step } => eprintln!("[vesper] thinking (step {step})…"),
        AgentEvent::ToolStart { step, call } => {
            eprintln!(
                "[vesper] ▶ #{step} {} {}",
                call.name,
                short_args(call)
            );
        }
        AgentEvent::DiffPreview { diff, .. } => {
            if !diff.is_empty() {
                eprintln!("{diff}");
            }
        }
        AgentEvent::AwaitingApproval { .. } => {}
        AgentEvent::ToolEnd { step, ok, output } => {
            let flag = if *ok { "ok" } else { "ERR" };
            let preview = output.lines().take(10).collect::<Vec<_>>().join("\n");
            eprintln!("[vesper] ■ #{step} {flag}\n{preview}");
        }
        AgentEvent::Todos { items } => {
            eprintln!("[vesper] todos:");
            for (i, t) in items.iter().enumerate() {
                eprintln!("  {}. {t}", i + 1);
            }
        }
        AgentEvent::Verify {
            command,
            output,
            ok,
        } => {
            let flag = if *ok { "PASS" } else { "FAIL" };
            eprintln!("[vesper] verify `{command}` → {flag}");
            let preview = output.lines().take(12).collect::<Vec<_>>().join("\n");
            if !preview.is_empty() {
                eprintln!("{preview}");
            }
        }
        AgentEvent::Final { message } => {
            println!("\n{message}");
        }
    }
}

fn short_args(call: &ToolCall) -> String {
    match call.name.as_str() {
        "read_file" | "write_file" | "str_replace" | "multi_str_replace" | "delete_file"
        | "list_dir" => call.arg_str("path").unwrap_or_default(),
        "grep" | "find_files" => call
            .arg_str("pattern")
            .or_else(|| call.arg_str("query"))
            .unwrap_or_default(),
        "run_shell" => call
            .arg_str("command")
            .or_else(|| call.arg_str("cmd"))
            .unwrap_or_default(),
        "git_commit" => call.arg_str("message").unwrap_or_default(),
        "remember" => call.arg_str("fact").unwrap_or_default(),
        _ => String::new(),
    }
}
