# `tests/fixtures/mock/` — canned engine streams (W07)

`scenario_a.msgpack` is the stream `--mock` replays. It is **not** a bag of
JSON: it is back-to-back CONTRACTS.md §10 frames,

```
len:u32LE | kind:u8 = 2 (Event) | corr:u32LE = 0 | rmp_serde::to_vec_named(EngineEvent)
```

so loading it exercises `stratum_proto::frame::FrameReader` — the same decoder
the desktop points at a real `stratum serve`. A fixture in a format nothing else
parses would prove nothing about the format everything else parses.

## What it contains

`auto.do`, three blocks, three runs:

| Run | Block | Card |
|---|---|---|
| 1 | `sysuse auto, clear` | `DataChanged` + `StateChanged` (74 × 12) |
| 2 | `summarize price mpg` | `Summarize` |
| 3 | `regress price mpg weight foreign` | `Estimation` with the ANOVA block |

**Every number is StataMP 18.5's.** The classic text and the pre-formatted
display strings are copied from `tests/golden/stata18/core_surface.log`
(lines 93–99 and 278–294), and
`mock_engine::tests::scenario_a_classic_text_is_verbatim_from_the_golden_log`
asserts each line still appears there verbatim. A renderer built against
invented numbers is a renderer that has never seen a real column width.

## Regenerating

The bytes are generated from `mock_engine::scenario_a()` and diffed by
`mock_engine::tests::committed_fixture_matches_the_script`, the same contract
`stratum-tokens`' generated source has. To regenerate: delete the file and run
that test.

## Framing guarantees the fixture upholds

CONTRACTS §7's list, asserted by
`mock_engine::tests::scenario_a_obeys_the_section_7_framing_guarantees`:
exactly one `RunStarted` first and one `RunFinished` last per run,
`BlockStarted`/`BlockFinished` never interleaved, and `seq` strictly increasing
by one across the whole stream.
