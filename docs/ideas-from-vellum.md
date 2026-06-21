# Ideas borrowed from personal-assistant patterns (Vellum-style)

Lean copies for `main` — no hosted control plane, no 8-type memory taxonomy.

## Shipped in code

| Idea | Where |
|------|--------|
| Memory brief (open loops + time windows) | `src/memory/brief.rs`, KV namespace `brief` |
| Recall gate → file + KV snippets | `src/memory/recall.rs`, `loop_runner` |
| Principal → merged history across channels | `src/memory/principal.rs`, `config.memory.principal_id` |
| Session-end daily note | `src/memory/session_note.rs`, `plugin` lifecycle hook |
| Channel ingress allowlist (opt-in) | `config.channel.allowed_*`, Telegram filter |
| Heartbeat delivery target | `config.memory.heartbeat_chat_id` |
| Workspace identity kit | `src/workspace_init.rs` |
| Graph memory (ideas + links + dream nodes) | `src/memory/graph.rs`, Surreal tables |
| gbrain vendor slot | `vendor/gbrain/README.md`, `scripts/vendor-gbrain.sh` |

## Deferred (YAGNI)

- Separate credential executor process
- LLM memory reducer daemon (use session notes + graph dream stub first)
- Full notification routing engine
- Native `auth-profiles.json` (OpenClaw import still works via `bootstrap.rs`)

## Config sketch (`unthinkclaw.json`)

```json
{
  "memory": {
    "principal_id": "me",
    "inject_context": true,
    "heartbeat_chat_id": "123456789",
    "dream_on_heartbeat": false
  },
  "channel": {
    "allowed_chat_ids": ["123456789"],
    "allowed_sender_ids": ["987654321"]
  }
}
```

Brief KV: namespace `brief`, keys `open_loops` / `time_contexts` (markdown bullet lists).

## gbrain

Clone upstream into `vendor/gbrain/` when you have a URL:

```bash
./scripts/vendor-gbrain.sh https://github.com/<org>/<gbrain-repo>.git
```

Wire adapters in Rust only at boundaries; do not reimplement graph logic in-tree unless the vendor is absent.