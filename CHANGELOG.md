# Changelog

## [0.5.0]

### Breaking

- **`IrcChannel::start` now connects eagerly and returns an error** if the
  server is unreachable. It previously spawned a task that logged a connection
  failure and returned an apparently healthy receiver.
- **`SlackChannel::start` now requires a channel id** and errors without one.
  It previously polled `conversations.history` with no `channel` parameter,
  which Slack rejects on every call, so nothing was ever received.
- Webhook channels (Google Chat, WhatsApp, Teams) **return a bind error** from
  `start` instead of panicking a detached task. A taken port used to kill a
  background task while `start` returned a working-looking receiver.
- Skill discovery additionally scans `<workspace>/plugins`,
  `.openclaw/plugins` and `.hermes/plugins`. These load SKILL.md, which
  supports inline shell — the same trust level as `.apollo/skills`, but a
  wider surface than before.

### Fixed

- **Discord could not receive.** `start()` dropped its sender, so the bot was
  deaf. It now polls `/channels/{id}/messages`, seeding its cursor from the
  first response so startup does not replay history.
- **Slack could not receive** — see above.
- **IRC could not reply.** `send()` only logged; it now writes PRIVMSG through
  a writer shared with the read loop.
- **Google Chat could not send.** It used the service-account key verbatim as
  a bearer token; it now signs an RS256 JWT assertion and exchanges it for an
  access token, cached until a minute before expiry.
- **Seven channels were unreachable.** `--channel` had arms only for `cli`,
  `telegram`, `discord` and `none`, so slack, matrix, irc, signal, whatsapp,
  googlechat and msteams compiled, passed their tests, and could not be
  selected at all.
- **Plugin-registered tools never reached the agent.** They were logged and
  discarded, so a plugin could register a tool the agent could not call.
- **Discovered plugin skills never loaded.** `discover_host_plugins` scanned
  three plugin roots for SKILL.md and only logged what it found; skill
  discovery scanned none of them.

### Added

- `Channel::send_media` — images, documents, voice, video and animations from
  a URL or a local path (multipart upload). The default **errors**, and
  `supports_media()` reports the truth, so a channel can never accept an
  attachment and quietly deliver a text message with a link instead.
  Telegram implements it; every other channel reports `false`.
- `channels::ChannelRegistry` — `--channel` resolves by name instead of
  through a match in `main.rs`. Parameters come from `[channel].settings` in
  the config, falling back to `APOLLO_CHANNEL_<KEY>` in the environment.
- `PluginContext::register_channel` — a plugin adds a channel with no feature
  flag, no `mod.rs` entry and no core edit.
- `channels::webhook` — one webhook receiver shared by Google Chat, WhatsApp
  and Teams, replacing three hand-rolled copies.
- `tests/channel_conformance.rs` covers all ten channels on four assertions
  (receive, send shape, reply delivery, media contract), with nothing ignored.
  `tests/plugin_channel.rs` drives the plugin channel and tool paths.

### Known limitations

- Channel coverage is **mock-based**. Only Telegram and CLI have been
  exercised against a real service; the rest are verified against a local
  mock server built from the published API shapes.
- Discord is REST polling, not the gateway websocket: no DMs, reactions or
  presence, and it watches only the configured channel.
- Media is implemented for Telegram only.
- A host plugin whose manifest declares neither a SKILL.md nor an in-process
  registration still does nothing — `HostPluginEntry` carries no entrypoint,
  and giving it one means executing code from a discovered directory.

## [0.4.0]

### Breaking

- **The agent HTTP API now requires authentication.** `POST /v1/chat` and the
  `/v1/chat/stream` WebSocket need `Authorization: Bearer <token>`. The server
  writes a token to `~/.apollo/http-token` (mode 0600) on first run;
  `APOLLO_HTTP_TOKEN` overrides it. Requests carrying an `Origin` header are
  refused on both routes. `/health` stays unauthenticated. Any script calling
  the API must send the token.
- **The hosted gateway is gone**, along with `GatewayConfig`, `HostingConfig`,
  and the gateway findings in `doctor`/`audit`. Existing `apollo.json` files
  keep loading — the keys are ignored.
- `MemoryBackend` gained `clear_conversation`, defaulting to an explicit
  "unsupported" error rather than a silent no-op.

### Added

- `apollo tui` — a terminal UI, and the default for a bare `apollo` when the
  `apollo-tui` binary is present. Slash palette, model selector, live status
  bar. Falls back to the line-based chat, and says so, when it is absent.
- `apollo serve` — run the agent headless, serving only the HTTP/WS API.
- The background server now **outlives its client**, so the next launch is
  instant and cron and heartbeat keep running between sessions.
  `apollo stop` shuts it down over an authenticated `/shutdown`.
- `apollo autostart` — a launchd agent on macOS or a systemd user unit on
  Linux, both under the user's own directory. `--disable` removes it.
- `apollo config list|get|set|path` — dotted-path access validated against the
  real schema before anything is written, with secrets masked.
- A first-run wizard: a bare `apollo` with no config now sets itself up
  instead of failing.
- `telekinesis` tool — delegate a self-contained task to a `tk` worker.
- Shared credentials via `rs_ai_oauth` — one login works across apollo and
  telekinesis, including an existing Claude Code login.
- `/v1/state`, `/v1/model`, `/v1/clear` routes, all behind the same auth gate.
- CI on push and pull request. There was none before; only a tag-triggered
  release that ran `cargo build` alone.

### Fixed

- **The final reply was never returned to non-draft channels.** It was
  delivered only through a channel's draft finalize, so HTTP, MCP, the CLI
  loop and swarm workers all received an empty string. Telegram was the only
  surface that worked, which is why this survived until the first real HTTP
  turn. Affected every provider.
- **A ~35-line reply froze the TUI** for over ten minutes with no working
  Ctrl-C, because the renderer called ratatui's constraint solver once per
  child. Fixed upstream in `crepuscularity-tui` 0.4.23; measured 35 lines from
  a hang to 69ms, and 5000 lines to 124ms.
- Eleven byte-slice panics on non-ASCII text — routine for a Telegram-first
  bot — including one that crashed compaction of any Cyrillic, CJK or emoji
  conversation, and one that panicked at startup on an accented persona file.
- Six truncation sites gated on byte length while truncating by characters, so
  output passed through uncapped with a footer stating a count that was never
  removed.
- **The rx4 engine enforced no permission hooks.** A hook blocking `exec`
  blocked under `legacy` and ran under `rx4`. Both engines now share one
  `execute_tool_with_hooks`, with a parity test.
- The catastrophic-command guard matched substrings, so `rm -r -f /`,
  `rm -rf ~` and `dd of=/dev/sda` all passed. It now tokenizes, and has tests.
- Prompt injection could read `.env` and `apollo.json` through the file tools,
  and write to `.apollo/skills`, which is executed via `sh -c` on next load —
  turning injection into persistent code execution. Both closed.
- `exec` handed over every secret in the environment, undoing the deny-list.
  Child processes now get a filtered environment.
- Secret files were created world-readable and chmodded afterwards, and were
  not written atomically. They now go through a 0600 temp file and a rename.
- `/shutdown` called `process::exit`, so RocksDB was never flushed and
  in-flight requests were cut. Verified: a request that used to return
  `HTTP 000` at 4s now completes at 8.07s.
- Cron jobs could be created unschedulable and could not be revived.
- A mid-stream WebSocket drop re-ran the whole turn, including mutating tools.
- Embeddings of different dimensions were compared and returned as similar
  results; SurrealDB's KNN operator never parsed, so it had never once run.
- `apollo-tui` was not built by `cargo build --release` or shipped by the
  release workflow or `install.sh`, so the TUI reached no user.

### Performance

- Warm start ~41ms, down from ~90ms: RocksDB now opens lazily off the boot
  path, with a warm-up task so a broken database still errors promptly.
- reqwest clients are shared rather than rebuilt per call.
- Skill preprocessing and the autonomous loop's git and test commands no
  longer block the async runtime.

### Notes

- Vector search is an in-process brute-force scan and is slow above a few
  hundred rows. MTREE was measured and rejected — it cannot coexist with mixed
  dimensions, costs ~25x more per write, and read slower than the scan. See
  the benchmarks in `src/memory/surreal.rs`.
- `serve_channels` is still not drained on shutdown, so a channel turn that
  has run tools but not yet persisted its reply can lose the history record.
