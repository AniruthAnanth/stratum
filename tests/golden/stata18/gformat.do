* Golden reference: the %g corpus (Q3). Each row prints the input's exact bit
* pattern via %21x alongside its rendering, so the rule is reconstructable
* without guessing what value was fed in.
set more off
set linesize 240

program define g4
    * `0' is the whole argument line; `args' would split "(10^3 - 1)" into tokens.
    local v `0'
    di %21x (`v') "|" %9.0g (`v') "|" %10.0g (`v') "|" %12.0g (`v') "|" %16.0g (`v') "|" %21.0g (`v') "|"
end

di "BITS|w9|w10|w12|w16|w21|"

* --- powers of ten, both directions: the fixed/scientific boundary at each width
foreach e of numlist -20/20 {
    g4 1e`e'
}

* --- just under and just over each power of ten (rounding carry cases)
foreach e of numlist 0/12 {
    g4 (10^`e' - 1)
    g4 (10^`e' + 1)
}

* --- nines: where rounding carries into a new digit
g4 0.9
g4 0.99
g4 0.999
g4 0.9999
g4 0.99999
g4 0.999999
g4 0.9999999
g4 0.99999999
g4 0.999999999
g4 9.9999999
g4 99.999999
g4 999999.99
g4 9999999.9

* --- repeating and irrational
g4 (1/3)
g4 (2/3)
g4 (1/7)
g4 (22/7)
g4 _pi
g4 exp(1)
g4 sqrt(2)
g4 (1/0.7)

* --- negatives mirror positives
g4 (-1/3)
g4 -0.000123456
g4 -12345.6789
g4 -1e-20
g4 -1e20

* --- values straight out of the golden summarize/regress tables
foreach v in 6165.257 2949.496 21.2973 5.785503 3019.459 777.1936 ///
             -49.51222 86.15604 1.746559 .6413538 1946.069 3597.05 ///
             .4995593889723035 2130.769528589715 23.29224584461624 {
    g4 `v'
}

* --- subnormals, extremes, and the missing sentinels
g4 1e-308
g4 4.9e-324
g4 1.7976931348623157e308
g4 0
g4 (-0)

di "MISSING|" %9.0g . "|" %10.0g . "|" %12.0g . "|"
di "MISSING_A|" %9.0g .a "|" %10.0g .a "|" %12.0g .a "|"
di "MISSING_Z|" %9.0g .z "|" %10.0g .z "|" %12.0g .z "|"

* --- other format families for contrast, same values
di ""
di "===== %f family ====="
foreach v in 3.14159265 1234.5678 0.000123456 -99.995 {
    di %21x (`v') "|" %9.2f (`v') "|" %12.4f (`v') "|" %6.1f (`v') "|" %9.0f (`v') "|"
}
di ""
di "===== %e family ====="
foreach v in 3.14159265 1234.5678 0.000123456 1e100 {
    di %21x (`v') "|" %9.3e (`v') "|" %12.6e (`v') "|" %8.0e (`v') "|"
}
di ""
di "===== %gc comma-grouped ====="
foreach v in 1234 12345 1234567 1234567.89 -1234567 {
    di %21x (`v') "|" %12.0gc (`v') "|" %14.2fc (`v') "|"
}
di ""
di "===== default display of a bare expression ====="
di 1/3
di 2/3
di 1e20
di 1e-20
di 12345678901234567890
di .1 + .2

di "===== END ====="
