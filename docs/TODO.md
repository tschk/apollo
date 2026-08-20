# TODO

Last updated: 2026-07-27

## Done

### v0.2.0 Features
- [x] Streaming tool-call parser — state-machine detects `<tool_call>` blocks
  incrementally from streaming LLM output, fires events on `</tool_call>`
- [x] Tool guardrails — loop detection, idempotent/mutating classification,
  failure counting with configurable warn/block thresholds
- [x] Pluggable context compaction — `Compactor` trait + default LLM summarizer
- [x] Self-healing retry — auto re-prompt LLM with error context on tool failures
- [x] Trajectory export — ReAct step serialization (thought→action→observation→response)
  for RL training data
- [x] Autonomous mode — 24/7 workspace loop: read TODO.md, run agent, validate
  with tests, git commit/push only on success. Failure pause across restarts
- [x] Skill curator — background task reviews skills, suggests improvements
- [x] Plugin lifecycle hooks — PreToolHook, PostToolHook, LifecycleHook traits
- [x] Skill template preprocessing — `${HERMES_SKILL_DIR}` / `${HERMES_SESSION_ID}`
  vars + `!\`command\`` inline shell substitution in SKILL.md
- [x] Cron scheduler — SurrealDB-backed with in-memory noop fallback for testing
- [x] Hermes-style self-healing, loop detection, and compaction integration in
  loop_runner.rs
- [x] All 85 tests pass, 0 clippy warnings
- [x] v0.2.0 published to crates.io as `apollo-agent`

### Previous (v0.1.x)
- [x] SurrealDB memory schema includes `files`, `chunks`, FTS, and sticker cache
- [x] Conversation history is loaded by `chat_id` before agent calls
- [x] `CLAUDE.md` is in sync with repo instructions via symlink to `AGENTS.md`
- [x] `cargo clippy --all-targets -- -D warnings` passes
- [x] `cargo test` passes
- [x] `cargo build --release` passes
- [x] GitHub issue `#2` through `#6` resolved and closed
- [x] Toolset allowlists, session search, managed skills, Daytona scaffolding

## Next

- [x] Wire autonomous mode into CLI entrypoint (`apollo autonomous`)
- [x] Integrate streaming parser into execution loop — `recover_tool_calls`
  runs when the provider returns no native tool calls
- [x] Wire the rx4 bridge into the execution path
- [x] Refresh and persist expired Anthropic OAuth tokens
- [x] Add trajectory collection toggle — `agent.trajectory_enabled` in the
  JSON config (the repo has no `config.yaml`; `Config::load` reads JSON)
- [x] Add tests for Telegram markdown conversion and long-message chunking
- [ ] Add tests for audio/sticker handling in Telegram
- [ ] Make guardrails state persistable across restarts

## Backlog

- [ ] Session branching (JSONL tree with parent pointers in SurrealDB)
- [ ] Add `curator start|stop` CLI commands
- [ ] Docker runtime adapter to replace Daytona
- [ ] WASM-based plugin sandbox (long-term)
