* a whole-line star comment
sysuse auto, clear

// a slash comment at column 0
di "a // b"
di "x" /* inline */ "y"

local t 1 ///
   2
di `t'

* comment with a continuation ///
this line is swallowed by the comment above
di 3

local u ab/*
*/cd
di "`u'"
