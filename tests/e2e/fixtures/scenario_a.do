sysuse auto, clear

summarize price mpg

regress price mpg weight foreign
