* tests/smoke/hello.do — the end-to-end case named by design 08 §4.4 and by the
* release checklist in §9.5:
*
*     stratum run tests/smoke/hello.do --json > out.jsonl
*     cargo xtask smoke assert-stream out.jsonl tests/smoke/expected.jsonl
*
* Every command below has a committed StataMP 18.5 golden in
* tests/golden/stata18/core_surface.log, so the day the engine is linked this
* file's output can be checked against Stata's own numbers rather than against
* an earlier run of ourselves. Keep it that way: a smoke case whose expected
* output has no external oracle only proves that we did the same thing twice.

// %% Load
sysuse auto, clear

// %% Describe
summarize price mpg

// %% Model
regress price mpg weight foreign
