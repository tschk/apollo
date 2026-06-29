# Hermes / OpenClaw ecosystem map (device-first `main`)

## What we already mirror

| Source | In unthinkclaw |
|--------|----------------|
| OpenClaw tool groups | `src/tools/*`, `toolsets.rs` |
| Hermes skill vars | `${HERMES_SKILL_DIR}`, `${HERMES_SESSION_ID}` in `skills/mod.rs` |
| Hermes autonomous loop | `autonomous.rs` (TODO.md driver) |
| hermes-rs streaming parser | `streaming_parser.rs` |
| OpenClaw auth paths | `~/.openclaw/...` in `bootstrap.rs`, `copilot.rs` |
| Host plugin scan | `plugin_hosts.rs` — `.openclaw/plugins`, `.hermes/plugins`, workspace `plugins/` |
| rs_gbrain | feature `rs-gbrain`, `brain_*` tools, brief inject, dream on heartbeat |

## v2 absorbed on main (partial)

- Workspace manifest: `.unthinkclaw/plugins/manifest.json` + `apply_package_manifest` (packages: `web`, `live`, `desktop`, …).
- `unthinkclaw-install` binary.
- Still not ported: Cargo feature-gated tool modules (`plugin-web`, …), `fastembed`, `vendor/equilibrium`.

## Gaps vs `feature/computer-use-integration`

- Do **not** merge Zig `computer_use` tree — use **rs_peekaboo** (crates.io) via `peekaboo` tool + feature `peekaboo`.

## rs_gbrain integration

- Crate: **gbrain** on crates.io (`rs-gbrain` dep alias).
- Config: `[rs_gbrain]` — `enabled`, `inject_brief`, `dream_on_heartbeat`.
- Tools: `brain_search`, `brain_query`, `brain_put`, `brain_get`.
- Host can also run `rs_gbrain serve` (MCP stdio) alongside in-process tools.

## rs_peekaboo integration

- Crate: **rs_peekaboo** `0.3` on crates.io, feature `peekaboo`.
- Tool: `peekaboo` — actions map to library (`image`, `see`, `click`, `type`, …).
- Build: `cargo build --features peekaboo` (not in default features).

## Consolidation goal

Land v2 ideas as **small commits** on `main`: install helper, optional tool gating, plugin manifest merge — without replacing `main` history or closing PRs before merge.