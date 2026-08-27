* ---------------------------------------------------------------------------
* gen-fmt-corpus.do — capture Stata's own numeric formatter, byte for byte.
*
* Q3 / A28.  `%w.dg` is the single most commonly wrong thing in a Stata
* reimplementation and StataCorp does not publish the rule, so we do not guess
* it: this file drives the locally licensed Stata 18.5 and freezes ~10^5 values
* x ~40 formats of measured (value, format) -> string cells into
* tests/data/fmt_corpus.jsonl, which `crates/stratum-core/tests/fmt_corpus.rs`
* then replays on a machine with no Stata on it.  Spec §32: this script is NOT
* part of the build.
*
* Run once, from the repository root, on a machine with a Stata license:
*
*     stata-mp -b do scripts/gen-fmt-corpus.do
*
* WHY `file write %fmt (exp)` AND NOT `mata: strofreal(x, fmt)`
* -----------------------------------------------------------------
* `strofreal` returns the formatter's *body* with the justification stripped,
* so it cannot see a width bug — measured here, it disagrees with `display` on
* 1237 of 2200 sample cells.  `file write` with a format writes exactly what
* `display` would put on the screen, padding included, which is the string the
* Data Editor and every classic renderer must match; that equivalence is
* measured too (3600 numeric + 4000 date cells, zero differences) and it is
* what lets this script emit 4.8M cells in one pass instead of round-tripping
* every cell through a macro.
*
* WHY THE VALUE IS TRANSPORTED AS `%21x`
* -----------------------------------------------------------------
* A decimal literal does not round-trip: writing 0.1 and reading it back is
* only the same double by luck of the parser.  `%21x` is Stata's exact
* hex-float rendering (sign, 1.<13 hex digits>, 'X', signed hex exponent), so
* the Rust side reconstructs the identical bit pattern with integer shifts and
* no decimal conversion at all.
*
* WHY SOME CELLS ARE `null`
* -----------------------------------------------------------------
* Stata 18.5's `%w.0g` is BROKEN for w >= ~21 on a double whose integer part
* needs more than 19 digits.  `di %24.0g 1e20` is `1000000000000000000` — 1e18,
* two decades wrong — and on a different run of the same binary the same call
* emits `1000000999999999836.` followed by two bytes of uninitialised memory,
* so the cell is not even stable between runs.  Measured over three runs on
* 2026-08-22, StataMP 18.5, aarch64-apple-darwin.
*
* A golden corpus cannot contain a nondeterministic cell, and bug-compatibility
* with a buffer overrun is not a thing that can be implemented, so those cells
* are written as JSON `null` and fmt_corpus.rs skips them by count.  The guard
* is not a magnitude rule (it would over-reject): every value at or above 2^53
* is an exact integer, so each of its FIXED-branch cells is compared against
* `%40.0f`, which is stable and correct, and only the cells that disagree are
* dropped.  Scientific-branch cells at the same magnitudes are unaffected and
* stay in.
*
* WHY NOTHING HERE REACHES 8.988e307
* -----------------------------------------------------------------
* Invariant M (CONTRACTS §13.1): no double at or above SYSMISS other than the
* 27 sentinels can exist in the engine.  Stata's `display` does not enforce it
* and walks off the end of its own tag table up there, so every generated block
* is bounded below 1e300 and the single out-of-domain probe (1e308) stays a
* hand-picked constant.  Widening the random blocks past SYSMISS would only
* add cells that compare a formatter against inputs it can never be handed.
* ---------------------------------------------------------------------------

version 18
clear all
set more off
set linesize 250
set seed 20260821

local out "tests/data/fmt_corpus.jsonl"

* --- group 0: the general numeric formats -----------------------------------
* Widths 1..14 of %w.0g are the load-bearing ones (every classic renderer and
* every default display format lives there); the wide ones pin the significant
* digit cap; the rest cover each branch of the grammar exactly once.
local FMT0 "%1.0g %2.0g %3.0g %4.0g %5.0g %6.0g %7.0g %8.0g %9.0g %10.0g %11.0g %12.0g %13.0g %14.0g %16.0g %18.0g %20.0g %24.0g %30.0g %9.2g %9.5g %12.5g %20.10g %6.5g %11.3g %8.0gc %9.0gc %12.0gc %16.0gc %20.0gc %9.0f %9.2f %12.4f %20.18f %6.1f %3.0f %10.3f %12.2fc %20.0fc %15.2fc %8.0e %12.4e %9.2e %20.12e %-12.0g %012.2f %21x"

* --- group 1: the date/time formats -----------------------------------------
* Applied to a separate value set: %td of 1e300 is not a test of anything.
local FMT1 "%tc %tC %td %tw %tm %tq %th %ty %tg %20.0g"

* ===========================================================================
* Value set for group 0.  ~10^5 doubles, laid down in blocks that each stress
* one decision the formatter makes.  Every block is deterministic under the
* seed above.
* ===========================================================================
tempname fh

* Hand-picked structural values first: every branch boundary this formatter
* has, the 14 published goldens, and the 27 missing values.
local HAND ". .a .b .c .d .e .f .g .h .i .j .k .l .m .n .o .p .q .r .s .t .u .v .w .x .y .z"
local HAND "`HAND' 0 1 -1 0.5 -0.5 1.5 2.5 0.125 0.375 0.25 1.25 2.25 10.25 0.0625"
local HAND "`HAND' 100 -100 12345 99999999 999999999 999999999.9 100000000 12345678 123456789"
local HAND "`HAND' 1234567.891 -1234567.891 317252881.2439711 2997197234.5 4540178.784"
local HAND "`HAND' 8699525.974 2130.769528589715 0.63074906 -5853.6957 0.158902485820707"
local HAND "`HAND' 2513.9942 0.000012345 -0.000012345 0.00001 0.0000099999 0.000010001"
local HAND "`HAND' 0.000009 0.000001 0.0001 0.001 0.01 0.1 3.14159265358979 -3.14159265358979"
local HAND "`HAND' 1e15 1e16 1e17 123456789012345678 1e20 1e100 1e300 1e-300 1e-320"
local HAND "`HAND' 0.33333333333333331 0.66666666666666663 1e308 -1e308 4.9406564584125e-324"
local HAND "`HAND' 1e-5 1e-4 1e-6 9.999999999999999 99.99999999999999 0.9999999999999999"
local HAND "`HAND' 1000.5 100.5 1e6 1e7 1e8 1e9 1e10 1e13 1e14 6165.2567567567 2949.4959558824"
local HAND "`HAND' 21.2972972972973 5.7855033029552 0.1 0.2 0.3 0.7 1.1 2.2 3.3 1e-10 1e-15"

local nhand : word count `HAND'
local NTOT 100000

set obs `NTOT'
gen double v = .

local i 0
foreach h of local HAND {
    local ++i
    quietly replace v = `h' in `i'
}
local cur = `nhand'

* --- B: integers of every decimal length 1..17 ------------------------------
* `%w.0g` caps significant digits by width; an integer that needs more digits
* than the cap is where the fixed/scientific handover shows up first.
forvalues d = 1/17 {
    local a = `cur' + 1
    local cur = `cur' + 200
    local lo = 10 ^ (`d' - 1)
    local hi = 10 ^ `d' - 1
    quietly replace v = cond(runiform() < 0.5, -1, 1) * ///
        (`lo' + floor(runiform() * (`hi' - `lo' + 1))) in `a'/`cur'
}
* the exact powers and their neighbours, both signs
forvalues d = 0/17 {
    foreach off of numlist -1 0 1 {
        foreach sg of numlist -1 1 {
            local ++cur
            quietly replace v = `sg' * (10 ^ `d' + `off') in `cur'
        }
    }
}

* --- C: decade boundaries ---------------------------------------------------
* 10^e scaled by (1 +/- 10^-j) is a number that rounds INTO or OUT OF a new
* decade as the digit budget changes, which is where every %g rule that is
* wrong is wrong.  Swept over the WHOLE exponent range, not a window around
* 1: the scientific fallback's decimal count depends on how many digits the
* exponent has, so a corpus that stops at 1e25 cannot see the three-digit case.
* 305 is the last exponent where 10^e * 1.1 is still below SYSMISS.
forvalues e = -305/305 {
    forvalues j = 1/16 {
        local ++cur
        quietly replace v = (10 ^ `e') * (1 - 10 ^ (-`j')) in `cur'
        local ++cur
        quietly replace v = (10 ^ `e') * (1 + 10 ^ (-`j')) in `cur'
    }
}

* --- D: k / 10^d, where the round-half ties live ----------------------------
forvalues d = 0/15 {
    local a = `cur' + 1
    local cur = `cur' + 400
    quietly replace v = cond(runiform() < 0.5, -1, 1) * ///
        floor(runiform() * 1e9) / (10 ^ `d') in `a'/`cur'
}
* explicit halves and quarters: exactly representable ties at every scale
forvalues d = 0/12 {
    local a = `cur' + 1
    local cur = `cur' + 100
    quietly replace v = cond(runiform() < 0.5, -1, 1) * ///
        (floor(runiform() * 1e6) + 0.5) / (10 ^ `d') in `a'/`cur'
}

* --- E: the decimal exponent x mantissa grid --------------------------------
* The fixed-vs-scientific decision is a function of the decimal exponent, so
* every exponent in -30..30 gets its own block of full-entropy mantissas.
forvalues e = -30/30 {
    local a = `cur' + 1
    local cur = `cur' + 700
    quietly replace v = cond(runiform() < 0.5, -1, 1) * ///
        (1 + 9 * runiform()) * (10 ^ `e') in `a'/`cur'
}

* --- F: the binary lattice, including subnormals ----------------------------
* Decimal blocks never produce a mantissa whose low bits are adversarial for
* the digit generator; sampling 2^e * [1,2) does.  e stops at 995 because
* 2 * 2^995 = 6.6e299 < SYSMISS (see the Invariant M note at the top), and
* runs down to -1074 so the subnormal ladder is covered.
local a = `cur' + 1
local cur = `cur' + 20000
quietly replace v = cond(runiform() < 0.5, -1, 1) * (1 + runiform()) * ///
    2 ^ (floor(runiform() * 2070) - 1074) in `a'/`cur'

* --- G: 15-, 16- and 17-significant-digit decimals --------------------------
* The %g significant-digit cap and double's own 17-digit round-trip limit are
* different numbers; values that need all 17 separate them.
forvalues k = 15/17 {
    local a = `cur' + 1
    local cur = `cur' + 1500
    quietly replace v = cond(runiform() < 0.5, -1, 1) * ///
        (10 ^ (`k' - 1) + floor(runiform() * (10 ^ `k' - 10 ^ (`k' - 1)))) / ///
        (10 ^ (floor(runiform() * 31) - 15)) in `a'/`cur'
}

* --- H: fill the remainder with a broad uniform sweep -----------------------
if `cur' < `NTOT' {
    local a = `cur' + 1
    quietly replace v = cond(runiform() < 0.5, -1, 1) * ///
        (1 + 9 * runiform()) * (10 ^ (floor(runiform() * 61) - 30)) in `a'/`NTOT'
    local cur = `NTOT'
}
* If a block edit ever overruns NTOT, stop rather than silently truncate.
assert `cur' <= `NTOT'
* Only the hand-picked head may hold the 27 missing sentinels; a generated
* block that produced `.` would be a silently empty test.
assert !missing(v) in `=`nhand'+1'/`NTOT'

* Every double at or above 2^53 is an exact integer; those are the only cells
* the Stata defect guard has to look at.  Missing values are sentinels far
* above 2^53 and are not integers in any useful sense, so they are excluded.
gen byte big = abs(v) >= 9007199254740992 & !missing(v)
quietly count if big
display "group 0: `r(N)' values need the defect guard"
local nkept 0
local nnull 0

file open `fh' using "`out'", write text replace
file write `fh' `"{"note":"generated by scripts/gen-fmt-corpus.do; do not edit by hand"}"' _n
file write `fh' `"{"group":0,"formats":["' _n(0)
local first 1
foreach f of local FMT0 {
    if !`first' file write `fh' ","
    file write `fh' `""`f'""'
    local first 0
}
file write `fh' "]}" _n

* `file write %fmt (exp)` applies the format itself — see the header note on
* why that is byte-equal to `display` and what it costs.  The value is still
* round-tripped through a macro for `x` and for the defect guard, because a
* compound-quoted literal may not END in a double quote and because the guard
* has to read the cell before deciding whether to keep it.
scalar QT = char(34)

* %21x is exempt from the guard: its body is `1.<hex>X<exp>`, which is not a
* decimal integer and would trip the integer comparison on every value.
forvalues i = 1/`NTOT' {
    local hx : display %21x v[`i']
    file write `fh' `"{"g":0,"x":"`hx'","s":["'
    local k 0
    if big[`i'] {
        * exact integer value, stable and correct at every magnitude here
        local exact : display %40.0f v[`i']
        local exact = subinstr("`exact'", " ", "", .)
    }
    foreach f of local FMT0 {
        local ++k
        if `k' > 1 file write `fh' ","
        local keep 1
        if big[`i'] & "`f'" != "%21x" {
            local s : display `f' v[`i']
            local s = subinstr(subinstr("`s'", " ", "", .), ",", "", .)
            if strpos("`s'", "e") == 0 & strpos("`s'", "E") == 0 {
                local ip = cond(strpos("`s'", ".") > 0, ///
                                substr("`s'", 1, strpos("`s'", ".") - 1), "`s'")
                local fp = cond(strpos("`s'", ".") > 0, ///
                                substr("`s'", strpos("`s'", ".") + 1, .), "")
                * the value is an exact integer, so a fixed cell must be that
                * integer with an all-zero fractional part and nothing else
                if "`ip'" != "`exact'" local keep 0
                if subinstr("`fp'", "0", "", .) != "" local keep 0
            }
        }
        if `keep' {
            file write `fh' (QT)
            file write `fh' `f' (v[`i'])
            file write `fh' (QT)
            local ++nkept
        }
        else {
            file write `fh' "null"
            local ++nnull
        }
    }
    file write `fh' "]}" _n
}
display "group 0: `nkept' cells kept, `nnull' dropped to null"

* ===========================================================================
* Value set for group 1 — dates.  Stata's epoch is 1960-01-01; %tc counts
* MILLISECONDS, so the interesting values are enormous and the leap-second
* variant %tC diverges from %tc only after 1972.
* ===========================================================================
clear
local DHAND ". .a .z 0 1 -1 22000 -22000 365 366 -365 730 21916 21915 43829"
local DHAND "`DHAND' 1e9 -1e9 86400000 1e12 1.5e12 -1.5e12 2e12 378691200000 1262304000000"
local DHAND "`DHAND' 730 100 -100 1000 -1000 10000 -10000 6939 6940 25567 18262"
local DHAND "`DHAND' 0.5 1.5 -0.5 999.9 1e15 -1e15 2932896 -46751"

local ndhand : word count `DHAND'
local NDTOT 12000
set obs `NDTOT'
gen double v = .
local i 0
foreach h of local DHAND {
    local ++i
    quietly replace v = `h' in `i'
}
local dcur = `ndhand'

* --- leap seconds -----------------------------------------------------------
* %tC is %tc plus the UTC leap-second table; the two agree until 1972 and then
* separate by one second per insertion.  A random millisecond sweep would hit
* an insertion instant with probability ~0, so the instants are enumerated:
* every possible insertion point is 23:59:5x on 30jun or 31dec.
forvalues y = 1970/2024 {
    foreach dm in "30jun" "31dec" {
        forvalues s = 55/59 {
            local ++dcur
            quietly replace v = clock("`dm'`y' 23:59:`s'", "DMYhms") in `dcur'
            local ++dcur
            quietly replace v = clock("`dm'`y' 23:59:`s'", "DMYhms") + 500 in `dcur'
        }
        local ++dcur
        quietly replace v = clock("`dm'`y' 23:59:59", "DMYhms") + 1000 in `dcur'
    }
}

* --- leap-second instants, on the %tC axis -----------------------------------
* The instants above are on the %tc axis, where a leap second does not exist;
* landing inside one on the %tC axis takes the accumulated offset into account.
* The i-th leap second BEGINS at (midnight ending its day) + i seconds, so
* these are the only 27 windows in which Stata prints `:60`, and the offsets
* step across each window's edges.
local LEAPDAY "01jul1972 01jan1973 01jan1974 01jan1975 01jan1976 01jan1977 01jan1978 01jan1979 01jan1980 01jul1981 01jul1982 01jul1983 01jul1985 01jan1988 01jan1990 01jan1991 01jul1992 01jul1993 01jul1994 01jan1996 01jul1997 01jan1999 01jan2006 01jan2009 01jul2012 01jul2015 01jan2017"
local li 0
foreach ld of local LEAPDAY {
    local base = clock("`ld' 00:00:00", "DMYhms") + `li' * 1000
    foreach off of numlist -1500 -1000 -500 -1 0 1 500 999 1000 1500 {
        local ++dcur
        quietly replace v = `base' + `off' in `dcur'
    }
    local ++li
}

* --- calendar boundaries ----------------------------------------------------
* Leap years, century rules, and the day before/after each year boundary: the
* Hinnant days_from_civil conversion in fmt/datetime.rs is exactly what these
* catch when it is off by one.
forvalues y = 1800/2200 {
    local ++dcur
    quietly replace v = mdy(1, 1, `y') in `dcur'
    local ++dcur
    quietly replace v = mdy(12, 31, `y') in `dcur'
    local ++dcur
    quietly replace v = mdy(2, 28, `y') + 1 in `dcur'
}

* --- day-scale, week/month/quarter/half boundaries --------------------------
local a = `dcur' + 1
local dcur = `dcur' + 2000
quietly replace v = round((runiform() - 0.5) * 200000) in `a'/`dcur'
* millisecond scale, spanning the whole %tc range Stata renders
local a = `dcur' + 1
local dcur = `dcur' + 2000
quietly replace v = round((runiform() - 0.5) * 4e12) in `a'/`dcur'
* small integers: the epoch neighbourhood, where every unit's sign flips
local a = `dcur' + 1
local dcur = `dcur' + 1000
quietly replace v = round((runiform() - 0.5) * 4000) in `a'/`dcur'
* non-integral values: Stata floors toward -inf, and that is testable
local a = `dcur' + 1
local dcur = `dcur' + 1000
quietly replace v = (runiform() - 0.5) * 1e7 in `a'/`dcur'
if `dcur' < `NDTOT' {
    local a = `dcur' + 1
    quietly replace v = round((runiform() - 0.5) * 6e12) in `a'/`NDTOT'
    local dcur = `NDTOT'
}
assert `dcur' <= `NDTOT'
assert !missing(v) in `=`ndhand'+1'/`NDTOT'
* Group 1 carries no defect guard; assert the reason rather than assuming it.
assert abs(v) < 9007199254740992 if !missing(v)

file write `fh' `"{"group":1,"formats":["' _n(0)
local first 1
foreach f of local FMT1 {
    if !`first' file write `fh' ","
    file write `fh' `""`f'""'
    local first 0
}
file write `fh' "]}" _n

* No defect guard here: every group-1 value is below 2^53 by construction, so
* the wide-%g overrun documented in the header cannot be reached.
forvalues i = 1/`NDTOT' {
    local hx : display %21x v[`i']
    file write `fh' `"{"g":1,"x":"`hx'","s":["'
    local k 0
    foreach f of local FMT1 {
        local ++k
        if `k' > 1 file write `fh' ","
        file write `fh' (QT)
        file write `fh' `f' (v[`i'])
        file write `fh' (QT)
    }
    file write `fh' "]}" _n
}

file close `fh'
display "wrote `out': `NTOT' group-0 values, `NDTOT' group-1 values"
