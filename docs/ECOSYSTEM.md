# Hermes / OpenClaw ecosystem map (device-first `main`)

## What we already mirror

| Source | In apollo |
|--------|----------------|
| OpenClaw tool groups | `src/tools/*`, `toolsets.rs` |
| Hermes skill vars | `${HERMES_SKILL_DIR}`, `${HERMES_SESSION_ID}` in `skills/mod.rs` |
| Hermes autonomous loop | `autonomous.rs` (TODO.md driver) |
| hermes-rs streaming parser | `streaming_parser.rs` |
| OpenClaw auth paths | `~/.openclaw/...` in `bootstrap.rs`, `copilot.rs` |
| Host plugin scan | `plugin_hosts.rs` — `.openclaw/plugins`, `.hermes/plugins`, workspace `plugins/` |
| zkr | feature `zkr-memory`, `zkr_memory` tool, recall inject, auto capture |

## v2 absorbed on main (partial)

- Workspace manifest: `.apollo/plugins/manifest.json` + `apply_package_manifest` (packages: `web`, `live`, `desktop`, …).
- `apollo-install` binary.
- Cargo feature-gated tools: `plugin-web`, `plugin-browser`, `plugin-skills`, `plugin-advanced` (+ `computer-use-praefectus`, `swarm`).
- Still not ported: `fastembed`, `vendor/equilibrium`.

## Computer use

- Crate: **praefectus** `0.3.0` on crates.io, feature `computer-use-praefectus` or bundle `desktop`.
- Tool: `praefectus` — truthful runtime capabilities, semantic observations with short-lived tags, and host-authorized `invoke` or verified `set_value` effects.
- The host policy check precedes one-operation authority issuance. Tool requests cannot supply authority, session isolation, coordinates, selectors, screenshots, or verification policy.
- `background_only` requests use the executor's reported isolation and background support. `outcome_unknown` is always `retry_safe: false` and must not be retried automatically.

## zkr integration

- Crate: **zkr** `0.2` on crates.io, feature `zkr-memory`.
- Config: `[zkr]` — `enabled`, `database`, `tenant_id`, `person_id`, `auto_capture`, `inject_recall`, `recall_limit`.
- Tool: `zkr_memory` — `remember`, `search`, `get`, `correct`, `delete`, `profiles`.
- Evidence-backed temporal memory with citations; replaces the former rs_gbrain integration.

## Consolidation goal

Land v2 ideas as **small commits** on `main`: install helper, optional tool gating, plugin manifest merge — without replacing `main` history or closing PRs before merge.
