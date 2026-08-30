#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DEST="$ROOT/python/trace_diff/_bin"
TARGET="${CARGO_BUILD_TARGET:-}"
mkdir -p "$DEST"
BASE="$ROOT/target"
if [[ -n "$TARGET" ]]; then
  BASE="$BASE/$TARGET"
fi
if [[ -f "$BASE/release/trace-diff" ]]; then
  cp -f "$BASE/release/trace-diff" "$DEST/trace-diff"
  chmod +x "$DEST/trace-diff"
  echo "Bundled trace-diff -> $DEST"
elif [[ -f "$BASE/release/trace-diff.exe" ]]; then
  cp -f "$BASE/release/trace-diff.exe" "$DEST/trace-diff.exe"
  echo "Bundled trace-diff.exe -> $DEST"
else
  echo "Release binary not found under $BASE/release" >&2
  exit 1
fi
