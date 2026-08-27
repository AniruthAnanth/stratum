* Golden reference: the semantics that are easiest to get subtly wrong.
* Missing-value encoding and ordering, type promotion, precision, varlists.
set more off
set linesize 100

di "===== extended missing ordering ====="
clear
set obs 8
gen double x = .
replace x = 1      in 1
replace x = 100    in 2
replace x = -50    in 3
replace x = .      in 4
replace x = .a     in 5
replace x = .b     in 6
replace x = .z     in 7
replace x = 0      in 8
list x
di "sort ascending:"
sort x
list x
di "counts:"
count if x < .
count if missing(x)
count if x >= .
di "is .a > . ? " (.a > .)
di "is .z > .a ? " (.z > .a)
di "is . > 1e300 ? " (. > 1e300)

di "===== missing propagation in arithmetic ====="
di 1 + .
di 1 * .
di . / .
di max(1, .)
di min(1, .)
di sum(1)
di cond(1 > ., 10, 20)
di missing(.a)
di 2 == .

di "===== division and edge numerics ====="
di 1/0
di -1/0
di 0/0
di sqrt(-1)
di log(0)
di log(-1)
di exp(1000)

di "===== type promotion and storage ====="
clear
set obs 3
gen byte b = 100
gen int i = 30000
gen long l = 2000000
gen float f = 1.5
gen double d = 1.5
describe
di "byte overflow:"
capture replace b = 200
di "rc=" _rc
gen byte b2 = 200
describe b2

di "===== float vs double precision ====="
clear
set obs 1
gen float ff = 1.1
gen double dd = 1.1
di %21x ff
di %21x dd
di ff == 1.1
di dd == 1.1
di float(1.1) == ff
format ff %20.18f
format dd %20.18f
list ff dd

di "===== integer display and rounding ====="
di 0.1 + 0.2
di (0.1 + 0.2) == 0.3
di round(2.5)
di round(3.5)
di round(-2.5)
di int(2.9)
di int(-2.9)
di ceil(2.1)
di floor(-2.1)
di mod(7, 3)
di mod(-7, 3)

di "===== varlist expansion ====="
sysuse auto, clear
di "wildcard:"
summarize m*
di "range:"
summarize price-rep78
di "abbreviation:"
summarize pri
di "_all count:"
ds
di "negated / multiple:"
summarize price mpg weight

di "===== if and in interaction ====="
count if mpg > 20
count if mpg > 20 & !missing(rep78)
count in 1/10
count if foreign == 1 in 1/40
summarize price if rep78 == 3 in 1/50

di "===== _n and _N ====="
sort price
gen idx = _n
gen tot = _N
gen lagprice = price[_n-1]
gen leadprice = price[_n+1]
list idx price lagprice leadprice in 1/5
list idx price lagprice leadprice in 72/74

di "===== by-group _n and _N ====="
sort foreign price
by foreign: gen gidx = _n
by foreign: gen gtot = _N
by foreign: gen firstprice = price[1]
list foreign gidx gtot firstprice in 1/3
list foreign gidx gtot firstprice in 52/54

di "===== string functions ====="
di length("hello")
di substr("hello", 2, 3)
di upper("abc")
di strpos("hello", "ll")
di trim("  pad  ")
di subinstr("aaa", "a", "b", 2)
di strlen("héllo")
di ustrlen("héllo")
di ("abc" + "def")
di real("3.14")
di string(3.14159, "%6.2f")

di "===== compound quotes and macros ====="
local a = 5
local b "some text"
di `a'
di "`b'"
di `"nested "quoted" text"'
local c `"`b' more"'
di `"`c'"'
global g "world"
di "hello $g"
local n : word count one two three
di "word count = `n'"
local w : word 2 of one two three
di "word 2 = `w'"

di "===== macro expansion order ====="
local var price
summarize `var'
local cmd summarize
`cmd' mpg
local opt , detail
quietly summarize price `opt'
di r(p50)

di "===== display formats ====="
di %9.0g 1234567
di %9.2f 3.14159
di %-10s "left"
di %10s "right"
di %tc 0
di %td 0
di %td 22000

di "===== END ====="
