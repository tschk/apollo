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

> **rx4 migration:** apollo is migrating its built-in agent loop to the
> `rx4` (rotary) harness engine. `rx4` is a crates.io dependency providing
> the core agent loop, tools, providers, sessions,
> skills, memory, guardrails, and MCP support. The bridge lives in
> `src/agent/rotary_bridge.rs`.
>
> Both loops are selectable at runtime via `agent.engine`:
>
> - `legacy` (default) — apollo's Planning → Executing → Summarizing state
>   machine in `loop_runner.rs`
> - `rx4` — the rotary harness owns model calls and tool cycling
>
> Under either engine apollo owns everything around the loop: system prompt,
> skill injection, conversation history, memory recall, tool set,
> persistence, and lifecycle hooks. Delegation happens at the single
> `handle_message` chokepoint, so every channel honors the setting.

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

Vector search is an in-process brute-force cosine scan. There is deliberately
**no MTREE index**, and that is now a measured decision rather than an
assumption — see `bench_search_embeddings_scaling` and
`bench_mtree_feasibility` in `src/memory/surreal.rs`. Both are `#[ignore]`d;
run them under `--release` or the numbers are meaningless:

```bash
APOLLO_BENCH_SIZES=100,1000,10000 \
  cargo test --release -p apollo-agent --lib bench_ -- --ignored --nocapture
```

Scan latency, dim 1536, release, macOS arm64, 100 queries per size:

| rows | median | p95 | insert/row |
|------|--------|-----|------------|
| 100 | 30ms | 34ms | 1.8ms |
| 1 000 | 278ms | 321ms | 0.9ms |
| 10 000 | 2 538ms | 5 018ms | 1.2ms |

(50 000 was not measured: a 10k corpus already costs ~900MB on disk, so 50k
needs ~4.5GB free that the benchmark host did not have. The curve is linear —
~0.27ms per candidate row with no meaningful constant — so 50k extrapolates to
roughly 13s.)

So the scan *is* too slow for an interactive agent above a few hundred rows —
it crosses 50ms at around 200 rows. MTREE is nonetheless the wrong fix, on all
three axes:

- **It cannot coexist with mixed dimensions.** One MTREE index locks the whole
  table to its `DIMENSION`; a subsequent 3-dim insert fails with `Incorrect
  vector dimension (3). Expected a vector of 1536 dimension.` Recording `dim`
  per row does not enable "one index per dimension" — that would require
  dimension-partitioned *tables*.
- **It is ~25x more expensive to write**: 29.76ms/row versus ~1.2ms/row.
- **It is slower to read.** At 2 000 rows, `vector <|10,COSINE|> $q` measured a
  2 558ms median against 961ms for the scan.

The bottleneck is materializing a 1536-element array as SurrealDB `Value`s for
every candidate row, not the cosine arithmetic. Pushing the score into the
engine (`vector::similarity::cosine(...) ORDER BY score LIMIT k`) was measured
too and is ~1.6x slower, because it walks the same arrays. A real fix has to
make a row cheaper to read — a compact vector encoding, or an ANN index kept
outside SurrealDB. Do not re-litigate MTREE without new numbers.

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

### Loop Runner (loop_runner.rs)
- Circuit breaker: 50 rounds max
- Loop detection: tool guardrails (idempotent/mutating classify, failure count, warn/block)
- Self-healing: auto re-prompt LLM with error context on tool failures
- Progress channel: receiver is kept alive (not dropped)
- History: last 20 messages loaded from the active backend, ordered ASC
- Context compaction: pluggable compactor trait, default LLM summarizer
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
| Discord | ❌ Incomplete | `channel-discord` — `start()` drops the sender, so nothing is ever received |
| Slack | ❌ Incomplete | `channel-slack` — polls `conversations.history` with no `channel` param, so every poll errors |
| WhatsApp | ⚠️ Opt-in | `channel-whatsapp` feature |
| Matrix | ⚠️ Opt-in | `channel-matrix` feature |
| Signal | ⚠️ Opt-in | `channel-signal` feature |
| IRC | ❌ Incomplete | `channel-irc` — `send()` only logs; the bot cannot reply |
| Google Chat | ❌ Incomplete | `channel-googlechat` — sends the service-account key as a bearer token instead of exchanging it for one |
| MS Teams | ⚠️ Opt-in | `channel-msteams` feature |

Channels marked ❌ compile and can be enabled, but do not work. They were
found non-functional by an audit after 0.4.0 shipped advertising them. No
channel except Telegram and CLI has an end-to-end test, which is why this went
unnoticed — see the conformance-test gap in section 12.

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