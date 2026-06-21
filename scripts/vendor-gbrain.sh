#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DEST="$ROOT/vendor/gbrain"
URL="${1:-https://github.com/garrytan/gbrain.git}"
REF="${2:-master}"

if [[ -z "$URL" ]]; then
  echo "Usage: $0 [git-url] [ref]" >&2
  echo "Default: https://github.com/garrytan/gbrain.git master" >&2
  exit 1
fi

mkdir -p "$DEST"
if [[ -f "$DEST/.git/HEAD" ]] || [[ -d "$DEST/.git" ]]; then
  echo "vendor/gbrain already a git checkout; pull instead: cd vendor/gbrain && git pull" >&2
  exit 1
fi

rm -rf "$DEST"/*
git clone --depth 1 ${REF:+--branch "$REF"} "$URL" "$DEST/tmp"
shopt -s dotglob
mv "$DEST/tmp"/* "$DEST/"
rmdir "$DEST/tmp"
echo "Cloned into vendor/gbrain. Keep README.md at repo root if upstream overwrote it."