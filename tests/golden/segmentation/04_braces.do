foreach v of varlist price mpg weight {
    summarize `v'
    if `r(N)' > 0 {
        display "`v' has data"
    }
}

forvalues i = 1/3 {
    display `i'
}

if 1 == 1 {
    display "yes"
}
else {
    display "no"
}

capture {
    confirm variable nosuch
}

quietly {
    summarize price
}

while 0 {
    display "never"
}
