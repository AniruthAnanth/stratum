// %% Data loading
use survey.dta, clear

// %% Cleaning
* explains the next command
drop if missing(income)

* standalone note

summarize income
