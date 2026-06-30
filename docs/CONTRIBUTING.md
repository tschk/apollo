# Contributing

## Pull requests

- **One logical change per PR** — tests, refactor, or feature; not mixed mega-diffs.
- **Merge or squash-merge** when CI passes (`cargo fmt`, `cargo clippy -D warnings`, `cargo test`). Prefer keeping PR open until merged; avoid closing as duplicate without a linked commit on `main`.
- **Draft PRs** for Jules/bot refactors: review diff, rebase onto `main`, merge if unique; close only after the same commit exists on `main` (reference SHA in comment).

## Build features

Default includes `plugin-web` and `plugin-skills`. Heavier packs:

```bash
cargo build --features full          # all channels, providers, plugins, swarm
cargo build --features plugin-advanced,plugin-browser,peekaboo
```

Runtime tool visibility still follows `toolsets` and workspace `.unthinkclaw/plugins/manifest.json`.

## Commits

Conventional prefix: `feat`, `fix`, `test`, `refactor`, `docs`, `chore`. No unrelated files in one commit.