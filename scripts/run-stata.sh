#!/usr/bin/env bash
# The ONE place a Stata batch run is spelled (plan W23, spec §32).
#
# The harness (`stratum-difftest`), the self-hosted workflow
# (`.github/workflows/stata-diff.yml`) and a developer at a licensed machine
# all invoke Stata through this script, so "how we run Stata" cannot drift
# between them. It is DEVELOPER/HARNESS TOOLING ONLY: the normal build and the
# normal CI pipeline never invoke it and never need Stata.
#
#   usage: scripts/run-stata.sh <do-file> [workdir]
#
#   env:   STATA_BIN   the batch-capable binary; probed from the conventional
#                      install locations when unset.
#
# The do-file is run with `-b do` in <workdir> (default: the do-file's own
# directory). Batch mode writes `<basename>.log` into the WORKING DIRECTORY —
# not beside the do-file — which is why the harness gives every case its own
# temp cwd and why this script prints the log path it expects on stdout.
#
# EXIT CODES — and the one thing this script refuses to do:
#
#     0   Stata launched and the log exists. NOTHING about the run's outcome:
#         `stata -b` returns 0 unconditionally, even on r(111), even on an
#         explicit `exit _rc`. The truthful record is the `r(NNN);` line in
#         the log, and PARSING THAT IS THE CALLER'S JOB. This script does not
#         try: an exit-code protocol here would tempt someone to trust it.
#     3   Stata launched but no log appeared (crashed before opening it).
#    64   usage error.
#    77   no Stata binary exists here — the harness's SKIP, and the reason
#         this script can be committed to a repository whose CI has no Stata.
set -uo pipefail

if [[ $# -lt 1 || $# -gt 2 ]]; then
  echo "usage: $0 <do-file> [workdir]" >&2
  exit 64
fi

dofile="$1"
if [[ ! -f "$dofile" && -n "${2:-}" && -f "$2/$dofile" ]]; then
  dofile="$2/$dofile"
fi
if [[ ! -f "$dofile" ]]; then
  echo "error: no such do-file: $1" >&2
  exit 64
fi

dodir="$(cd "$(dirname "$dofile")" && pwd)"
base="$(basename "$dofile")"
workdir="${2:-$dodir}"
if [[ ! -d "$workdir" ]]; then
  echo "error: no such workdir: $workdir" >&2
  exit 64
fi

# Locate a binary. Existence only — usability (an expired licence launches
# and does nothing) is the harness's probe, not ours.
if [[ -z "${STATA_BIN:-}" ]]; then
  for c in \
    /Applications/Stata/StataMP.app/Contents/MacOS/stata-mp \
    /Applications/Stata/StataSE.app/Contents/MacOS/stata-se \
    /usr/local/stata18/stata-mp \
    /usr/local/stata18/stata-se \
    /usr/local/stata/stata-mp \
    /usr/local/stata/stata-se; do
    if [[ -x "$c" ]]; then STATA_BIN="$c"; break; fi
  done
fi
if [[ -z "${STATA_BIN:-}" ]]; then
  if command -v stata-mp >/dev/null 2>&1; then STATA_BIN="$(command -v stata-mp)"; fi
fi
if [[ -z "${STATA_BIN:-}" || ! -x "$STATA_BIN" ]]; then
  echo "skip: no Stata binary found (set STATA_BIN); differential testing needs a licensed Stata (spec §32)" >&2
  exit 77
fi

# `-b` (batch): no GUI, no prompts, log to $PWD/<base>.log. The exit status
# is ignored ON PURPOSE — see the header. `|| true` spells that decision.
(
  cd "$workdir" || exit 64
  [[ "$dodir" == "$workdir" ]] || cp -f "$dodir/$base" .
  "$STATA_BIN" -b do "$base" >/dev/null 2>&1 || true
)

log="$workdir/${base%.do}.log"
if [[ ! -f "$log" ]]; then
  echo "error: Stata launched but wrote no log at $log" >&2
  exit 3
fi
echo "$log"
exit 0
