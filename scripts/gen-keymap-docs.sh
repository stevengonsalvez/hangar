#!/usr/bin/env bash
# Generate man/keyboard-shortcuts.md from the effective built-in keymap.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
OUT="$REPO_ROOT/man/keyboard-shortcuts.md"

if [[ -n "${AINB_BIN:-}" ]]; then
  if [[ "$AINB_BIN" = /* ]]; then
    BIN="$AINB_BIN"
  elif [[ -x "$AINB_BIN" ]]; then
    BIN="$(cd "$(dirname "$AINB_BIN")" && pwd)/$(basename "$AINB_BIN")"
  else
    BIN="$REPO_ROOT/$AINB_BIN"
  fi
else
  (cd "$REPO_ROOT" && cargo build --release -p ainb >&2)
  BIN="$REPO_ROOT/target/release/ainb"
fi

[[ -x "$BIN" ]] || { echo "[gen-keymap-docs] binary not found: $BIN" >&2; exit 1; }
"$BIN" keymap list --format md > "$OUT"
echo "[gen-keymap-docs] wrote $OUT" >&2
