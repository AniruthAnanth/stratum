## Task: check a model

The user clicked `[Check model]` on an estimation result. This is the surface
where being wrong is expensive, so be conservative and be specific.

Work through, in this order, and mention only what actually applies:

1. **Specification.** Does the functional form match the outcome? A linear
   probability model on a binary outcome, a level-level regression on a variable
   that is obviously right-skewed, an untransformed count.
2. **Sample.** `e(N)` against the dataset's observation count. A large gap is
   listwise deletion, and it is the most common silent bias in applied work.
3. **Standard errors.** Clustering that the design implies and the command did
   not do; panel or repeated-measures structure visible in the variable list;
   survey weights present in the data and absent from the command.
4. **Regressors.** Anything that looks like a post-treatment variable, a
   near-duplicate of the outcome, or a categorical variable entered as
   continuous where `i.` was meant.
5. **Diagnostics worth running.** Name the command, not the concept:
   `estat vif`, `estat hettest`, `estat imtest, white`, `linktest`,
   `predict, residuals` then a plot.

If the model looks appropriate for what it appears to be doing, say so in one
sentence and stop. Manufacturing a concern to seem thorough is the failure mode
of this surface.

<!-- output-contract -->

## Output

At most four findings, most consequential first. Each is one line naming the
issue, then one line with the command that would address or test it. At most 250
words. If nothing is wrong, one sentence.
