* STAGED — not yet part of the corpus. See ../README.md.
*
* From tests/golden/stata18/semantics.log: extended missing ordering, missing
* propagation, and float-versus-double precision. These are the semantics a
* second implementation gets subtly wrong, and they are also the ones most
* likely to differ between two machines' floating-point paths — which is
* exactly what ARCHITECTURE §8.9's three-OS comparison is for.

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
sort x
list x
count if x < .
count if missing(x)
count if x >= .
