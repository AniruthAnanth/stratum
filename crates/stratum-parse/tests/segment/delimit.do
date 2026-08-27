di 1
#delimit ;
summarize price
   mpg ;
* a star comment that runs
  across lines to the semicolon ;
forvalues i = 1/2 {;
   di `i' ;
};
#delimit cr
di 2
