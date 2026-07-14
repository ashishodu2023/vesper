use anyhow::{bail, Result};
use std::time::Duration;
use vesper_agent::{Agent, SessionMode};
use vesper_config::Config;
use vesper_llm::LlmClient;

use crate::{build_options, print_event};

pub async fn run_watch<C: LlmClient + Clone>(
    agent: &mut Agent<C>,
    cfg: &Config,
    interval: u64,
    yes: bool,
    max_steps: Option<u32>,
    once: bool,
) -> Result<()> {
    let verify = cfg
        .verify_command
        .clone()
        .or_else(|| {
            vesper_tools::scan_project(agent.workspace().root())
                .suggested_verify
        })
        .ok_or_else(|| {
            anyhow::anyhow!("no verify_command — set with: vesper config set verify_command \"…\" --project")
        })?;

    eprintln!("[vesper] watch: `{verify}` every {interval}s (Ctrl-C to stop)");
    let mut last_fp = String::new();

    loop {
        eprintln!("[vesper] watch: running verify…");
        let out = agent.workspace().run_shell(&verify).await?;
        if out.status == 0 {
            eprintln!("[vesper] watch: PASS");
            last_fp.clear();
            if once {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_secs(interval.max(1))).await;
            continue;
        }

        let combined = out.combined();
        let fp = fingerprint(&combined);
        if fp == last_fp {
            eprintln!("[vesper] watch: still failing (same fingerprint) — waiting");
            tokio::time::sleep(Duration::from_secs(interval.max(1))).await;
            continue;
        }
        last_fp = fp;
        eprintln!("[vesper] watch: FAIL — entering fix loop");
        let excerpt: String = combined.chars().take(4000).collect();
        let prompt = format!(
            "Verify command `{verify}` failed. Diagnose and fix with minimal diffs, then re-verify.\n\n{excerpt}"
        );
        let mode = if yes {
            SessionMode::Auto
        } else {
            cfg.mode
        };
        let options = build_options(mode, max_steps.unwrap_or(cfg.max_steps), cfg, yes);
        let result = agent.run(&prompt, options, |ev| print_event(&ev)).await?;
        eprintln!(
            "[vesper] watch: fix finished ({} steps) — {}",
            result.steps,
            if result.truncated { "truncated" } else { "ok" }
        );

        if once {
            // One more verify to report final state.
            let again = agent.workspace().run_shell(&verify).await?;
            if again.status == 0 {
                eprintln!("[vesper] watch: green after fix");
                return Ok(());
            }
            bail!("still failing after one fix attempt");
        }
        tokio::time::sleep(Duration::from_secs(interval.max(1))).await;
    }
}

fn fingerprint(s: &str) -> String {
    let lines: Vec<&str> = s
        .lines()
        .filter(|l| {
            let t = l.trim();
            !t.is_empty() && !t.starts_with("warning:")
        })
        .take(20)
        .collect();
    lines.join("\n")
}
