# Golden reference output

`stata18/` holds output captured from a **real licensed StataMP 18.5** on
macOS (Apple Silicon). These files are the accuracy oracle for the runtime:
both the numeric results and the exact text layout of classic output.

**The licence on the capture machine expired 2026-08-22.** Until a licensed
Stata exists again (a future machine, or a colleague's), these logs cannot be
regenerated and are treated as immutable — including the `[redacted]` banner
lines, which the difftest normalizer tolerates by design. Where a licence
exists, regenerate with:

```bash
scripts/capture-golden.sh tests/golden/stata18/core_surface.do
```

The differential harness that consumes this corpus is `stratum-difftest`
(plan W23): `cargo xtask difftest --corpus` re-derives our side fresh on
every run and compares it against these logs byte-for-byte (classic text)
and per-class (`return list`/`ereturn list` values, 05 §17.3) — no Stata
needed. `cargo xtask difftest` additionally runs `tests/difftest/cases/**`
through a live Stata where one is usable, and exits 77 (SKIP) where not.

Rules:

- **Never hand-edit a `.log` file.** It is machine output. If it looks wrong,
  fix the `.do` and re-capture.
- The normal build and normal CI **must never require Stata**. Differential
  testing is a separate opt-in job (product spec section 32).
- The `.log` files contain the Stata banner, license text and absolute paths.
  The differential harness normalizes those away before comparing.

Reference environment: StataNow 18.5, MP (4-core), macOS 25.5 arm64,
`auto.dta` release-118 (74 obs, 12 vars, LSF byte order).
