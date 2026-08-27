* difftest case: tab2_chi2 (plan W23, spec §32).
*
* Runs INSIDE a licensed Stata via scripts/run-stata.sh, after
* tests/difftest/prologue.do has pinned linesize 80, one processor and the
* seed. The trailing stratum_capture writes the r() state as CaptureRecord
* NDJSON; the harness regenerates our side fresh and compares per-class
* (docs/design/05-statistics.md §17.3). A capture that is to be committed
* under golden/ must be canonicalized first: `stratum-difftest canon`.
sysuse auto, clear
tabulate foreign rep78, chi2
stratum_capture r using "capture.jsonl", replace
