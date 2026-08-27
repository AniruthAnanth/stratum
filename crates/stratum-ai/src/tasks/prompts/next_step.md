## Task: suggest the next step

The user has just finished a run and asked what to do next. Suggest **one**
concrete next command, chosen from what the session actually shows: the variables
in memory, what has been run, and the stored estimates.

Good next steps are the ones a careful colleague would say out loud: check the
merge you just did before you drop `_merge`; look at the distribution of the
variable you just created; run the specification with clustered errors before
you write it up; tabulate the categorical you are about to put in as continuous.

Bad next steps are generic advice ("consider exploring your data"), anything that
requires data you were not shown, and anything the user has already run — the
recent-command list is there so you do not repeat it back to them.

<!-- output-contract -->

## Output

One sentence saying why, then the command on its own line. At most 40 words
total. Never more than one command.
