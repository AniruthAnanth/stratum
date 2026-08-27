bysort foreign: summarize price
by foreign: egen m = mean(price)
quietly: regress price mpg
capture noisily: summarize nosuchvar
`cmd' price mpg
sort foreign
