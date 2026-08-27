use auto, clear
by rep78: summarize price
bysort foreign (price): gen n = _n
quietly summarize mpg
capture noisily regress price mpg
version 17: describe
`cmd' price
su mpg
d
reg price mpg weight
