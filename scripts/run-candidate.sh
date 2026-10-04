#!/usr/bin/env bash
set -euo pipefail

# Run an already-built debug binary with private data and loopback transport.
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
if (( $# != 0 )); then
  echo "Usage: bash scripts/run-candidate.sh (build the debug binary first)" >&2
  exit 2
fi
BIN="$ROOT/target/debug/chat-cmd-client"
if [[ ! -x "$BIN" ]]; then
  echo "Build first: cd web && npm ci && npm run build; then cargo build --features embedded-web at the repository root." >&2
  exit 1
fi

DATA="$ROOT/.smoke/candidate"
if [[ -L "$ROOT/.smoke" || -L "$DATA" ]]; then
  echo "Candidate data directories must not be symlinks." >&2
  exit 1
fi
mkdir -p "$DATA"
for candidate_file in "$DATA/chatcmd.db" "$DATA/chatcmd.db-wal" "$DATA/chatcmd.db-shm" "$DATA/chatcmd.log"; do
  if [[ -L "$candidate_file" ]]; then
    echo "Candidate data files must not be symlinks." >&2
    exit 1
  fi
done
export CHATCMD_BIND=127.0.0.1
export CHATCMD_PORT=8081
export CHATCMD_DB_PATH="$DATA/chatcmd.db"
export CHATCMD_LOG_PATH="$DATA/chatcmd.log"
export CHATCMD_WEB_DIST="$ROOT/web/dist"
cd "$DATA"
printf 'Candidate: http://127.0.0.1:8081\nData: %s\n' "$DATA"
exec "$BIN"
