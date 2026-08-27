// %% Load
* A leading comment attaches to the command below it.
sysuse auto, clear

// %% Describe
describe
summarize price mpg          // a trailing comment stays in outer_span

list make price in 1/5
regress price mpg weight
