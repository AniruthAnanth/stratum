/* a plain block comment */
display 1

/* an outer block
   /* with a nested one inside it */
   still inside the outer block */
display 2

display "a // that is not a comment"
display "a /* that is not a comment either */"

local q `"a compound "quoted" string"'
display `q'

* A `*` comment only counts at the start of a line.
display 3 * 4
