#!/usr/bin/env bash
# ARCHITECTURE §8, the text-scan half.
#
# `cargo xtask layering` reasons over the resolved crate graph. Everything below
# is a property of the SOURCE, invisible in that graph, so it is a scan — which
# is exactly how ARCHITECTURE §8 states items 3, 6 (workflow half), 7, 11 and
# 14. Runs as a required PR check alongside `xtask layering`.
#
# | check | invariant |
# |---|---|
# | tauri-bridge     | §8.3  — `@tauri-apps` imports live in exactly one file |
# | stata-free-ci    | §8.6  — no workflow but stata-diff.yml can reach Stata |
# | missing-values   | §8.7  — one definition of the Stata missing sentinels |
# | number-format    | §8.7  — user-visible numbers go through stratum_core::fmt |
# | no-fma           | §8.11 — no `mul_add`, no std f64 transcendental |
# | no-blas          | §8.11 — no faer/BLAS/LAPACK/MKL in any dependency tree |
# | graph-no-io      | §8.14 — stratum-graph resolves no paths and reads no file |
# | manifest-targets | R0    — every declared cargo target has a file on disk |
#
# Every check skips cleanly when its target does not exist yet, so this script is
# green on an empty tree and gets teeth as each unit lands. It never needs Stata.

set -uo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.." || exit 2
failures=0
checks=0
skipped=()

red() { printf '\033[31m%s\033[0m\n' "$*"; }

# `rg` when available (much faster on a big tree), `grep -r` otherwise, so a
# contributor without ripgrep gets the same answer as CI.
if command -v rg >/dev/null 2>&1; then
  scan() { rg --no-heading --line-number --color never "$@"; }
  scan_files() { rg --files-with-matches --color never "$@"; }
else
  scan() { grep -rEn --color=never "$@"; }
  scan_files() { grep -rEl --color=never "$@"; }
fi

# THE SCANS MATCH CODE, NOT PROSE. Drops any hit whose line is nothing but a
# Rust comment.
#
# Three of the seven checks below were red at once on a tree with no violation
# in it, because each of them found its own forbidden pattern inside a comment
# that was *explaining the rule*:
#
#   crates/stratum-graph/src/lib.rs:17   //! ... `rg 'std::fs|Utf8Path|...'`
#   crates/stratum-stats/src/ttest.rs    /// Stata's `.` is `8.98846...e307`
#   crates/stratum-stats/tests/...       /// A renderer that used `{:.4}` ...
#
# The first is the whole argument in one line: the module header states that the
# crate does no I/O, quotes the ripgrep that proves it, and is thereby reported
# as doing I/O. A check that fails on a docstring for describing the invariant
# it upholds trains people to ignore the check, which is the same reasoning that
# already keeps a bare `{}` out of NUMBER_FORMAT below.
#
# Whole-line comments only, on purpose. A trailing `let n = 1; // 32741` is
# still reported: deciding whether a match sits inside a comment, a string or
# code needs a lexer, and where this scanner has to guess it guesses towards
# reporting. Block comments are not tracked either — the repo's idiom is
# `//!`/`///`, and every line of a `/* */` block here starts with `*` or `/*`,
# which this pattern does not claim to catch.
code_lines() { grep -vE '^[^:]*:[0-9]+:[[:space:]]*(//|/\*|\*)'; }

# fail <check> <message> <offending-lines>. Deliberately NOT a pipeline target:
# a function on the right of `|` runs in a subshell, and `failures` would be
# incremented in a copy of the shell that then exits. That bug makes a topology
# check report every violation and still exit 0.
fail() {
  failures=$((failures + 1))
  red "FAIL  $1 — $2"
  printf '%s\n' "$3" | sed 's/^/      /'
}

pass() { printf 'ok    %s\n' "$1"; }

skip() { skipped+=("$1 ($2)"); }

# ---------------------------------------------------------------------------
# §8.3 — the Electron escape hatch is exactly one file
# ---------------------------------------------------------------------------
# This is why the migration estimate in ADR-001 is three weeks and not a
# rewrite: every Tauri API call in the frontend goes through one adapter.
check_tauri_bridge() {
  local dir=apps/desktop/src
  [[ -d $dir ]] || { skip tauri-bridge "$dir does not exist yet"; return; }
  checks=$((checks + 1))
  local expected='apps/desktop/src/platform/bridge.ts'
  local found
  found=$(scan_files '@tauri-apps' "$dir" 2>/dev/null | sed 's#^\./##' | sort)
  if [[ $found == "$expected" ]]; then
    pass tauri-bridge
  else
    fail tauri-bridge "expected exactly $expected, found:" "${found:-<no file imports @tauri-apps>}"
  fi
}

# ---------------------------------------------------------------------------
# §8.6 — spec §32, machine-checked: the normal build never needs Stata
# ---------------------------------------------------------------------------
# Matched on things that can only be a *path to* or *invocation of* Stata. A
# workflow is free to say the word "Stata" in a comment explaining why it does
# not use one — ci.yml does exactly that.
STATA_REACH='(StataMP|StataSE|StataBE|StataNow|stata-mp|stata-se|stata-be|/Applications/Stata|Program Files.{0,4}Stata|STATA_(PATH|HOME|CMD|LICENSE)|stata +-[bq]|run-stata\.sh|stratum-difftest|xtask +difftest)'
check_stata_free_ci() {
  local dir=.github/workflows
  [[ -d $dir ]] || { skip stata-free-ci "$dir does not exist yet"; return; }
  checks=$((checks + 1))
  local hits
  hits=$(scan -e "$STATA_REACH" "$dir" 2>/dev/null | grep -v '^\.\?/\?.github/workflows/stata-diff\.yml')
  if [[ -z $hits ]]; then
    pass stata-free-ci
  else
    fail stata-free-ci 'only .github/workflows/stata-diff.yml may reach Stata (spec §32)' "$hits"
  fi
}

# ---------------------------------------------------------------------------
# §8.7 — one definition of the missing-value encoding (ADR-005)
# ---------------------------------------------------------------------------
# The bit patterns and the `.`/`.a`..`.z` integer sentinels appear once, in
# stratum-core. A second copy is how the engine ends up with two answers to
# "is this missing", which ADR-005 exists to prevent.
MISSING_LITERALS='(8\.98846567431158[0-9]*e\+?307|0x7FE0_?0000_?0000_?0000|\b2147483621\b|\b32741\b|\b8\.988465674311579)'
check_missing_values() {
  [[ -d crates ]] || { skip missing-values 'crates/ is empty'; return; }
  checks=$((checks + 1))
  local hits
  hits=$(scan -e "$MISSING_LITERALS" crates 2>/dev/null | code_lines |
    grep -v '^\.\?/\?crates/stratum-core/src/missing\.rs')
  if [[ -z $hits ]]; then
    pass missing-values
  else
    fail missing-values 'the Stata missing sentinels are declared only in stratum_core::missing (ADR-005)' "$hits"
  fi
}

# ---------------------------------------------------------------------------
# §8.7 — user-visible numbers go through stratum_core::fmt (C12)
# ---------------------------------------------------------------------------
# An approximation, and deliberately the narrow one: a float *precision* spec in
# a format string (`{:.4}`, `{:e}`) is the failure mode that makes `list`, the
# Data Editor and an inline card disagree about the same number. Bare `{}` is
# not matched, because it is overwhelmingly used on strings and integers and a
# check that cries wolf gets suppressed.
NUMBER_FORMAT='\{:[-+#0-9 ]*\.[0-9]+(e|E)?\}|\{:[-+#0-9 ]*(e|E)\}'
check_number_format() {
  [[ -d crates ]] || { skip number-format 'crates/ is empty'; return; }
  checks=$((checks + 1))
  local hits
  # `benches/**` is out of scope, not exempted-because-inconvenient. C12 is about
  # the numbers a *user* reads — the failure mode named above is `list`, the Data
  # Editor and an inline card disagreeing. A criterion harness printing
  # `bytes_scanned = {} ({:.2} % of the file)` to its own stdout is an ADR-017
  # counter being reported to whoever ran the bench, and routing it through
  # `stratum_core::fmt` would make the engine's number formatter a dependency of
  # benchmark diagnostics for no reader's benefit. `tests/**` is deliberately NOT
  # exempt: a test can contain a second renderer, which is exactly the bug.
  hits=$(scan -e "$NUMBER_FORMAT" crates 2>/dev/null | code_lines |
    grep -v '^\.\?/\?crates/stratum-core/src/fmt\.rs' |
    grep -v '^\.\?/\?crates/stratum-core/src/missing\.rs' |
    grep -v '^\.\?/\?crates/[^/]*/benches/')
  if [[ -z $hits ]]; then
    pass number-format
  else
    fail number-format 'format a user-visible number with stratum_core::fmt, not a precision spec (C12)' "$hits"
  fi
}

# ---------------------------------------------------------------------------
# §8.11 — numeric reproducibility (ADR-004 / ADR-013)
# ---------------------------------------------------------------------------
# `mul_add` fuses a multiply and an add at one rounding step, so the same
# expression gives a different last bit depending on whether the target has FMA.
# The std transcendentals are not correctly rounded and differ between libms.
# `sqrt` is exempt: IEEE-754 requires it to be correctly rounded.
TRANSCENDENTALS='\.(ln|ln_1p|log|log10|log2|exp|exp_m1|exp2|powf|powi|sin|cos|tan|asin|acos|atan|atan2|sinh|cosh|tanh|asinh|acosh|atanh|cbrt|hypot|mul_add)\('
check_no_fma() {
  [[ -d crates ]] || { skip no-fma 'crates/ is empty'; return; }
  checks=$((checks + 1))
  local hits
  hits=$(scan -e "$TRANSCENDENTALS" crates 2>/dev/null | code_lines |
    grep -v '^\.\?/\?crates/stratum-core/src/math\.rs')
  if [[ -z $hits ]]; then
    pass no-fma
  else
    fail no-fma 'transcendentals come from stratum_core::math (libm); no mul_add anywhere (ADR-004)' "$hits"
  fi
}

BLAS_CRATES='^name = "(faer|faer-.*|blas|blas-src|blas-sys|openblas-src|openblas-system|netlib-src|cblas|cblas-sys|lapack|lapack-src|lapack-sys|lapacke|ndarray-linalg|intel-mkl-src|intel-mkl-tool)"$'
check_no_blas() {
  [[ -f Cargo.lock ]] || { skip no-blas 'Cargo.lock does not exist yet'; return; }
  checks=$((checks + 1))
  local hits
  hits=$(grep -En "$BLAS_CRATES" Cargo.lock)
  if [[ -z $hits ]]; then
    pass no-blas
  else
    fail no-blas 'the kernels are hand-written and deterministic; no BLAS/LAPACK/MKL in the tree (ADR-004)' "$hits"
  fi
}

# ---------------------------------------------------------------------------
# §8.14 — stratum-graph does no path resolution (A14)
# ---------------------------------------------------------------------------
# Scheme colours come from `stratum_tokens::SCHEMES`, compiled in. A graph must
# render identically in the CLI, in a headless conformance run and in the app,
# and it cannot do that if it reads a file.
check_graph_no_io() {
  local dir=crates/stratum-graph/src
  [[ -d $dir ]] || { skip graph-no-io "$dir does not exist yet"; return; }
  checks=$((checks + 1))
  local hits
  hits=$(scan -e 'std::fs|Utf8Path|include_str!|include_bytes!' "$dir" 2>/dev/null | code_lines)
  if [[ -z $hits ]]; then
    pass graph-no-io
  else
    fail graph-no-io 'stratum-graph resolves no paths and reads no file (ARCHITECTURE §8.14)' "$hits"
  fi
}

# ---------------------------------------------------------------------------
# R0 — a declared cargo target with no file takes the whole workspace down
# ---------------------------------------------------------------------------
# `members = ["crates/*", ...]` is a glob on purpose: the root Cargo.toml header
# explains that it is what lets a unit register its crate without editing a file
# it does not own, which is what makes R0 enforceable at all. The price is that
# ONE unparseable member manifest fails `cargo metadata`, and `cargo metadata` is
# the first thing every cargo command does — so a single crate mid-edit stops
# all 24 others from building, testing or even being enumerated. Cargo names the
# manifest and nothing else, so what every other unit sees is "cargo is broken".
#
# That is not hypothetical and it is not rare. In one session `stratum-intel`,
# `stratum-platform-linux` and `stratum-platform-windows` each blocked the whole
# repository by committing a `[package]` before any source file ("no targets
# specified in the manifest"), and `stratum-exec` blocked it again — for forty
# minutes and roughly twenty-five retries by a unit with no way to fix it — by
# declaring `[[bench]] staleness` with no `benches/staleness.rs`.
#
# This check parses the manifests as text and needs no cargo, which is the point:
# it is the one check that still runs when cargo is the thing that is broken. It
# names the crate, the missing file, and the unit that owes it under R0.
#
# It cannot replace cargo's own error, and does not try to: cargo is the
# authority on target resolution. It exists so that the answer takes one second
# and arrives with an owner attached.

# One `kind<TAB>name<TAB>path` line per EXPLICITLY declared target. Auto-
# discovered targets are cargo's business and never fail this way.
declared_targets() {
  awk '
    function value(l,   v) {
      v = l
      sub(/^[^=]*=[ \t]*/, "", v)
      sub(/^"/, "", v); sub(/"[ \t]*$/, "", v)
      return v
    }
    function flush() {
      if (kind != "") printf "%s\t%s\t%s\n", kind, name, path
      kind = ""; name = ""; path = ""
    }
    { line = $0; sub(/[ \t]*#.*$/, "", line) }
    line ~ /^[ \t]*\[\[(bin|bench|test|example)\]\]/ {
      flush()
      kind = line
      sub(/^[ \t]*\[\[/, "", kind); sub(/\]\].*$/, "", kind)
      next
    }
    line ~ /^[ \t]*\[lib\][ \t]*$/ { flush(); kind = "lib"; next }
    line ~ /^[ \t]*\[/            { flush(); next }
    kind != "" && line ~ /^[ \t]*name[ \t]*=/ { name = value(line); next }
    kind != "" && line ~ /^[ \t]*path[ \t]*=/ { path = value(line); next }
    END { flush() }
  ' "$1"
}

# Best-effort R0 owner for a crate directory. `cargo xtask ownership` is the
# authority and this is not it — but that tool is a cargo command, so on exactly
# the tree this check is for, it cannot run.
owner_of() {
  [[ -f docs/ownership.toml ]] || return 0
  awk -v dir="$1" '
    /^id[ \t]*=/ { id = $0; sub(/^[^"]*"/, "", id); sub(/".*$/, "", id); next }
    /^[ \t]*"/ {
      s = $0; gsub(/[ \t",]/, "", s)
      if (index(s, dir "/") == 1) { print id; exit }
    }
  ' docs/ownership.toml
}

check_manifest_targets() {
  [[ -d crates ]] || { skip manifest-targets 'crates/ is empty'; return; }
  checks=$((checks + 1))
  local hits='' manifest dir kind name path owner where
  while IFS= read -r manifest; do
    [[ -f $manifest ]] || continue
    grep -qE '^[ \t]*\[package\]' "$manifest" || continue   # virtual manifests
    dir=${manifest%/Cargo.toml}
    owner=$(owner_of "$dir")
    where="${owner:-owner unknown}"

    local explicit=0
    while IFS=$'\t' read -r kind name path; do
      [[ -n $kind ]] || continue
      [[ $kind == lib || $kind == bin ]] && explicit=1
      if [[ -n $path ]]; then
        [[ -f "$dir/$path" ]] ||
          hits+="$manifest declares [$kind] '$name' at $path — $dir/$path does not exist  [$where]"$'\n'
        continue
      fi
      case $kind in
        lib) [[ -f "$dir/src/lib.rs" ]] ||
          hits+="$manifest declares [lib] — $dir/src/lib.rs does not exist  [$where]"$'\n' ;;
        bin) [[ -f "$dir/src/main.rs" || -f "$dir/src/bin/$name.rs" || -f "$dir/src/bin/$name/main.rs" ]] ||
          hits+="$manifest declares [[bin]] '$name' — none of $dir/src/main.rs, $dir/src/bin/$name.rs, $dir/src/bin/$name/main.rs exists  [$where]"$'\n' ;;
        bench) [[ -f "$dir/benches/$name.rs" || -f "$dir/benches/$name/main.rs" ]] ||
          hits+="$manifest declares [[bench]] '$name' — neither $dir/benches/$name.rs nor $dir/benches/$name/main.rs exists  [$where]"$'\n' ;;
        test) [[ -f "$dir/tests/$name.rs" || -f "$dir/tests/$name/main.rs" ]] ||
          hits+="$manifest declares [[test]] '$name' — neither $dir/tests/$name.rs nor $dir/tests/$name/main.rs exists  [$where]"$'\n' ;;
        example) [[ -f "$dir/examples/$name.rs" || -f "$dir/examples/$name/main.rs" ]] ||
          hits+="$manifest declares [[example]] '$name' — neither $dir/examples/$name.rs nor $dir/examples/$name/main.rs exists  [$where]"$'\n' ;;
      esac
    done < <(declared_targets "$manifest")

    # The other half of the same outage: a `[package]` with no target of ANY
    # kind is cargo's "no targets specified in the manifest", and it blocks the
    # workspace just as completely. This is what a manifest written before its
    # first source file looks like — the shape `stratum-intel`,
    # `stratum-platform-linux` and `stratum-platform-windows` each shipped.
    #
    # "Of any kind" is cargo's rule and it has to be matched exactly, or this
    # becomes the cry-wolf check it exists to prevent: an auto-discovered
    # integration test is a target, so a crate with `tests/` and no `src/lib.rs`
    # is a package cargo loads happily. Verified against cargo 1.96 both ways.
    if ((!explicit)) && [[ ! -f "$dir/src/lib.rs" && ! -f "$dir/src/main.rs" ]] &&
      ! compgen -G "$dir/src/bin/*.rs" >/dev/null &&
      ! compgen -G "$dir/tests/*.rs" >/dev/null &&
      ! compgen -G "$dir/benches/*.rs" >/dev/null &&
      ! compgen -G "$dir/examples/*.rs" >/dev/null; then
      hits+="$manifest has no targets of any kind — no src/lib.rs, no src/main.rs, no src/bin/*.rs, no tests|benches|examples/*.rs, and no [lib]/[[bin]] path  [$where]"$'\n'
    fi
  done < <(ls -1 crates/*/Cargo.toml apps/*/src-tauri/Cargo.toml xtask/Cargo.toml 2>/dev/null)

  hits=${hits%$'\n'}
  if [[ -z $hits ]]; then
    pass manifest-targets
  else
    fail manifest-targets \
      'a declared cargo target with no file fails `cargo metadata` for every crate in the workspace (R0: the owner named in brackets fixes it, nobody else)' \
      "$hits"
  fi
}

check_tauri_bridge
check_stata_free_ci
check_missing_values
check_number_format
check_no_fma
check_no_blas
check_graph_no_io
check_manifest_targets

echo
# `${arr[@]}` on an empty array is an unbound-variable error under `set -u` in
# the bash 3.2 that ships with macOS, so both uses are guarded.
if ((${#skipped[@]:-0})); then
  echo "skipped ${#skipped[@]} check(s), target not present yet:"
  printf '  %s\n' "${skipped[@]+"${skipped[@]}"}"
fi

if ((failures)); then
  red "check-topology: $failures of $checks check(s) failed"
  exit 1
fi
echo "check-topology: OK — $checks check(s) passed"
