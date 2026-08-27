* STAGED — not yet part of the corpus. See ../README.md.
*
* The spine of tests/golden/stata18/core_surface.log: every command here has
* StataMP 18.5 output committed against it, so when the engine is linked this
* case checks fidelity and cross-platform byte-identity in the same run.

sysuse auto, clear
describe
summarize
summarize price mpg weight
summarize price, detail
count
count if foreign == 1
gen lnprice = log(price)
gen byte hi = price > 6000
replace hi = 0 if mpg > 30
summarize lnprice hi
regress price mpg weight foreign
