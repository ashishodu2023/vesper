# VESPER

```
 ██╗   ██╗███████╗███████╗██████╗ ███████╗██████╗
 ██║   ██║██╔════╝██╔════╝██╔══██╗██╔════╝██╔══██╗
 ██║   ██║█████╗  ███████╗██████╔╝█████╗  ██████╔╝
 ╚██╗ ██╔╝██╔══╝  ╚════██║██╔═══╝ ██╔══╝  ██╔══██╗
  ╚████╔╝ ███████╗███████║██║     ███████╗██║  ██║
   ╚═══╝  ╚══════╝╚══════╝╚═╝     ╚══════╝╚═╝  ╚═╝
```

**VESPER** — **V**erified **E**diting **S**ystem for **P**rivate **E**ngineering **R**epos

A local-first coding agent for your terminal — built in Rust.

It reads your repo, walks the codebase, edits with diffs + backups, runs shell/git, and **auto-verifies** its own changes. Everything runs on your machine through [Ollama](https://ollama.com). No API keys. No cloud. No subscriptions. Your code never leaves the machine.

```bash
vesper summarize              # walk key files and summarize the codebase
vesper run "fix the failing test" -y
vesper watch -y               # poll verify_command and auto-fix
vesper                        # interactive REPL
```

## Why VESPER

| | **VESPER** | Typical cloud agents |
|--|--|--|
| Privacy | Code never leaves your machine | Sent to hosted APIs |
| Cost | Free (your GPU/CPU) | Metered tokens |
| Runtime | Single Rust binary | Heavy Node/Python stacks |
| Verify | Forced auto-verify after edits | Hope the model remembers |
| Safety | Sandboxed paths, diffs, backups, plan/ask/auto modes | Varies |

## Features

- **Codebase walk + summarize** — deterministic file gathering, then LLM synthesis
- **Streaming replies** — live tokens for summarize / ask / fast answers
- **Native Ollama tools** — structured tool calls + JSON fallback
- **Repo map** — ranked file context pack
- **Edit retry** — soft-match on failed `str_replace`
- **Parallel subagents** — `spawn_subagents` runs plan-mode explorers in parallel
- **MCP plugins** — stdio MCP servers from config (`mcp_<server>_<tool>`)
- **Watch mode** — poll `verify_command` and enter fix loop on failure
- **Session modes** — `plan` / `ask` / `auto`
- **Diffs + backups + checkpoints** — `/undo`, `vesper checkpoint`
- **Remote Ollama** — `VESPER_OLLAMA_URL`; tools stay local
- **VS Code / Cursor scaffold** — `extensions/vesper-vscode`

## Install

```bash
curl https://sh.rustup.rs -sSf | sh
# https://ollama.com then:
ollama pull qwen2.5-coder:14b

git clone https://github.com/ashishodu2023/vesper.git
cd vesper
cargo install --path vesper-cli

vesper doctor
vesper init
```

Remote GPU:

```bash
export VESPER_OLLAMA_URL=http://192.168.x.x:11434
vesper doctor
```

### MCP plugins

In `~/.vesper/config.json` or `.vesper/config.json`:

```json
{
  "mcp_servers": [
    {
      "name": "fs",
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "/absolute/path"],
      "enabled": true
    }
  ]
}
```

```bash
vesper mcp list
```

## Usage

| Command | What it does |
|---------|----------------|
| `vesper` | Interactive REPL |
| `vesper summarize` | Walk + summarize |
| `vesper run "…"` | One-shot tool agent |
| `vesper fix -y` | Diagnose/fix failures |
| `vesper watch -y` | Poll verify → auto-fix |
| `vesper ask "…"` | Chat only (streams) |
| `vesper mcp list` | Show MCP tools |
| `vesper checkpoint …` | Edit checkpoints |
| `vesper doctor` | Health check |

### REPL highlights

`/workspace` `/summarize` `/undo` `/checkpoints` `/mode` `/help`

### Session modes

| Mode | Behavior |
|------|----------|
| **ask** | Mutating tools ask y/n |
| **auto** | Routine edits fly; destructive / MCP still ask |
| **plan** | Research only |

## Architecture

```
vesper/
├── vesper-cli       # binary + REPL + watch
├── vesper-agent     # tool loop, subagents, summarize
├── vesper-tools     # FS / shell / git / repo map / checkpoints
├── vesper-llm       # Ollama (stream + native tools)
├── vesper-mcp       # MCP stdio host
├── vesper-memory
├── vesper-config
└── extensions/vesper-vscode
```

## Requirements

- Rust 1.75+
- [Ollama](https://ollama.com) coding model (`7b` laptop / `14b+` GPU)

## Honest ceiling

Local open models still lag Claude/Codex on hard multi-file refactors. VESPER wins on **privacy**, **verify-by-default**, and **one binary**.

## License

MIT
