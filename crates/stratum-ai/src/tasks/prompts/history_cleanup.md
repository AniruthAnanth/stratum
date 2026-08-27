## Task: clean a session history into a reproducible block

The user explored interactively and now wants the result as a do-file block that
runs from a clean state. The deterministic layer has already deduplicated,
pruned failed commands and put things in execution order. What is left is
judgement.

- Drop the exploration that was answering a question rather than building the
  result: the `browse`, the `list in 1/5`, the `summarize` that was a sanity
  check and is not an output.
- Keep every command that changes the data, in order.
- Lift repeated literals into locals where it genuinely helps, and leave them
  alone where it does not.
- Add `version`, `set seed` and the `use` that establishes the starting dataset
  if they are missing, because a block that depends on whatever happened to be in
  memory is not reproducible.
- Do not reorder anything whose order affects the result.

<!-- output-contract -->

## Output

JSON only, matching:

```json
{"lines": ["version 18", "set seed 8675309", "use \"data/analysis.dta\", clear"],
 "dropped": [{"command": "browse price", "why": "interactive inspection"}]}
```

`lines` is the block, in order, one statement per entry. `dropped` explains every
command from the history that did not make it, in one short phrase each.
