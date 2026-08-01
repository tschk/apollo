# AGENTS.md — apollo Engineering Protocol

Scope: entire repository.

## Branch Posture

- `main` is the device-first runtime branch.
- Keep this branch focused on the local bot a user runs on their own machine.
- Hosted gateway / web control-plane is out of scope unless revived on a dedicated branch.

## 1) Project Snapshot

apollo is a lean, fast Rust AI agent runtime — Telegram-first, trait-driven, and using SurrealDB + RocksDB as the primary state layer.

Goals:
- Fast startup (<10ms), low RAM (<10MB)
- Async-first (tokio), no blocking on the runtime thread
- Swappable providers, channels, tools via traits
- Persistent memory with BM25 full-text + vector hybrid search
- Agent swarm support (parallel sub-agents)

> **Binary size:** The original <10MB target is no longer achievable with the
> current dependency tree (SurrealDB + RocksDB + rx4 + reqwest + axum). Size
> optimizations (`opt-level = "z"`, LTO, strip, `panic = abort`) remain enabled
> but the release binary exceeds 10MB. Treat <10MB as aspirational, not a gate.

> **Startup:** measured warm boot is ~41ms, not <10ms. It was ~90ms until
> `SurrealMemory` moved to a lazy `OnceCell` open — the embedded RocksDB open
> cost ~51ms and `SCHEMA_SQL` ~10ms, and both now happen off the boot path,
> with a warm-up task in `build_memory_backend` so a broken database still
> errors promptly rather than waiting for the first message.
>
> Everything else on the boot path totals under 1.5ms. Skill discovery, prompt
> assembly, config parsing and workspace setup are each well under a
> millisecond and are *not* worth optimizing — measure before believing
> otherwise. A schema-version gate to skip the 69 `DEFINE` statements was
> tried and measured at no gain; don't re-add it. <10ms remains out of reach,
> so treat it as aspirational.

> **rx4 engine:** apollo's agent loop is owned by the `rx4` (rotary) harness
> engine. `rx4` is a crates.io dependency providing the core agent loop, tools,
> providers, sessions, skills, memory, guardrails, and MCP support. The bridge
> lives in `src/agent/rotary_bridge.rs`.
>
> apollo owns everything around the loop: system prompt, skill injection,
> conversation history, memory recall, tool set, persistence, and lifecycle
> hooks. Delegation happens at the single `handle_message` chokepoint, so
> every channel honors the setting.
>
> The legacy Planning → Executing → Summarizing state machine has been
> removed. rx4 is the only engine.
>
> Working identically: pre/post tool hooks, plugin pre-tool blocking,
> `BeforeToolCall`/`AfterToolCall` lifecycle events, and
> `ToolStart`/`ToolEnd` stream events — all funnel through
> `rotary_bridge::execute_tool_with_hooks`. Draft progress works because the
> bridge is handed the turn's stream sink.
>
> Trajectory recording stays in apollo and is fed from `rx4::Event`
> subscription.
>
> `tests/rx4_engine.rs` drives a real turn. Extend it before changing the
> bridge.

Key extension points:
- `src/providers/traits.rs` — AI model providers
- `src/channels/traits.rs` — messaging channels
- `src/tools/traits.rs` — tool execution
- `src/memory/` — memory backends (SurrealDB + RocksDB)

## 2) Architecture

```
src/
  main.rs              — CLI entrypoint
  lib.rs               — module exports
  config/              — config schema + loading
  agent/               — orchestration loop, compaction trait, skill curator
  channels/            — Telegram, Discord, Slack, CLI, etc.
  providers/           — Anthropic, OpenAI-compat, Ollama, Copilot
  tools/               — shell, file_ops, web_search, web_fetch, edit, vibemania, guardrails
  memory/              — SurrealDB-backed history, cache, retrieval
  streaming_parser.rs  — tolerant XML tool-call parser (streaming)
  trajectory.rs        — ReAct step serialization for RL training
  autonomous.rs        — 24/7 TODO.md-driven coding loop
  swarm.rs             — parallel sub-agent spawning
  cron_scheduler.rs    — scheduled tasks
  heartbeat.rs         — periodic background checks
  plugin.rs            — plugin system + lifecycle hooks
  skills/              — skill loading, template vars, inline shell, curator
  cost.rs              — token cost tracking
  embeddings.rs        — embedding provider trait
```

## 3) Engineering Principles

### 3.1 Async-First
- All I/O must use `tokio` async primitives
- Voice/audio transcription uses `tokio::process::Command`, not `std::process::Command`

### 3.2 KISS
- Prefer explicit match branches over dynamic dispatch where possible
- Keep error paths obvious — use `?` and `bail!`
- No clever meta-programming in security-sensitive paths

### 3.3 YAGNI
- Don't add config keys or feature flags without a concrete use case
- Don't add speculative abstractions

### 3.4 Secure by Default
- Deny-by-default for channel allowlists
- Privileged capabilities are enabled by default and remain individually configurable through runtime policy and onboarding profiles
- Never log tokens, API keys, or message content
- Filesystem access scoped to workspace

### 3.5 Fail Fast
- Unsupported states should error explicitly, not silently fall back
- Validate inputs at tool boundaries

## 4) Key Implementation Notes

### Memory
- SurrealDB + RocksDB is the intended primary backend direction
- Sticker cache: `sticker_id → description` (avoids re-analysis)
- Conversation history: last 20 messages per chat_id, loaded on each request

#### Storage layers — what is actually where

"SurrealDB + RocksDB" is **one** database, not two. RocksDB is SurrealDB's
embedded storage engine, selected by the `kv-rocksdb` feature on the
`surrealdb` dependency. The `DEFINE TABLE`/`DEFINE INDEX` statements in
`SCHEMA_SQL` are SurrealQL, SurrealDB's own schema language — not SQLite.

- **SurrealDB** (`src/memory/surreal.rs`) — primary store. Memories,
  conversations, embeddings, sticker cache, cron jobs, graph memory nodes.
  Full-text search is SurrealDB's own BM25 with a snowball analyzer.
- **zkr** (`src/memory/zkr.rs`, `zkr-memory` feature, on by default) — a
  genuinely separate SQLite-backed store for evidence-backed temporal memory.
  This is the only other embedded database in the default build.
- **rocksdb** (direct dependency) — used *only* by `src/swarm/storage.rs`,
  behind the non-default `swarm` feature. It resolves to the same
  `librocksdb-sys` that SurrealDB already pulls in, so it costs no extra
  compilation and should not be "consolidated away".

Vector search is an **exact** brute-force cosine scan over an in-process
vector cache, not over SurrealDB rows. There is deliberately **no MTREE index
and no ANN crate**, and both are measured decisions rather than assumptions —
see `bench_search_embeddings_scaling` and `bench_mtree_feasibility` in
`src/memory/surreal.rs`. Both are `#[ignore]`d; run them under `--release` or
the numbers are meaningless:

```bash
APOLLO_BENCH_SIZES=100,1000,10000,50000 \
  cargo test --release -p apollo-agent --lib bench_ -- --ignored --nocapture
```

Search latency, dim 1536, release, macOS arm64, 100 queries per size. "Before"
is the previous design (`array` rows read from SurrealDB on every query);
"after" is the cache. Both columns were measured on the same host:

| rows | before median | after median | after p95 | insert/row |
|------|---------------|--------------|-----------|------------|
| 100 | 30ms | 2.4ms | 3.0ms | 1.1ms |
| 1 000 | 278ms | 6.2ms | 7.4ms | 0.4ms |
| 10 000 | 2 538ms | 38.1ms | 43.5ms | 0.35ms |
| 50 000 | ~13s (extrapolated) | 219ms | 712ms | 0.63ms |

Writes did not get more expensive — they got cheaper, because the packed
encoding is less work to serialise (10k rows: 0.35ms/row versus 3.60ms/row for
`array`), and on-disk size roughly halved (10k: 83.8MB versus 180.9MB).

Two costs are real and were **not** free:

- **Cold start.** The first search in a namespace loads every vector for it:
  ~1.8s at 10k rows, ~26s at 50k. Steady state is the table above. The load is
  once per namespace per process.
- **RAM.** The cache holds `dim * 4` bytes per row — ~60MB at 10k rows and
  ~300MB at 50k. This is the reason the <10MB RAM goal is dead, not a new
  regression, but it does scale with corpus size.

Why not an ANN crate. `hnsw_rs`, `instant-distance`, `usearch`, `arroy`,
`granne` and `annoy-rs` were all evaluated. All are permissively licensed, but:
`instant-distance` and `granne` are archived/unmaintained and build-once only;
`annoy-rs` cannot build an index at all, only read Spotify Annoy files;
`hnsw_rs` has no delete API whatsoever; `usearch` and `arroy` are the only two
with insert+delete+persistence, and both compile native C/C++ (relevant for the
musl cross-compile targets). Every one of them fixes dimension per index, so
mixed dimensions would need one index instance per dimension. None of that is
worth taking on when an exact scan of a cached corpus already answers in 38ms
at 10k rows — and an approximate index would trade away exactness for it.

MTREE remains the wrong fix, on all three axes:

- **It cannot coexist with mixed dimensions.** One MTREE index locks the whole
  table to its `DIMENSION`; a subsequent 3-dim insert fails with `Incorrect
  vector dimension (3). Expected a vector of 1536 dimension.` Recording `dim`
  per row does not enable "one index per dimension" — that would require
  dimension-partitioned *tables*.
- **It is ~25x more expensive to write**: 29.76ms/row versus ~1.2ms/row.
- **It is slower to read.** At 2 000 rows, `vector <|10,COSINE|> $q` measured a
  2 558ms median against 961ms for the scan.

Correction to an earlier note in this file: materializing the 1536-element
array as SurrealDB `Value`s was *not* the whole bottleneck. Storing the vector
as a single packed base64 `string` instead of an `array` — one `Value` per row
rather than 1536 — only bought 2.2x (1 000 rows: 265ms to 122ms). The rest is
SurrealDB's own per-row query cost: a `SELECT namespace, key` that projects no
vector at all still measured ~0.07ms/row, i.e. a ~700ms floor at 10k rows that
no encoding can get under. That floor is why the vectors are cached in process.
The packed encoding was kept anyway — it halves disk and makes the cold load
and the inserts cheaper. Do not re-litigate MTREE without new numbers.

The vector is stored in `vector_b64` as base64 of little-endian `f32`, not as a
native `bytes` field, because neither reachable `Bytes` type round-trips:
`surrealdb::Bytes` is a newtype with a derived `Serialize` and stores as an
array of 6144 integers, and `surrealdb::sql::Bytes`, which does emit a native
bytes value, is not exported by the 2.3 SDK. Rows written before this encoding
still carry `vector` as an array and are read back through the same path, so no
migration is needed.

SurrealDB's native KNN operator is *not* used, despite what earlier notes said.
The query was written `vector <| $v |>`, which does not parse (K must be a
literal integer), so it errored on every call and the code silently fell
through to the scan. The correct form `vector <|K,COSINE|> $v` parses but
returns nothing without an MTREE index, so the KNN path was removed rather
than left as a dead round-trip.

Every embedding row records the `dim` and `model` that produced it, and
searches filter on `dim`. Vectors of a different dimension are not comparable,
and before this filter existed they were scored `0.0` and still returned as
"similar" results. Rows predating the field have no `dim` and are excluded.

### Telegram (telegram.rs)
- Markdown sanitizer: single-pass state machine (not asterisk counting)
- Message chunking: paragraph-aware, 4096-char Telegram limit
- Markdown fallback: try with parse_mode, retry without on error
- Voice: `tokio::process::Command` for faster-whisper transcription

### Agent Loop (loop_runner.rs)
- Circuit breaker: max rounds forwarded to rx4 as `max_tool_iterations`
- Progress channel: receiver is kept alive (not dropped)
- History: last 20 messages loaded from the active backend, ordered ASC
- Context compaction: rx4 auto-compaction via `agent.auto_compact_after`
- Trajectory recording: per-chat ReAct step capture for RL training
- Lifecycle hooks: pre/post tool, session events from plugin system
- Skill preprocessing: template vars + inline shell in SKILL.md

### Swarm (swarm.rs)
- Spawns parallel Codex sub-agents via API
- Each agent gets isolated context
- Results merged back to caller
- Used for: security audits, code review, parallel research

## 5) Channels

| Channel | Status | Notes |
|---------|--------|-------|
| Telegram | ✅ Default | Full support (default feature) |
| CLI | ✅ Default | Dev/testing (default feature) |
| Discord | ⚠️ Opt-in | `channel-discord` — REST polling of `/channels/{id}/messages`, not the gateway websocket |
| Slack | ⚠️ Opt-in | `channel-slack` — requires `with_channel`; `start()` errors without one |
| WhatsApp | ⚠️ Opt-in | `channel-whatsapp` feature |
| Matrix | ⚠️ Opt-in | `channel-matrix` feature |
| Signal | ⚠️ Opt-in | `channel-signal` feature |
| IRC | ⚠️ Opt-in | `channel-irc` feature |
| Google Chat | ⚠️ Opt-in | `channel-googlechat` — OAuth2 service-account JWT exchange, needs a JSON key |
| MS Teams | ⚠️ Opt-in | `channel-msteams` feature |

Every channel is covered end-to-end by `tests/channel_conformance.rs`, which
drives each implementation against a local mock server and asserts three
things: `start()` yields a receiver that really delivers, `send()` puts a
correctly shaped request on the wire, and the `Delivery` contract holds. The
four channels that shipped non-functional in 0.4.0 (Discord's dropped sender,
Slack's missing `channel` param, IRC's log-only `send()`, Google Chat's raw
key used as a bearer token) were pinned as `#[ignore]`d tests there and are
now fixed, with the suite green and nothing ignored.

Add a case to that suite when adding a channel. It is the only thing standing
between "compiles" and "works" — that gap is how 0.4.0 shipped advertising
four channels that could not send or receive.

## 6) Providers

| Provider | Notes |
|----------|-------|
| Anthropic | Primary (Codex-sonnet-4-5 default) |
| OpenAI-compat | Any OpenAI-compatible endpoint |
| Ollama | Local + remote |
| GitHub Copilot | OAuth flow |

## 7) Tools

| Tool | Description |
|------|-------------|
| shell | Execute shell commands |
| file_ops | Read/write/list files |
| web_search | Perplexity/Brave search |
| web_fetch | Fetch URL content |
| edit | Surgical file edits |
| message | Send Telegram/Discord messages |
| vibemania | Subspace coding agent |
| dynamic | Dynamically loaded tools |
| session | Session management |
| guardrails | Loop detection, failure counting, idempotent/mutating classify |
| cron | Schedule recurring tasks |
| mode_switch | Switch execution mode at runtime |
| doctor | Diagnostics and health checks |
| worktree | Git worktree operations |
| config | Configuration management |
| sleep | Timer utility |

## 8) Validation

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo build --release -p apollo-agent -p apollo-tui
cargo test --workspace --all-features
```

The repository root is both the workspace root *and* a package, so a bare
`cargo build --release` builds `apollo-agent` alone and silently skips
`apollo-tui`. That is how the TUI first shipped unbuilt — `apollo` falls back
to the line-based chat when the `apollo-tui` binary is absent, so the gap
surfaced as a missing feature rather than a build error. Name both binaries.

Do **not** routinely run `cargo build --workspace --release`. That pulls in
`apollo-ui`, whose GPUI dependency tree produces well over 20GB of release
artifacts and has filled this machine's disk. Build `apollo-ui` deliberately
when you are working on it, not as part of a validation sweep. `clippy` and
`test` over the whole workspace are check-only and stay cheap.

`--all-features` is not optional. Several providers are behind non-default
features, so a `ProviderCapabilities` field added without it compiles locally
and breaks for anyone enabling `provider-copilot` — which is how 0.3.0 shipped
broken.

If `ndarray-stats` fails to compile against `indexmap 1.9.3` (`IndexMap` "takes
3 generic arguments"), the build-script artifacts in `target/` are corrupt —
indexmap 1.x detects `std` from a build script, and a truncated probe leaves
`has_std` unset. `rm -rf target/release` and rebuild; it is not a source error.

## 9) Workflow

- Work on feature branches, not main
- Small focused commits
- `cargo build --release` before pushing — verify binary size stays reasonable
- No blocking calls on async runtime
- No secrets in commits

## 9.1 Documentation Rules

- Keep root Markdown short, current, and user-facing.
- Keep planning and migration notes in `docs/`.
- Rewrite stale speculative docs into concise migration notes instead of letting
  them drift.
- Do not keep generated placeholder state as if it were maintained
  documentation.
- `CLAUDE.md` should stay aligned with this file; if it is a symlink, update
  this file and verify the link still resolves.

## 10) Anti-Patterns (Do Not)

- Do not call `std::process::Command` in async context — use `tokio::process::Command`
- Do not drop progress channel receivers
- Do not use global asterisk counting for markdown — use state machine
- Do not add heavy dependencies for minor convenience
- Do not silently weaken security/allowlist defaults
- Do not mix unrelated changes in one commit

### Sweeping for a bug class

When a bug turns out to be one instance of a class, search for **the
condition that makes code wrong**, not for the construct the fix introduces.
Three sweeps in one day failed the same way by ignoring this:

- Fixing byte-slice panics, the sweep grepped `truncate_chars` — the helper
  the fix *added* — and missed the byte-length **gate** in front of it. Six
  sites survived two separate "exhaustive" passes.
- Fixing `&s[..n]` panics, the sweep matched literal indices and missed one in
  the very file it was editing.
- The `.apollo` deny-list blocked **reads** of credentials and left the
  **write** path open, which turned prompt injection into persistent code
  execution via `.apollo/skills`.

So: grep for the predicate (`.len() >`, a raw `[..`, a write path), not for
the helper. Then prove the sweep — write the failing case first and watch it
fail before the fix lands.

### Gates that cannot fail

`cargo build --release 2>&1 | grep -E '^error'; echo OK` prints OK whatever
happens: the `echo` is a separate command with its own exit status. Every
"release build passes" claim for a whole day rested on that line. Capture the
real status (`cmd; echo "EXIT=$?"`) and check it. The same applies to a test
that asserts on a constant, or on its own mock rather than the code.

## 11) Adding Things

### New Provider
- Implement `Provider` trait in `src/providers/`
- Register in `src/providers/mod.rs`

### New Channel
- Implement `Channel` trait in `src/channels/`
- Handle allowlist, typing indicators, message chunking

### New Tool
- Implement `Tool` trait in `src/tools/`
- Validate all inputs, return structured `ToolResult`
- Never panic in tool execution path

### New Memory Backend
- Implement `Memory` trait in `src/memory/`
- All DB ops via `spawn_blocking`


<claude-mem-context>
# Memory Context

# [apollo] recent context, 2026-05-05 12:29pm GMT+10

No previous sessions found.
</claude-mem-context>