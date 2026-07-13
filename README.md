# VESPER

```
 ██╗   ██╗███████╗███████╗██████╗ ███████╗██████╗
 ██║   ██║██╔════╝██╔════╝██╔══██╗██╔════╝██╔══██╗
 ██║   ██║█████╗  ███████╗██████╔╝█████╗  ██████╔╝
 ╚██╗ ██╔╝██╔══╝  ╚════██║██╔═══╝ ██╔══╝  ██╔══██╗
  ╚████╔╝ ███████╗███████║██║     ███████╗██║  ██║
   ╚═══╝  ╚══════╝╚══════╝╚═╝     ╚══════╝╚═╝  ╚═╝
```

**VESPER** is a local-first coding agent for your terminal — built in Rust.

It reads your repo, walks the codebase, edits with diffs + backups, runs shell/git, and **auto-verifies** its own changes. Everything runs on your machine through [Ollama](https://ollama.com). No API keys. No cloud. No subscriptions.

```bash
vesper summarize              # walk key files and summarize the codebase
vesper run "fix the failing test" -y
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

- **Codebase walk + summarize** — deterministic file gathering, then LLM synthesis (`vesper summarize`)
- **Tool-calling agent** — read, grep, find, edit, multi-edit, shell, git, todos, memory
- **Session modes** — `plan` (research only) / `ask` (confirm edits) / `auto` (routine edits fly)
- **Diffs + backups** — mutating tools preview changes; copies land in `.vesper/backups/`
- **Auto-verify** — set `verify_command` (e.g. `cargo test`) and VESPER re-checks after edits
- **Project memory** — persistent facts in `.vesper/memory.json`
- **Interactive REPL** — `vesper` with `/summarize`, `/mode`, `/remember`, …

## Install

```bash
# Prerequisites: Rust + Ollama
curl https://sh.rustup.rs -sSf | sh
# install Ollama from https://ollama.com then:
ollama pull qwen2.5-coder:14b   # or 7b on smaller machines

git clone https://github.com/ashishodu2023/vesper.git
cd vesper
cargo install --path vesper-cli

vesper doctor
vesper init
```

## Usage

| Command | What it does |
|---------|----------------|
| `vesper` | Interactive VESPER REPL |
| `vesper summarize` | Walk key source files and summarize |
| `vesper run "…"` | One-shot tool agent |
| `vesper fix -y` | Diagnose/fix build-test failures |
| `vesper ask "…"` | Chat only (no tools) |
| `vesper analyze` | Languages / manifests / suggested verify |
| `vesper models` | List Ollama models |
| `vesper init` | Create `.vesper/` project config |
| `vesper config show` / `set` | Layered config |
| `vesper memory …` | Project facts |
| `vesper restore …` | Restore file backups |
| `vesper doctor` | Health check |

### REPL

```text
vesper (ask) › summarize this codebase
vesper (ask) › /mode auto
vesper (ask) › /summarize agent loop
vesper (ask) › /help
```

### Session modes

| Mode | Behavior |
|------|----------|
| **ask** (default) | Mutating tools show a diff and ask y/n |
| **auto** | Routine edits auto-run; delete / dangerous shell / `git push` still ask |
| **plan** | Research only — mutating tools not offered |

## Configuration

- Global: `~/.vesper/config.json`
- Project: `<repo>/.vesper/config.json`

```bash
vesper config set model qwen2.5-coder:14b
vesper config set verify_command "cargo test" --project
vesper config set mode ask --project
vesper config set num_ctx 4096
vesper config set num_predict 512
```

Env overrides: `VESPER_MODEL`, `VESPER_OLLAMA_URL`, `VESPER_WORKSPACE`

## Architecture

```
vesper/
├── vesper-cli       # VESPER binary + REPL
├── vesper-agent     # tool loop, summarize, verify
├── vesper-tools     # sandboxed FS / shell / git / gather
├── vesper-llm       # Ollama client
├── vesper-memory    # session + project facts
└── vesper-config    # layered settings
```

## Requirements

- Rust 1.75+
- [Ollama](https://ollama.com) with a coding model  
  - Laptop: `qwen2.5-coder:7b`  
  - NVIDIA GPU (e.g. 4060 Ti): `qwen2.5-coder:14b` or larger

## License

MIT
