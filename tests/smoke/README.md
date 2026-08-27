# `tests/smoke` — the exit-code corpus and the end-to-end stream

Design 08 §4.4 says the eleven exit codes are "fixed, documented, and asserted by
`tests/smoke`", and §9.5's release checklist runs `hello.do` through the
*installed* binary on all three operating systems:

```sh
stratum run tests/smoke/hello.do --json > out.jsonl
cargo xtask smoke assert-stream out.jsonl tests/smoke/expected.jsonl
```

## What is here

| File | What it is |
|---|---|
| `hello.do` | The end-to-end case. Three commands, every one of which has a committed StataMP 18.5 golden in `tests/golden/stata18/core_surface.log`. |
| `expected.jsonl` | `hello.do`'s stream under `--json --deterministic`, byte for byte. |

## Regenerating `expected.jsonl`

From the repo root, and **only** after reading the next section:

```sh
cargo run -q -p stratum-cli -- \
  run tests/smoke/hello.do --json --deterministic > tests/smoke/expected.jsonl
```

`--deterministic` is what makes the file comparable at all: a raw `--json` stream
carries a wall-clock timestamp, a duration, a version string and an absolute
`cwd`, and could never be byte-identical between two runs, let alone between
macOS and Windows. CONTRACTS §7.2 declares
`stratum run --json | xtask normalize-ndjson` *equivalent* to `--deterministic`,
which is why the release checklist above may capture the raw stream and still
diff it against this file: `assert-stream` normalises before comparing.

## Read this before you regenerate

Regenerating is how a golden stops being an oracle. This file exists to catch a
change nobody intended, so the only honest reason to rewrite it is that the
change **was** intended and you can say what it was. Two changes are expected and
already known:

1. **The engine lands.** `crates/stratum-exec` (W08) is not linked into this
   build, so today the stream is three events: `RunStarted`, one `STRATUM0010`
   diagnostic reading "the execution engine is not linked into this build", and
   `RunFinished` with `rc = 10`. `stratum run` therefore exits **10** — "we are
   incomplete" — and not 1, which would say "we are wrong". When the engine is
   linked, this file becomes the full three-block stream and the exit code
   becomes 0. That is the one regeneration this corpus is waiting for.
2. **`plan_len` follows the file.** It is 3 today: `sysuse`, `summarize`,
   `regress`. The `// %%` markers and the `*` comments are trivia and are not in
   the plan (CONTRACTS §2). Editing `hello.do` changes it.

What must **not** change without an argument: `seq` is 0, 1, 2; `run` and
`session` are both 1; `source` is the bare `hello.do`. §7.2 leaves every id
verbatim on purpose — normalising them would hide the id-allocation drift this
comparison exists to catch — so a diff in any of them is a real finding, not
noise to be regenerated away.

## The exit codes

`cli.rs` transcribes design 08 §4.4's table and `main.rs`'s `#[cfg(test)] mod
tests` drives every one of the eleven through `dispatch`, which is the same
function the binary runs. The codes are `0` success, `1` runtime error (**we are
wrong**), `2` usage, `3` I/O, `4` parse, `5` `check` denied, `6` `fmt --check`
changed, `7` interrupted, `8` timeout, `9` internal, `10` unsupported (**we are
incomplete**).
