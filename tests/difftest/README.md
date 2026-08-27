# Stata differential-test cases (plan W23, spec §32)

The live half of the differential harness. Nothing here runs in the normal
build or normal CI — a licensed Stata is required, and where none is usable
`cargo xtask difftest` exits **77 (SKIP)**, which `stata-diff.yml` reports as
neutral. The Stata-free half (the committed-corpus comparison against
`tests/golden/stata18/*.log`) lives in `crates/stratum-difftest` and runs in
ordinary `cargo test --workspace`.

## Layout

```
prologue.do               pins linesize 80, `set processors 1`, the seed —
                          run before every case, on every machine
ado/stratum_capture.ado   emits r()/e() as CaptureRecord NDJSON (CONTRACTS §9):
                          %21.17g strings, parsed to f64 only at compare time
normalize.rules           volatile-line patterns (banner incl. `[redacted]`,
                          paths, timestamps) for whole-log comparisons
cases/<name>/case.do      one command under test; <name> matches the corpus
                          case the harness regenerates for our side
cases/<name>/golden/      OPTIONAL committed Stata output for the case:
                          stata.jsonl (canonical form) and stata.log.
                          ONLY Stata's output is ever committed here — ours
                          is regenerated every run, so a regression can never
                          be blessed into the repository.
```

## Running a case by hand on a licensed machine

```bash
cargo xtask difftest --live --case regress_ols
```

Each case runs in its own temp cwd (batch mode drops the log in `$PWD`), the
return code is parsed from the log's `r(NNN);` line — `stata -b` exits 0
unconditionally and is never trusted — and the fresh `capture.jsonl` is
compared record-by-record under the tolerances of
`docs/design/05-statistics.md` §17.3 (missing values by code, never by
tolerance).

Before committing a capture under `golden/`, canonicalize it so re-capture
diffs are stable, and lint:

```bash
cargo run -p stratum-difftest -- canon cases/<name>/golden/stata.jsonl
cargo xtask difftest --lint     # 256 KB ceiling + canonical order
```
