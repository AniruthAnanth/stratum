* Golden reference capture for the v1 command surface.
* Generated against StataMP 18.5. Do not edit output by hand.
set more off
set linesize 100

sysuse auto, clear

di "===== describe ====="
describe

di "===== summarize ====="
summarize

di "===== summarize varlist ====="
summarize price mpg weight

di "===== summarize detail ====="
summarize price, detail

di "===== return list after summarize ====="
summarize mpg
return list

di "===== count ====="
count
count if foreign == 1

di "===== generate / replace ====="
gen lnprice = log(price)
gen byte hi = price > 6000
replace hi = 0 if mpg > 30
summarize lnprice hi

di "===== missing value handling ====="
summarize rep78
count if missing(rep78)
gen rep_missing = missing(rep78)
tabulate rep_missing

di "===== list ====="
list make price mpg in 1/5

di "===== tabulate oneway ====="
tabulate rep78

di "===== tabulate twoway ====="
tabulate foreign rep78, chi2

di "===== tabulate row col ====="
tabulate foreign rep78, row col

di "===== regress ====="
regress price mpg weight foreign

di "===== ereturn list after regress ====="
ereturn list

di "===== regress robust ====="
regress price mpg weight, robust

di "===== regress with collinearity ====="
gen mpg2 = mpg
regress price mpg mpg2 weight

di "===== predict ====="
quietly regress price mpg weight
predict pricehat
predict resid, residuals
summarize pricehat resid

di "===== correlate ====="
correlate price mpg weight

di "===== pwcorr ====="
pwcorr price mpg rep78, sig

di "===== ttest ====="
ttest mpg, by(foreign)

di "===== ttest onesample ====="
ttest mpg == 20

di "===== sort and by ====="
sort foreign price
by foreign: summarize price

di "===== bysort ====="
bysort foreign: gen n_in_grp = _n
list foreign n_in_grp in 1/3

di "===== macros ====="
local vars price mpg
global gname weight
summarize `vars' $gname
di "local was: `vars'"

di "===== foreach ====="
foreach v of varlist price mpg {
    quietly summarize `v'
    di "`v' mean = " r(mean)
}

di "===== forvalues ====="
forvalues i = 1/3 {
    di "iter `i'"
}

di "===== if else ====="
if 1 == 1 {
    di "true branch"
}
else {
    di "false branch"
}

di "===== program ====="
program define showmean
    quietly summarize `1'
    di "`1': " r(mean)
end
showmean price
showmean mpg

di "===== display expressions ====="
di 2 + 3 * 4
di log(exp(1))
di sqrt(16)
di round(3.14159, 0.01)
di 1/0
di .
di missing(.)
di 5 > .
di ("abc" + "def")
di length("hello")
di substr("hello", 2, 3)

di "===== drop keep ====="
preserve
drop if price > 10000
count
restore
preserve
keep make price
describe
restore

di "===== rename label ====="
rename mpg mileage
label variable mileage "Miles per gallon"
describe mileage
rename mileage mpg

di "===== egen-ish / summarize by ====="
egen meanprice = mean(price)
summarize meanprice

di "===== inspect ====="
inspect rep78

di "===== codebook ====="
codebook foreign

di "===== tabstat ====="
tabstat price mpg, statistics(mean sd min max) columns(statistics)

di "===== set seed determinism ====="
set seed 12345
gen u = runiform()
summarize u
set seed 12345
gen u2 = runiform()
assert u == u2
di "seed reproducible OK"

di "===== save and use roundtrip ====="
tempfile t
save "`t'", replace
use "`t'", clear
describe
summarize price

di "===== END ====="
