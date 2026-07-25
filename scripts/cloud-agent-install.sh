#!/usr/bin/env bash
set -euo pipefail

if ! command -v rustc >/dev/null 2>&1; then
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
fi

# shellcheck disable=SC1091
source "${HOME}/.cargo/env" 2>/dev/null || true

rustup show active-toolchain >/dev/null 2>&1 || rustup default stable
cargo fetch
cargo check --all-targets
