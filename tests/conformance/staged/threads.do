* STAGED — not yet part of the corpus. See ../README.md.
*
* ADR-013: the map/reduce split must not leak the thread count. This case is the
* one whose answer could plausibly depend on RAYON_NUM_THREADS — a sort with
* ties, and reductions over a column large enough to be chunked — so it is the
* case `xtask conformance --threads 1,2,8` exists to run. Nothing above it in
* this directory can currently exercise that, and the README says so.

clear
set obs 100000
set seed 20250822
gen double v = runiform()
gen long g = mod(_n, 7)
gen byte tie = 1
sort tie g
summarize v, detail
by g: summarize v
total v
