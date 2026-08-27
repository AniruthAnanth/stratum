* `///` folds the next physical line into this logical one.
local controls mpg ///
    weight ///
    length

regress price `controls', ///
    robust

* A `///` with a comment after it: everything to end of line is comment.
summarize price ///  this text is a comment
    mpg

* Two slashes are a comment, three are a continuation.
display 1 // not a continuation
display 2
