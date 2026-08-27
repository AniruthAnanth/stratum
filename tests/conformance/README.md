# `tests/conformance` — our-runtime-only, no Stata needed

ARCHITECTURE §8.9 (as amended by A8) states three properties of
`stratum run <case> --json --deterministic`:

1. the bytes are **identical on macOS, Windows and Linux**;
2. **two consecutive clean runs** on one machine are identical, tempnames
   included;
3. the output is **unchanged at `RAYON_NUM_THREADS ∈ {1, 2, 8}`** (ADR-013 — the
   map/reduce split must not leak the thread count).

CONTRACTS §7.2 adds a fourth: every captured stream must be a **fixed point** of
`cargo xtask normalize-ndjson`, which is how the two implementations of the
substitution table are kept from drifting apart.

Two drivers run this directory, and both take `tests/conformance/*.do` — the
files directly under it, never a subdirectory:

```sh
cargo xtask conformance                       # all four properties, locally
cargo xtask conformance --out out/ ...        # CI's producer/comparator split
```

**Every case must exit 0.** Both drivers treat a non-zero exit as a failure, so
a case that is *expected* to fail belongs in `tests/golden/stata18/errors.log`'s
world, not in this one.

## What is in the corpus today, and what it proves

| Case | What it puts on the wire |
|---|---|
| `envelope.do` | The run envelope alone: entry path, path separator, working directory, wall clock, version string, id allocation order. |
| `sections.do` | The same, over a file with section markers, a block comment, a continuation and non-ASCII text — so a platform that disagreed about UTF-8, line endings or what counts as an executable region shows up as a different `plan_len`. |

Both files **execute nothing**: they contain only trivia, so `plan_len` is 0 and
the stream is `RunStarted`, `RunFinished`, `rc = 0`, no blocks.

That is a deliberate and limited claim, so read it precisely. Properties 1, 2
and 4 are genuinely exercised end to end, through the shipped binary, on three
operating systems — and property 1 is the one that catches a Windows backslash
leaking into `RunStarted.source`, which is a real bug this corpus would have
caught. **Property 3 is not exercised at all**: nothing here does arithmetic, so
the thread count has nothing to leak into. The corpus is currently a test of the
harness and of the envelope, and it is not yet a test of the numbers.

## `staged/`

`staged/*.do` are the cases that need the execution engine. They are held one
directory down because neither driver recurses, so they are inert until somebody
moves them up — which is the whole activation step.

| Staged case | Waiting on |
|---|---|
| `auto_core.do` | `crates/stratum-exec` (W08). The spine of `tests/golden/stata18/core_surface.log`, so it checks fidelity and cross-platform identity in one run. |
| `semantics.do` | The same. Extended missing ordering and float-versus-double, from `semantics.log`. |
| `threads.do` | The same. This is the case ADR-013's property needs: a sort with ties and reductions over a column large enough to be chunked. |

**Why they are staged rather than live.** `crates/stratum-exec` is not linked
into `stratum-cli` in this build, so `stratum run` on a file that has anything to
execute emits one `STRATUM0010` diagnostic and exits **10** — "we are
incomplete", as distinct from 1, "we are wrong". A driver that requires exit 0
would fail on every one of them, and it would fail for a reason no change to this
directory can fix. Adding them to the corpus now would turn a known, named gap
into a red build that says nothing.

## When the engine lands

1. `git mv tests/conformance/staged/*.do tests/conformance/`.
2. Run `cargo xtask conformance` and read the failures — the first run of a real
   corpus is where thread-count leaks and platform drift surface.
3. Leave `envelope.do` and `sections.do` where they are. Their streams do not
   change when an engine appears (there is still nothing to execute), and a
   corpus that can isolate an envelope difference from a numerical one is worth
   more than a corpus that can only report that two megabytes of NDJSON differ.

## Not yet here

`docs/design/08-platform-packaging-ci.md` §9.5's Linux release checklist runs

```sh
test "$(xdg-mime query filetype tests/conformance/dta/auto.dta)" = "application/x-stata-dta"
```

There is no `dta/` directory here. The `.dta` fixtures are committed once, at
`tests/fixtures/dta/`, and they were captured from a StataMP 18.5 licence that
has since expired — copying an irreplaceable fixture to give a checklist its
literal path is a worse trade than fixing the path. **Escalated:** either §9.5
should read `tests/fixtures/dta/auto.dta`, or `tests/fixtures/dta/**`'s owner
should place the copy.
