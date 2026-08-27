foreach v of varlist mpg price {
    summarize `v'
}
forvalues i = 1/3 {
    di `i'
}
if 1 == 1 {
    di "yes"
}
else if 2 == 2 {
    di "maybe"
}
else {
    di "no"
}
capture {
    di 1
}
quietly {
    di 2
}
{
    di 3
}
forvalues i = 1/1 {
    di "{"
}
if 1 == 1 {
    di "a"
} else {
    di "b"
}
while 0 {
    di "never"
}
