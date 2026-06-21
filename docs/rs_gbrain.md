# rs_gbrain (local Rust brain)

Sibling repo: `../rs_gbrain` → symlink `vendor/rs_gbrain`.

## Local only

No remote MCP. Brain is SQLite on disk; optional loopback HTTP is on the **rs_gbrain** crate (`--features local-http`), not OAuth MCP.

## unthinkclaw wiring

- Default feature `rs-gbrain`: `brain_search`, `brain_query`, `brain_put`, `brain_get`
- `[rs_gbrain]`: `enabled`, `inject_brief`, `dream_on_heartbeat`
- `[memory]`: `dream_on_heartbeat` + `heartbeat_chat_id` — dream runs when both memory and rs_gbrain flags allow
- Startup: `sync_workspace_brief`, host plugin scan (`plugin_hosts`)
- Workspace kit installs `plugins/rs_gbrain/SKILL.md` + `plugin.json` (OpenClaw/Hermes shaped)

Parity: `rs_gbrain/docs/PARITY.md`.