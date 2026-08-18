# Roadmap

Last updated: 2026-08-18

## Now

- keep the local-first runtime stable while `main` stays focused on the
  single-machine experience
- maintain 0 clippy warnings, 100% test pass rate
- improve integration docs and onboarding

## Done (v0.2.0)

- Streaming tool-call parser (hermes-rs `parser.rs` port) — incremental
  `<tool_call>` detection from partial LLM output
- Tool guardrails — loop detection, idempotent/mutating classification,
  failure counting with configurable warn/block thresholds
- Pluggable context compaction — `Compactor` trait + default LLM summarizer
- Self-healing retry — auto re-prompt LLM with error context on tool failures
- Trajectory export — ReAct step serialization for RL training data
- Autonomous coding mode — 24/7 workspace loop: read TODO.md, run agent,
  validate with tests, git commit on success
- Skill curator — background task that reviews skills and suggests improvements
- Plugin lifecycle hooks — pre/post tool call, session start/end events
- Skill template preprocessing — `${HERMES_SKILL_DIR}` / `${HERMES_SESSION_ID}`
  variables + `!\`command\`` inline shell in SKILL.md
- Cron scheduler — SurrealDB-backed with in-memory noop fallback for testing
- Cargo publish v0.2.0 to crates.io as `apollo-agent`

## Done (since v0.6.0)

- trajectory collection toggle, and trajectories that actually record steps
- loop detection wired to rx4, with per-chat failure streaks that survive a
  restart
- Telegram sanitizer, chunker and inbound-media tests
- a compactor that summarizes instead of claiming to
- skill routing for space-free scripts (Cantonese, Japanese)

## Next

- non-English operators end to end: descriptions, skill bodies and default
  prompts are still English-only
- make swarm state more visible and easier to operate
- improve operator diagnostics
- move scheduler/session/control metadata onto unified Surreal contract

## Later

- unify storage contracts for memory, session state, swarm tasks, and scheduling
- add better observability without bloating the binary
- add agent-authored skill maintenance and better long-term procedural memory

## Not For This Branch

- hosted gateway product work
- web UI and deployment surface
- multi-user hosted control-plane (not planned on `main`)
