# GBrain sidecar (garrytan/gbrain)

unthinkclaw does **not** reimplement GBrain. It shells out to the upstream CLI or calls a remote `gbrain serve --http` MCP (future).

## Why sidecar

GBrain is Postgres/PGLite + hybrid RAG + entity graph + dream cycle + synthesis (`think`). That stack is ~100k LOC TypeScript/Bun. The lazy path: **one brain daemon, thin Rust harness**.

## Install upstream

```bash
./scripts/vendor-gbrain.sh https://github.com/garrytan/gbrain.git master
cd vendor/gbrain && bun install
gbrain init --pglite   # or follow INSTALL_FOR_AGENTS.md
```

Or install global CLI per [gbrain README](https://github.com/garrytan/gbrain).

## unthinkclaw config

```json
{
  "gbrain": {
    "enabled": true,
    "binary": "gbrain",
    "vendor_root": "vendor/gbrain",
    "search_timeout_secs": 120,
    "think_timeout_secs": 300
  }
}
```

- `vendor_root`: use `bun run src/cli.ts` from cloned repo when `binary` not on PATH.
- Dream cycle / ingest: run on host (`gbrain dream`, cron, minions) — not inside unthinkclaw.

## Agent tools

| Tool | Maps to |
|------|---------|
| `gbrain_search` | `gbrain search <query> --json` |
| `gbrain_think` | `gbrain think "<question>" --json` |

Enable toolset `memory` or leave toolsets empty (default allow).

## Local Surreal graph

`src/memory/graph.rs` stays a **lightweight** idea/dream map when GBrain is off. When GBrain is on, prefer `gbrain_*` tools for real brain queries.

## Remote MCP (optional)

Point coding agents at `gbrain serve --http` + OAuth per `docs/guides/agent-to-gbrain.md`. unthinkclaw HTTP MCP can be wired later via bearer token to `/mcp` — not required for v1 sidecar.