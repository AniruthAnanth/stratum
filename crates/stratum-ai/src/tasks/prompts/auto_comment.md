## Task: propose comments

Spec §23. You are writing explanatory comments for the code in the focus. You are
**not** editing the code, and you structurally cannot: the application accepts
comment insertions only, and it verifies that the non-comment token stream is
byte-identical before and after applying anything you return. A comment that
would change what runs is rejected, along with the whole batch.

Write the comment a good collaborator leaves: why this step exists, what the
choice implies, what the reader would otherwise have to reconstruct. Not what the
line obviously says.

- Bad: `// generate log price` above `gen ln_price = log(price)`.
- Good: `// Log-transform price; the level model's residuals fan out badly.`
- Bad: `// merge the two files`.
- Good: `// 1:1 on pid; unmatched 2019 records are kept and flagged below.`

Skip anything that needs no comment. A file with four good comments is better
than one with thirty restatements. If a block is self-explanatory, leave it out
of your output entirely — coverage is not the goal.

Each comment is attached to an anchor line by its number and by a hash of that
line exactly as it was given to you. Do not invent anchors. Do not renumber.

<!-- output-contract -->

## Output

JSON only, no prose around it, matching:

```json
{"comments": [
  {"anchor_line": 42,
   "anchor_hash": "<the hash given with that line, copied verbatim>",
   "position": "above",
   "text": "One line, at most 200 characters, no newlines.",
   "kind": "explain"}
]}
```

`position` is `"above"` or `"trailing"`. `kind` is `"explain"`, `"why"`,
`"caveat"` or `"section"`. `text` must not contain `//`, `/*`, `*/`, `///`, or a
line break — the applier rejects any of those and drops the whole batch.
