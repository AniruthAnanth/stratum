program define mysum, rclass
    version 18
    syntax varlist(min=1) [if] [in]
    summarize `varlist' `if' `in'
    return scalar n = r(N)
end

mata:
real scalar f(real scalar x)
{
    return(x * 2)
}
end

program drop mysum

input id x
1 2
3 4
end

python:
print("hello")
end
