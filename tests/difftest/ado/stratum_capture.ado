*! stratum_capture — CONTRACTS §9's CaptureRecord, emitted from INSIDE Stata.
*!
*! This is the Stata half of the differential contract: `stratum run
*! --capture` (once W09's engine edge lands; the stats-crate regeneration in
*! `stratum-difftest` until then) emits the identical NDJSON schema, so the
*! comparison is between two files in one format, never between a format and
*! a screen-scrape.
*!
*!     stratum_capture e using "capture.jsonl", replace
*!     stratum_capture r using "capture.jsonl", append
*!
*! One JSON object per line. Numerics are written as %21.17g STRINGS —
*! seventeen significant digits round-trip any double exactly — and are
*! parsed to f64 only at compare time, on the Rust side. Missing values
*! render as their code (".", ".a" … ".z"): string(., "%21.17g") already does
*! this, and the comparator matches them BY CODE, never by tolerance.
*!
*! Record shapes (serde: tag "kind", snake_case):
*!   {"kind":"scalar","name":"e(N)","value":"74"}
*!   {"kind":"macro","name":"e(cmd)","value":"regress"}
*!   {"kind":"matrix","name":"e(V)","rows":3,"cols":3,
*!    "rownames":[...],"colnames":[...]}
*!   {"kind":"coef","name":"mpg","value":"-49.512221457826422"}      <- e(b)
*!   {"kind":"cell","name":"e(V)[mpg,weight]","value":"..."}         <- others
*!
*! Line order does not matter to the comparator (records are keyed); a
*! capture that is to be COMMITTED must afterwards be canonicalized with
*! `stratum-difftest canon <file>` so re-capture diffs are stable. The lint
*! (`stratum-difftest lint`, wrapped by `cargo xtask goldens --lint`)
*! enforces that.
program define stratum_capture
    version 18
    gettoken class 0 : 0
    if !inlist(`"`class'"', "e", "r") {
        di as error "stratum_capture: first token must be e or r"
        exit 198
    }
    syntax using/ [, APPend REPlace]
    tempname h
    if "`append'" != "" {
        file open `h' using `"`using'"', write text append
    }
    else {
        file open `h' using `"`using'"', write text replace
    }

    * The order below never runs an `class'-clobbering command before the
    * corresponding reads: file/gettoken/syntax are neither e- nor r-class.
    local sc : `class'(scalars)
    foreach s of local sc {
        local v = string(`class'(`s'), "%21.17g")
        file write `h' `"{"kind":"scalar","name":"`class'(`s')","value":"`v'"}"' _n
    }
    local mc : `class'(macros)
    foreach m of local mc {
        local v `"``class'(`m')'"'
        * JSON escaping: backslash first, then double quote.
        local v : subinstr local v `"\"' `"\\"', all
        local v : subinstr local v `"""' `"\""', all
        file write `h' `"{"kind":"macro","name":"`class'(`m')","value":"`v'"}"' _n
    }
    local mx : `class'(matrices)
    foreach m of local mx {
        tempname M
        matrix `M' = `class'(`m')
        local rows = rowsof(`M')
        local cols = colsof(`M')
        local rn : rownames `M'
        local cn : colnames `M'
        local rj ""
        foreach r of local rn {
            local rj `"`rj'"`r'","'
        }
        local rj = substr(`"`rj'"', 1, length(`"`rj'"') - 1)
        local cj ""
        foreach c of local cn {
            local cj `"`cj'"`c'","'
        }
        local cj = substr(`"`cj'"', 1, length(`"`cj'"') - 1)
        file write `h' `"{"kind":"matrix","name":"`class'(`m')","rows":`rows',"cols":`cols',"rownames":[`rj'],"colnames":[`cj']}"' _n
        * Cells: e(b) row vectors as coef records keyed by column name (the
        * shape the Rust side emits); every other matrix cell-by-cell.
        forvalues i = 1/`rows' {
            forvalues j = 1/`cols' {
                local v = string(`M'[`i', `j'], "%21.17g")
                if "`m'" == "b" & `rows' == 1 {
                    local c : word `j' of `cn'
                    file write `h' `"{"kind":"coef","name":"`c'","value":"`v'"}"' _n
                }
                else {
                    local r : word `i' of `rn'
                    local c : word `j' of `cn'
                    file write `h' `"{"kind":"cell","name":"`class'(`m')[`r',`c']","value":"`v'"}"' _n
                }
            }
        }
    }
    file close `h'
end
