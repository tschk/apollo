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
| zkr | feature `zkr-memory`, `zkr_memory` tool, recall inject, auto capture |

## v2 absorbed on main (partial)

- Workspace manifest: `.unthinkclaw/plugins/manifest.json` + `apply_package_manifest` (packages: `web`, `live`, `desktop`, …).
- `unthinkclaw-install` binary.
- Cargo feature-gated tools: `plugin-web`, `plugin-browser`, `plugin-skills`, `plugin-advanced` (+ `peekaboo`, `swarm`).
- Still not ported: `fastembed`, `vendor/equilibrium`.

## Gaps vs `feature/computer-use-integration`

- Do **not** merge Zig `computer_use` tree — use **rs_peekaboo** (crates.io) via `peekaboo` tool + feature `peekaboo`.

## zkr integration

- Crate: **zkr** `0.2` on crates.io, feature `zkr-memory`.
- Config: `[zkr]` — `enabled`, `database`, `tenant_id`, `person_id`, `auto_capture`, `inject_recall`, `recall_limit`.
- Tool: `zkr_memory` — `remember`, `search`, `get`, `correct`, `delete`, `profiles`.
- Evidence-backed temporal memory with citations; replaces the former rs_gbrain integration.

## rs_peekaboo integration

- Crate: **rs_peekaboo** `0.3` on crates.io, feature `peekaboo`.
- Tool: `peekaboo` — actions map to library (`image`, `see`, `click`, `type`, …).
- Build: `cargo build --features peekaboo` (not in default features).

## Consolidation goal

Land v2 ideas as **small commits** on `main`: install helper, optional tool gating, plugin manifest merge — without replacing `main` history or closing PRs before merge.