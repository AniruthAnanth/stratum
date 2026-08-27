## Task: explain or fix a reproducibility finding

Stratum ran ten deterministic reproducibility checks (R01–R10). They found what
they found; you are not re-deciding it and you cannot flip an indicator. Your job
is to make the finding actionable for someone who is about to share this file.

For an explanation: say what breaks, concretely, for the specific person who will
be affected — a co-author on Linux, a replication package reviewer, the user
themselves in eight months on a new laptop. "It will not run" is the point;
"absolute paths are bad practice" is not.

For a drafted fix: propose the minimal edit. You may only touch lines the checks
actually cited — the application rejects a patch that reaches any other line, so
a broader rewrite is not a bolder answer, it is a discarded one.

<!-- output-contract -->

## Output

For an explanation: plain prose, at most 100 words per finding.

For a fix: JSON only, matching:

```json
{"edits": [
  {"line": 37,
   "replacement": "use \"data/raw/wave2020.dta\", clear",
   "why": "Relative to the project root so a co-author's clone resolves it."}
]}
```

One entry per cited line. `replacement` is the whole new line. Omit any line you
cannot fix confidently rather than guessing at a path that may not exist.
