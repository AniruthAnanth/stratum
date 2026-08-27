sysuse auto, clear

#delimit ;
summarize
    price
    mpg
    weight;
regress price mpg
    weight /* a block comment inside a semicolon statement
              spanning physical lines */
    , robust;
#delimit cr

summarize price
summarize mpg
