* Generate .dta fixtures covering every storage type and metadata feature.
* Run with: scripts/capture-golden.sh tests/fixtures/dta/make_fixtures.do
set more off
set linesize 100

*----------------------------------------------------------------- alltypes
clear
set obs 5
gen byte   v_byte   = _n - 3
gen int    v_int    = (_n - 3) * 1000
gen long   v_long   = (_n - 3) * 1000000
gen float  v_float  = (_n - 3) * 1.5
gen double v_double = (_n - 3) * 1.123456789012345
gen str1   v_str1   = substr("abcde", _n, 1)
gen str18  v_str18  = "row " + string(_n) + " padded"

* extended missing values across types
replace v_byte   = .  in 1
replace v_int    = .a in 2
replace v_long   = .b in 3
replace v_float  = .z in 4
replace v_double = .  in 5

label variable v_byte   "A byte variable"
label variable v_double "A double with a deliberately long variable label to test truncation behaviour"

label define yesno 0 "No" 1 "Yes" -1 "Negative" 2 "Two"
label values v_byte yesno

format v_double %12.4f
format v_str18  %-18s

label data "All storage types fixture"
notes: this fixture exercises every Stata storage type
char _dta[source] "make_fixtures.do"
char v_byte[units] "arbitrary"

save "alltypes.dta", replace
describe
list

*----------------------------------------------------------------- strl
clear
set obs 3
gen strL big = ""
replace big = "short" in 1
replace big = "a much longer string that exceeds the usual inline threshold used by Stata for strL storage, repeated to be certain: a much longer string that exceeds the usual inline threshold used by Stata for strL storage" in 2
replace big = "" in 3
gen str5 small = "abc"
label data "strL fixture"
save "strl.dta", replace
describe
list

*----------------------------------------------------------------- sorted
sysuse auto, clear
keep make price mpg foreign rep78
sort foreign price
label data "sorted subset of auto"
save "sorted.dta", replace
describe
di "sortlist should show: foreign price"

*----------------------------------------------------------------- empty
clear
set obs 0
gen byte novalues = .
label data "zero observations"
save "empty.dta", replace
describe

*----------------------------------------------------------------- wide-ish
clear
set obs 100
forvalues i = 1/40 {
    gen double x`i' = runiform() * `i'
}
label data "40 numeric variables, 100 obs"
save "wide.dta", replace
describe, short

*----------------------------------------------------------------- auto copy
sysuse auto, clear
save "auto.dta", replace
describe, short

di "===== FIXTURES DONE ====="
