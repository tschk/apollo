#!/bin/bash
# Hot-reload watcher for apollo
# Watches src/ and Cargo.toml for changes, rebuilds, and restarts the bot.

set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LOG="/tmp/aclaw.log"
PIDFILE="/tmp/apollo.pid"

# Load env from .env file
if [ -f "$REPO/.env" ]; then
  source "$REPO/.env"
fi

# Launch args (mirror production)
TELEGRAM_TOKEN="${APOLLO_TELEGRAM_TOKEN:?set APOLLO_TELEGRAM_TOKEN in .env}"
TELEGRAM_CHAT_ID="${APOLLO_CHAT_ID:-5708941906}"
MODEL="${MODEL:-claude-sonnet-4-5}"
BIN="$REPO/target/release/apollo"

# ── helpers ──────────────────────────────────────────────────────────────────

log() { echo "[watch] $(date '+%H:%M:%S') $*"; }

kill_bot() {
  if [ -f "$PIDFILE" ]; then
    local pid
    pid=$(cat "$PIDFILE")
    if kill -0 "$pid" 2>/dev/null; then
      log "Stopping bot (PID $pid)…"
      kill "$pid" && sleep 1
      kill -9 "$pid" 2>/dev/null || true
    fi
    rm -f "$PIDFILE"
  fi
}

start_bot() {
  log "Starting apollo…"
  cd "$REPO"
  nohup env APOLLO_TELEGRAM_TOKEN="$TELEGRAM_TOKEN" "$BIN" chat \
    --channel telegram \
    --telegram-chat-id "$TELEGRAM_CHAT_ID" \
    --model "$MODEL" \
    >> "$LOG" 2>&1 &
  echo $! > "$PIDFILE"
  log "Bot started (PID $(cat "$PIDFILE")) — log: $LOG"
}

build() {
  log "Building…"
  cd "$REPO"
  if cargo build --release 2>&1; then
    log "✅ Build OK"
    return 0
  else
    log "❌ Build FAILED — keeping old binary running"
    return 1
  fi
}

# ── initial build + start ─────────────────────────────────────────────────────

trap 'log "Shutting down…"; kill_bot; exit 0' INT TERM

log "Hot-reload watcher starting (repo: $REPO)"
build && { kill_bot; start_bot; }

# ── watch loop ────────────────────────────────────────────────────────────────

log "Watching src/ and Cargo.toml for changes…"

while true; do
  # Wait for any change in src/ or Cargo.toml (5 s timeout so we can check the bot is still alive)
  if inotifywait -r -e modify,create,delete,move \
       --timeout 5 \
       "$REPO/src" "$REPO/Cargo.toml" "$REPO/Cargo.lock" \
       -q 2>/dev/null; then

    # Debounce — collect rapid successive saves
    sleep 0.5

    log "Change detected — rebuilding…"
    if build; then
      kill_bot
      start_bot
    fi
  fi

  # Restart bot if it crashed
  if [ -f "$PIDFILE" ]; then
    pid=$(cat "$PIDFILE")
    if ! kill -0 "$pid" 2>/dev/null; then
      log "⚠️  Bot crashed — restarting…"
      rm -f "$PIDFILE"
      start_bot
    fi
  fi
done
