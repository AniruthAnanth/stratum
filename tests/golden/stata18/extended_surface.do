* Golden reference: the command surface beyond core_surface.do.
* Estimation variants, data management, dates, and by-group edge cases.
set more off
set linesize 100

sysuse auto, clear

di "===== regress noconstant ====="
regress price mpg weight, noconstant

di "===== regress level(90) ====="
regress price mpg weight, level(90)

di "===== regress beta ====="
regress price mpg weight, beta

di "===== regress vce(cluster) ====="
regress price mpg weight, vce(cluster rep78)

di "===== regress vce(robust) ereturn ====="
quietly regress price mpg weight, robust
ereturn list

di "===== regress single regressor ====="
regress price mpg

di "===== regress perfect fit ====="
gen exact = 2*mpg + 3
capture noisily regress exact mpg
di "rc=" _rc

di "===== predict variants ====="
quietly regress price mpg weight
predict xb_hat
predict res, residuals
predict stdp_hat, stdp
summarize xb_hat res stdp_hat
drop xb_hat res stdp_hat exact

di "===== correlate covariance ====="
correlate price mpg weight, covariance

di "===== ttest paired ====="
gen mpg2 = mpg + 2
ttest mpg == mpg2
drop mpg2

di "===== ttest unequal ====="
ttest mpg, by(foreign) unequal

di "===== egen catalogue ====="
egen m_price   = mean(price)
egen sd_price  = sd(price)
egen md_price  = median(price)
egen mn_price  = min(price)
egen mx_price  = max(price)
egen tot_price = total(price)
egen cnt_price = count(price)
egen rank_p    = rank(price)
egen grp       = group(foreign rep78)
egen rowm      = rowmean(price mpg)
egen rowt      = rowtotal(price mpg)
egen rowmiss   = rowmiss(price rep78)
summarize m_price sd_price md_price mn_price mx_price tot_price cnt_price
summarize rank_p grp rowm rowt rowmiss
egen bygrp = mean(price), by(foreign)
summarize bygrp
drop m_price-bygrp

di "===== by-group edge cases ====="
sort foreign rep78
by foreign rep78: gen n_cell = _N
by foreign rep78: gen i_cell = _n
list foreign rep78 n_cell i_cell in 1/8
drop n_cell i_cell
bysort foreign (price): gen cheapest = price[1]
by foreign: gen dearest = price[_N]
list foreign price cheapest dearest in 1/3
drop cheapest dearest

di "===== collapse ====="
preserve
collapse (mean) price mpg (sd) sd_price=price (count) n=price, by(foreign)
list
restore

di "===== append ====="
preserve
keep make price foreign
save "part1.dta", replace
append using "part1.dta"
count
restore

di "===== merge 1:1 ====="
preserve
keep make price
gen id = _n
save "left.dta", replace
sysuse auto, clear
keep make mpg
gen id = _n
merge 1:1 id using "left.dta"
tabulate _merge
restore

di "===== merge m:1 ====="
preserve
keep foreign
duplicates drop
gen fname = cond(foreign==0, "Domestic", "Foreign")
save "flabels.dta", replace
sysuse auto, clear
merge m:1 foreign using "flabels.dta"
tabulate _merge
restore

di "===== reshape ====="
preserve
clear
set obs 3
gen id = _n
gen y1 = _n * 10
gen y2 = _n * 20
list
reshape long y, i(id) j(year)
list
reshape wide y, i(id) j(year)
list
restore

di "===== duplicates ====="
preserve
keep foreign rep78
duplicates report
duplicates list
restore

di "===== dates ====="
di %td mdy(1,1,1960)
di %td mdy(8,22,2026)
di td(22aug2026)
di %tc tc(22aug2026 13:45:00)
di year(td(22aug2026))
di month(td(22aug2026))
di day(td(22aug2026))
di dow(td(22aug2026))
di %td date("2026-08-22", "YMD")
di daysinmonth(td(22aug2026))

di "===== string to numeric ====="
preserve
clear
set obs 3
gen str10 s = "12.5" in 1
replace s = "abc" in 2
replace s = "" in 3
gen n = real(s)
list s n
destring s, gen(sn) force
list s sn
restore

di "===== encode / decode ====="
preserve
keep make foreign
gen str8 grp = cond(foreign==0, "Dom", "For")
encode grp, gen(grpn)
describe grpn
tabulate grpn
decode grpn, gen(grpback)
list grp grpn grpback in 1/3
restore

di "===== recode ====="
preserve
recode rep78 (1 2 = 1 "Low") (3 = 2 "Mid") (4 5 = 3 "High"), gen(rep3)
tabulate rep3
restore

di "===== inlist inrange ====="
count if inlist(rep78, 1, 2)
count if inrange(mpg, 20, 30)
count if !missing(rep78) & inlist(foreign, 1)

di "===== gsort ====="
gsort -price
list make price in 1/3
gsort price
list make price in 1/3

di "===== scalar and matrix ====="
scalar x = 42
di x
scalar y = x * 2
di y
matrix A = (1, 2 \ 3, 4)
matrix list A
matrix B = A * A
matrix list B
di A[1,2]
quietly regress price mpg weight
matrix b = e(b)
matrix list b
di b[1,1]

* The merge/append sections write intermediates into this directory; erase them
* so re-capturing leaves the tree exactly as it found it.
capture erase "part1.dta"
capture erase "left.dta"
capture erase "flabels.dta"

di "===== END ====="
