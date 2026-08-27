// %% Setup

//| This is narrative prose. It renders as a paragraph,
//| not as three separate widgets.
//| Third line of the same run.

sysuse auto, clear

/*md
A markdown narrative block. Everything here is prose.
*/

// %%   Model   
regress price mpg

//| A second, unrelated narrative run.
summarize price
