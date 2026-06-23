# unthinkclaw

unthinkclaw is a **local-first Rust AI agent runtime** for people who want the bot on
their own machine, not hidden behind a hosted control plane.

Small binary (~14MB), async-first (tokio), trait-driven. Ships with 10+ messaging
channels, 20+ LLM providers, pluggable memory (SurrealDB + RocksDB), tool guardrails,
context compaction, streaming tool-call parsing, autonomous coding mode, and a plugin
system with lifecycle hooks.

## Features

### Core
- **Async-first** — tokio throughout, no blocking on the runtime thread
- **Trait-driven architecture** — swap providers, channels, tools, memory backends
- **Tool guardrails** — loop detection, idempotent vs mutating classification,
  failure counting, configurable warn/block thresholds
- **Pluggable context compaction** — summarizer trait + default LLM-based compactor
  with configurable thresholds
- **Self-healing retry** — auto re-prompts LLM with error context on tool failures
- **Streaming tool-call parser** — state-machine detects `<tool_call>` blocks
  incrementally from partial LLM output for early tool execution
- **Lifecycle hooks** — plugins can intercept pre/post tool calls, session start/end
- **Skill template preprocessing** — `${HERMES_SKILL_DIR}`, `${HERMES_SESSION_ID}`
  variables + `!\`command\`` inline shell execution in SKILL.md

### Messaging Channels
Telegram, CLI, Discord, Slack, WhatsApp, Matrix, Signal, IRC, Google Chat, MS Teams

### LLM Providers
Anthropic (default), OpenAI-compat, Ollama, Copilot, OpenRouter, Groq, Together,
Mistral, DeepSeek, Fireworks, Perplexity, xAI, Moonshot, Venice, HuggingFace,
SiliconFlow, Cerebras, MiniMax, Vercel, Cloudflare

### Tools
shell, file_ops (read/write/list), edit, web_search, web_fetch, vibemania (subspace
coding agent), dynamic tools, MCP bridge, session management, cron scheduling,
browser automation, message send, config management, mode switching, brief summary

### Memory
- SurrealDB + RocksDB backend with conversation history, FTS5, vector embeddings
- Sticker cache, file indexing, code chunk storage
- Plugable via `MemoryProvider` trait

### Advanced
- **Autonomous coding mode** — 24/7 loop: reads TODO.md, runs agent, validates
  with tests, commits/pushes only on success. Failure pause state persists
  across restarts
- **Skill curator** — background task reviews skills, suggests improvements
  for stale/empty/low-quality skills
- **Trajectory export** — ReAct step serialization (thought→action→observation)
  for RL training data
- **Agent swarm** — parallel sub-agent spawning for distributed task execution
- **Cron scheduler** — SurrealDB-backed recurring tasks with in-memory fallback
- **Plugin system** — JSON-RPC 2.0 + lifecycle hooks (pre/post tool, session events)
- **Hot-reloadable tools, skills, and system prompt**
- **Self-update** — git poll + rebuild + optional restart

## Quick Start

```bash
cargo build --release

./target/release/unthinkclaw init
./target/release/unthinkclaw chat --config unthinkclaw.json
./target/release/unthinkclaw ask "summarize this repo" --config unthinkclaw.json
```

## Current Status

- ✅ `cargo clippy --all-targets` — 0 warnings
- ✅ `cargo test` — 85 tests pass, 0 fail
- ✅ `cargo build --release` — passes, ~14MB binary
- ✅ v0.2.0 released

## Configuration

Initialize with `unthinkclaw init`, edit `unthinkclaw.json`. Key sections:

- `agent` — max rounds, history limit, model selection, compaction thresholds
- `provider` — LLM backend choice + credentials
- `channel` — messaging platform config
- `policy` — shell/dynamic tool/plugin permission gates
- `runtime.self_update` — auto-update git polling
- `toolsets.enabled/disabled` — per-platform tool allow/deny lists

## Validation

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release
```

## Storage

- SurrealDB + RocksDB is the only backend for memory, session state, and
  swarm/coordinator data.
- `storage.backend` is fixed to `surreal`.
- Startup fails fast if config requests any other storage mode.

## Docs

- [docs/README.md](docs/README.md)
- [docs/TODO.md](docs/TODO.md)
- [docs/ROADMAP.md](docs/ROADMAP.md)
- [docs/SWARM.md](docs/SWARM.md)
