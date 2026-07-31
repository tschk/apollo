# Swarm

Last updated: 2026-03-18

This file describes the current swarm shape on `main`, not the ideal future
design.

## Current State

- swarm is feature-gated behind `--features swarm`
- shared state is intended to live in SurrealDB with RocksDB-backed local
  storage/cache
- the single-machine runtime still works without swarm enabled
- the broader manager/worker and operator UX work is still in progress

## What Exists

- agent registration and coordination primitives
- delegation links and concurrency controls
- team/task board primitives
- handoff routing
- scheduler/concurrency tracking
- Surreal-backed swarm storage modules

## What Is Not Finished

- the runtime path is not yet uniformly Surreal-first across the rest of the
  repository
- storage contracts are still split between newer swarm storage and older
  runtime assumptions
- operator visibility is still thinner than the underlying storage model
- distributed execution and isolation work are not complete

## Build Modes

```bash
# default local runtime
cargo build --release

# swarm-enabled build
cargo build --release --features swarm
```

## Related Docs

- [ROADMAP.md](ROADMAP.md)
