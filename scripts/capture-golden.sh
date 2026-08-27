#!/usr/bin/env bash
# Capture golden reference output from a licensed local Stata installation.
#
# This script is DEVELOPER TOOLING ONLY. The normal build and the normal CI
# pipeline must never invoke it and must never require Stata to be installed
# (product spec section 32).
#
# Usage: scripts/capture-golden.sh tests/golden/stata18/core_surface.do
set -euo pipefail

STATA_BIN="${STATA_BIN:-/Applications/Stata/StataMP.app/Contents/MacOS/stata-mp}"

if [[ ! -x "$STATA_BIN" ]]; then
  echo "error: Stata binary not found at $STATA_BIN" >&2
  echo "       set STATA_BIN to override, or skip golden capture entirely." >&2
  exit 2
fi

if [[ $# -lt 1 ]]; then
  echo "usage: $0 <do-file> [more.do ...]" >&2
  exit 64
fi

for dofile in "$@"; do
  dir="$(cd "$(dirname "$dofile")" && pwd)"
  base="$(basename "$dofile")"
  echo "==> capturing $dofile"
  ( cd "$dir" && "$STATA_BIN" -b do "$base" >/dev/null 2>&1 ) || true
  log="${dir}/${base%.do}.log"
  if [[ ! -f "$log" ]]; then
    echo "error: no log produced for $dofile" >&2
    exit 1
  fi
  if grep -qE '^r\([0-9]+\);' "$log"; then
    echo "warning: $log contains Stata errors:" >&2
    grep -nB4 -E '^r\([0-9]+\);' "$log" >&2 || true
  fi
  echo "    -> $log ($(wc -l < "$log" | tr -d ' ') lines)"
done
