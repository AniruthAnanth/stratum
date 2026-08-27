* Difftest prologue — run before EVERY case, on every machine (plan W23).
*
* Pins every setting that could perturb the comparison, so a capture from a
* colleague's machine and one from a future licensed box are comparable to
* the last ulp. `scripts/run-stata.sh` places each case in its OWN temp cwd
* with this file, `ado/stratum_capture.ado` and `case.do` beside it, and a
* driver that runs `do prologue.do` then `do case.do`.
version 18
clear all
set more off

* linesize 80 is the classic-output contract (C44/A16: any other value is a
* hard error in our runtime, so 80 is the only width a comparison can use).
set linesize 80

* MP parallel reduction order perturbs the last ulp of sums, which is exactly
* the digit %21.17g exists to capture. One processor, always. `capture`
* because Stata/SE and BE have nothing to set — there the point is moot.
capture set processors 1

* Deterministic RNG state for any case that samples.
set seed 12345

* The capture emitter travels with the harness, never with the machine.
adopath ++ "ado"
