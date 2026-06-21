# gbrain (vendor slot)

Place a cloned **gbrain** (or compatible graph / dream-memory) project here.

```bash
# from repo root (defaults to garrytan/gbrain)
./scripts/vendor-gbrain.sh
# or: ./scripts/vendor-gbrain.sh https://github.com/garrytan/gbrain.git master
```

After clone:

1. Read upstream README for runtime deps (Node, Python, etc.).
2. Add a thin adapter under `src/memory/gbrain_adapter.rs` if you need process/HTTP bridge.
3. Prefer calling upstream for graph expansion (“dream”) instead of duplicating algorithms in Rust.

`vendor/gbrain/` is gitignored except this README until you vendor a real tree.