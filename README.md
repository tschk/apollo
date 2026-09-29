# apollo

[![License: MPL-2.0](https://img.shields.io/badge/License-MPL_2.0-brightgreen.svg)](LICENSE)

apollo is a **local-first Rust AI agent runtime** for people who want the bot on
their own machine, not hidden behind a hosted control plane.

Single ~16MB binary, ~41ms warm start, async-first (tokio), trait-driven. Ships
with 10+ messaging channels, 20+ LLM providers, pluggable memory (SurrealDB +
RocksDB), tool guardrails, context compaction, XML tool-call recovery,
autonomous coding mode, and a plugin system with lifecycle hooks.

## Features

### Core
- **Async-first** — tokio throughout, no blocking on the runtime thread
- **Trait-driven architecture** — swap providers, channels, tools, memory backends
- **Tool guardrails** — loop detection, idempotent vs mutating classification,
  failure counting, configurable warn/block thresholds
- **Pluggable context compaction** — summarizer trait + default LLM-based compactor
  with configurable thresholds
- **Self-healing retry** — auto re-prompts LLM with error context on tool failures
- **rx4 rotary harness** — the agent loop is owned by the rx4 (rotary) engine;
  apollo owns everything around it (context, tools, memory, persistence)
- **XML tool-call recovery** — state-machine parser extracts `<tool_call>`
  blocks from the response text, so providers without native tool calling still
  work. The parser also supports incremental feeding for streaming callers
- **Lifecycle hooks** — plugins can intercept pre/post tool calls, session start/end
- **Skill template preprocessing** — `${HERMES_SKILL_DIR}`, `${HERMES_SESSION_ID}`
  variables + `!\`command\`` inline shell execution in SKILL.md

### Messaging Channels
Telegram, CLI, Discord, Slack, WhatsApp, Matrix, Signal, IRC, Google Chat, MS Teams

Telegram and CLI are the default features and the two exercised against the
real services. The rest are opt-in Cargo features, covered end-to-end against
a mock server by `tests/channel_conformance.rs`. Verify one against its real
service with `apollo channel-check --channel <name>`.

### LLM Providers
Anthropic (API key or Claude subscription login), ChatGPT login, OpenAI-compat, Ollama, Copilot, OpenRouter, Groq, Together,
Mistral, DeepSeek, Fireworks, Perplexity, xAI, Moonshot, Venice, HuggingFace,
SiliconFlow, Cerebras, MiniMax, Vercel, Cloudflare

### Tools
shell, file_ops (read/write/list), edit, web_search, web_fetch, vibemania (subspace
coding agent), dynamic tools, MCP bridge, session management, cron scheduling,
browser automation, message send, config management, mode switching, brief summary,
and feature-gated signed desktop actions through Praefectus

### Web search
`web_search` picks a backend from the environment, in order:

1. `SEARXNG_URL` — a self-hosted SearXNG instance, queried through darash.
   Ranked results, no third-party key, and only the query leaves the machine.
2. Darash local — keyless and always available. In-process DuckDuckGo, OpenAlex,
   and Hacker News (`SearchClient::local()`).
3. `PERPLEXITY_API_KEY` — paid, kept for existing setups.

Anthropic can also run the search itself, which replaces the tool rather than
configuring it:

```json
{ "provider": { "name": "anthropic", "native_web_search": true } }
```

The model searches during its own turn, so results never round-trip through
apollo and nothing needs a search key — but the searches are billed by Anthropic,
and apollo's `web_search` tool is left unregistered to keep the name unambiguous.
Other providers ignore the setting.

### Memory
- SurrealDB + RocksDB backend with conversation history, BM25 full-text search,
  vector embeddings
- Each embedding records its dimension and model. Vector search filters by
  dimension and scores with an in-process cosine scan, so switching embedding
  models never mixes incomparable vectors. There is no ANN index — recall is
  exact and linear in the namespace
- RocksDB opens lazily on first use, not on the boot path
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
- **Cron scheduler** — SurrealDB-backed recurring and one-shot tasks with execution leases, retries, and in-memory fallback
- **Plugin system** — JSON-RPC 2.0 + lifecycle hooks (pre/post tool, session events)
- **Hot-reloadable tools, skills, and system prompt**
- **Self-update** — git poll + rebuild + optional restart

## Quick Start

```bash
# From crates.io (binaries: apollo, apollo-install)
cargo install apollo-agent

# From source
cargo install --path .
./scripts/install.sh   # build release + install to ~/.local/bin

apollo init          # interactive setup wizard (`apollo setup` is an alias)
apollo               # terminal UI if apollo-tui is installed, else CLI chat
apollo tui           # terminal UI, starting a background server if none is up
apollo chat          # line-based CLI chat
apollo serve         # headless: run the agent, serve only the HTTP/WS API
apollo ask "summarize this repo"
apollo doctor        # diagnose config / deps
apollo audit         # security/config audit

cargo build --release -p apollo-tui   # build the terminal UI
cargo run -p apollo-ui                # desktop UI (or `apollo ui`); onboarding on first launch
```

Other subcommands: `status`, `mcp`, `message` (`msg`), `cron`, `autonomous`,
`swarm`, `self-update`.

Install from a release binary (after build):

```bash
cargo build --release
./target/release/apollo-install install
```

## Desktop app (apollo-ui)

`apollo-ui` is the desktop app, built with
[Crepuscularity](https://crepuscularity.tsc.hk) on GPUI and styled with the
Telekinesis portal tokens (zinc-950, Chivo Mono — bundled, SIL OFL).

```bash
cargo build -p apollo-ui -p apollo-agent   # keep `apollo` next to `apollo-ui`
./target/debug/apollo-ui                   # first launch: onboarding (or `apollo ui`)
./target/debug/apollo-ui --onboarding      # run the onboarding again
./target/debug/apollo-ui --advanced        # or --simple: switch mode (saved)
```

The first launch walks through setup: welcome → connect a model → where it
works → permission profile → a test prompt → simple or advanced. There is no
title bar, just a 2px progress line and `n / 6` in the footer.

**Connect a model.** Three sign-in cards are pinned at the top. Clicking a
card starts that login straight away: the browser opens on the provider's
sign-in page and the login comes back to a localhost port. Everything else is
in a dropdown you can type into to filter.

| Option | How it signs in | Works end to end? |
| --- | --- | --- |
| ChatGPT | browser OAuth (PKCE, callback on `localhost:1455`) via `rs_ai_oauth` | yes: apollo reads the shared ChatGPT login when `provider.api_key` is unset |
| Claude | browser OAuth (PKCE, callback on `localhost:53692`) via `rs_ai_oauth` | yes: `provider.name = "anthropic"` runs rs_ai's `ClaudeProvider` with the shared Claude login, sent the way Claude Code sends it, refreshed when it expires. Anthropic may bill subscription logins used outside Claude Code as "extra usage" |
| GitHub Copilot | browser OAuth (callback on `localhost:9876`) | the sign-in is saved, but apollo only uses it when built with `--features provider-copilot`. The card says so |
| Anthropic API key | `ANTHROPIC_API_KEY` | yes, same `ClaudeProvider` with `x-api-key` |
| ~90 API-key providers | API key | yes. The list is `rs_ai_providers::catalog` (the catalog telekinesis uses): OpenRouter, OpenAI, Gemini, xAI, DeepSeek, Z.ai / Z.ai coding plan, Kimi for coding, Moonshot, Qwen (DashScope and the coding plan), MiniMax, Xiaomi MiMo, OpenCode Zen, Groq, Mistral, Together, Fireworks, Cerebras, DeepInfra, NVIDIA, Hugging Face and the rest |
| Ollama | none (local) | yes |
| Custom endpoint | name + base URL + API key, never OAuth | yes, any OpenAI-compatible `/chat/completions` API |

Providers apollo constructs itself (OpenAI, Anthropic, Gemini, OpenRouter,
xAI, DeepSeek, Groq, Together, Mistral, Fireworks, Perplexity, Moonshot,
Venice, Hugging Face, SiliconFlow, Cerebras, MiniMax) get their key under
their own variable. Every other catalog provider runs through apollo's
OpenAI-compatible client: `provider.name` is the catalog id, `provider.base_url`
the catalog URL, and the key goes in `.env` as `APOLLO_PROVIDER_API_KEY`.

**The model list fills itself.** Picking a provider, pasting a key or finishing
a sign-in looks the models up on a background thread, in this order:

1. **live**: the provider's own `GET {base_url}/models` with the key just
   entered (Anthropic uses `x-api-key`; sign-ins use the plan's listing). When
   the provider answers, that answer is the list.
2. **models.dev**: `https://models.dev/api.json`, cached for 24 hours.
3. **built-in**: the model ids in the rs_ai catalog.

The note under the field says which one the list came from, and why the live
listing failed if it did. Network lists are cached in
`~/.apollo/models/<provider>.json`. You can always type any model id instead.

Sign-ins land in the shared credential store,
`~/.config/rs_ai/credentials/<provider>.json` (0600). The app only ever sees
whether a sign-in succeeded, never the token.

A custom endpoint is saved as `provider.name = "custom-<your name>"` with its
base URL; its key goes in `.env` as `APOLLO_PROVIDER_API_KEY`, which apollo
reads for any provider whose `provider.api_key` is unset. It is read before
the ChatGPT-login and `OPENAI_API_KEY` fallbacks, so a configured provider is
never silently switched to another one.

**Where it works.** Pick one folder, which holds `apollo.json`, `.env` and
`.apollo/` like `apollo init` does. Or pick **works everywhere**: the workspace
is `~` and the config lives in `~/.apollo/instances/<id>/`.

**Instances.** Each instance is its own config dir with its own provider,
model and permissions. Add one with "+ new instance" in the instance pill
(simple mode) or in the roster sidebar (advanced mode). Two instances cannot
share a folder. Switching instances re-roots the chat in that instance's
config dir.

**Modes.** Simple mode is the chat plus an instance pill and a settings link.
Advanced mode adds:
- a roster sidebar (initial, name, model; click to switch);
- **tools**: switch the permission profile (writes apollo.json) and see policy flags and toolsets;
- **model parameters**: steppers for `agent.max_rounds`, `agent.max_history_messages` and `agent.auto_compact_after`;
- **logs**: the session's turns, tool calls, timings and errors.

Settings has the simple/advanced toggle in both modes.

Files written:

- `<config dir>/apollo.json`: provider, base URL (Ollama, custom and
  catalog providers apollo runs through its OpenAI-compatible client),
  model, workspace, permission profile. `provider.api_key` stays `null`.
- `<config dir>/.env`: the key, under the variable apollo reads for that
  provider, mode `0600`. A key is never logged or shown back, not even its
  length.
- `~/.apollo/desktop.json`: mode, the active instance and the instance list
  (name, scope, config dir, provider, model, profile). No secrets. The older
  single-workspace format is migrated automatically.

The test prompt uses the most real path available: a running agent server
(`apollo chat`, full rx4 turn), else `apollo ask` in the instance's config
dir, else a local reply labelled **offline mock**.

On Linux the window opens through X11 or Wayland (Vulkan via Blade; Mesa's
lavapipe works for headless/Xvfb). Layouts live in `ui/views/*.crepus` and
`ui/src/shell.rs`.

Screenshots: [`docs/screenshots/apollo-ui/v3/`](docs/screenshots/apollo-ui/v3/).

## Agent HTTP API

`apollo chat` and `apollo serve` expose the agent on `127.0.0.1:31338`
(`APOLLO_HTTP_PORT` overrides the port, `APOLLO_HTTP=0` disables the server):

- `POST /v1/chat` — `{"message": "...", "chat_id": "..."}`
- `GET /v1/chat/stream` — WebSocket, same request body, streamed events
- `GET /health` — unauthenticated liveness check

**Breaking change: the API now requires a bearer token.** Both `/v1/chat` and
the WebSocket need `Authorization: Bearer <token>` and return `401` without it.
The server generates a token on first run and writes it to
`~/.apollo/http-token` (mode 0600); set `APOLLO_HTTP_TOKEN` to supply your own
to the server and its clients instead. Existing scripts must be updated:

```bash
curl -sS http://127.0.0.1:31338/v1/chat \
  -H "Authorization: Bearer $(cat ~/.apollo/http-token)" \
  -H 'Content-Type: application/json' \
  -d '{"message":"hello"}'
```

Requests carrying an `Origin` header are refused with `403` on both endpoints.
Browsers always send `Origin`, so no web page can drive the agent; native
clients (the TUI, apollo-ui, curl) never send it.

## Current Status

- ✅ `cargo clippy --all-targets --all-features` — 0 warnings
- ✅ `cargo test --all-features` — passing
- ✅ `cargo build --release` — passes, ~16MB `apollo` binary
- ✅ v0.3.1 — install from source, release binary, or `cargo install apollo-agent` (crates.io package; binaries `apollo`, `apollo-install`)

## Configuration

Initialize with `apollo init`, edit `apollo.json`. Key sections:

- `agent` — max rounds, history limit, model selection, compaction thresholds
- `provider` — LLM backend choice + credentials
- `channel` — messaging platform config
- `policy` — shell/dynamic tool/plugin permission gates
- `runtime.self_update` — auto-update git polling
- `toolsets.enabled/disabled` — per-platform tool allow/deny lists

## Validation

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo build --release
cargo test --all-features
```

`--all-features` is not optional — several providers are behind non-default
features.

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
