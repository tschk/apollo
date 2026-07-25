#!/usr/bin/env bash
set -euo pipefail

REPO_URL="${APOLLO_REPO:-https://github.com/tschk/apollo.git}"
BRANCH="${APOLLO_BRANCH:-main}"
INSTALL_DIR="${APOLLO_INSTALL_DIR:-$HOME/.local/bin}"

CLONE_DIR=""

cleanup() {
  if [[ -n "$CLONE_DIR" && -d "$CLONE_DIR" ]]; then
    rm -rf "$CLONE_DIR"
  fi
}

find_repo_root() {
  local dir="$1"
  while [[ -n "$dir" && "$dir" != "/" ]]; do
    if [[ -f "$dir/Cargo.toml" ]] && grep -qE '^name = "apollo(-agent)?"' "$dir/Cargo.toml" 2>/dev/null; then
      printf '%s\n' "$dir"
      return 0
    fi
    dir="$(dirname "$dir")"
  done
  return 1
}

resolve_source() {
  if [[ -n "${APOLLO_SOURCE_DIR:-}" ]]; then
    printf '%s\n' "$APOLLO_SOURCE_DIR"
    return 0
  fi

  local root=""
  if root="$(find_repo_root "$(pwd)")"; then
    printf '%s\n' "$root"
    return 0
  fi

  if [[ -n "${BASH_SOURCE[0]:-}" ]]; then
    local script="${BASH_SOURCE[0]}"
    if [[ -f "$script" ]]; then
      printf '%s\n' "$(cd "$(dirname "$script")/.." && pwd)"
      return 0
    fi
  fi

  if ! command -v git >/dev/null 2>&1; then
    echo "error: git is required when not run from a source checkout" >&2
    exit 1
  fi

  CLONE_DIR="$(mktemp -d)"
  trap cleanup EXIT
  git clone --depth 1 --branch "$BRANCH" "$REPO_URL" "$CLONE_DIR/apollo"
  printf '%s\n' "$CLONE_DIR/apollo"
}

ensure_cargo() {
  if command -v cargo >/dev/null 2>&1; then
    return 0
  fi
  # shellcheck disable=SC1091
  source "${HOME}/.cargo/env" 2>/dev/null || true
  if command -v cargo >/dev/null 2>&1; then
    return 0
  fi
  if ! command -v rustc >/dev/null 2>&1; then
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
    # shellcheck disable=SC1091
    source "${HOME}/.cargo/env"
  fi
}

install_binary() {
  local name="$1"
  local src="$2"
  local dst="$INSTALL_DIR/$name"
  if [[ ! -f "$src" ]]; then
    return 0
  fi
  mkdir -p "$INSTALL_DIR"
  cp "$src" "$dst"
  chmod +x "$dst"
  echo "Installed $dst"
}

path_hint() {
  case ":$PATH:" in
    *":$INSTALL_DIR:"*) return 0 ;;
  esac
  echo
  echo "Add to PATH (e.g. in ~/.bashrc or ~/.zshrc):"
  echo "  export PATH=\"$INSTALL_DIR:\$PATH\""
}

ROOT="$(resolve_source)"
RELEASE_DIR="$ROOT/target/release"

ensure_cargo

if [[ ! -f "$RELEASE_DIR/apollo" ]]; then
  echo "Building apollo (release)..."
  (cd "$ROOT" && cargo build --release)
fi

install_binary apollo "$RELEASE_DIR/apollo"
install_binary apollo-install "$RELEASE_DIR/apollo-install"

path_hint
