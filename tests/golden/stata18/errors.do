* Golden reference: exact error messages and return codes.
* `capture noisily` prints the message and lets the do-file continue.
set more off
set linesize 100

program define try
    di ""
    di `"----- `0'"'
    capture noisily `0'
    di "rc = " _rc
end

sysuse auto, clear

try summarize nosuchvar
try summarize incom
try regress price nosuchvar
try gen price = 1
try gen = 1
try replace nosuchvar = 1
try drop nosuchvar
try rename nosuchvar other
try use /no/such/file.dta, clear
try list in 999
try list in 0
try count if nosuchvar > 1
try gen x = "text" + 1
try gen byte b = 500
try regress price
try regress
try tabulate nosuchvar
try summarize price, nosuchoption
try summarize price, detial
try foo bar baz
try sort nosuchvar
try merge 1:1 nosuchvar using nofile.dta
try predict yhat
try replace price = 1 if nosuchvar
try egen z = nosuchfunc(price)
try summarize price in 1/999
try recode price (1=2)
try destring make, replace
try encode price, gen(newv)
try label values price nosuchlabel
try describe nosuchvar
try codebook nosuchvar
try correlate price nosuchvar
try ttest price
try tabstat nosuchvar

di ""
di "----- capture semantics"
capture summarize nosuchvar
di "captured rc = " _rc
capture summarize price
di "after success rc = " _rc

di ""
di "----- assert failure"
capture noisily assert price > 100000
di "rc = " _rc

di ""
di "----- confirm"
capture noisily confirm variable price
di "rc = " _rc
capture noisily confirm variable nosuchvar
di "rc = " _rc
capture noisily confirm numeric variable make
di "rc = " _rc
capture noisily confirm new variable price
di "rc = " _rc

di ""
di "----- error inside loop propagates"
capture noisily {
    foreach v of varlist price nosuchvar {
        summarize `v'
    }
}
di "rc = " _rc

di ""
di "===== END ====="
