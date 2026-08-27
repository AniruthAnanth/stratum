//! The corpus a scenario is written against, and the five scenario scripts.
//!
//! # Nothing here is invented
//!
//! Every number a scenario asserts comes from one of two committed artifacts:
//!
//! * `tests/fixtures/mock/scenario_a.msgpack` — W07's canned `EngineEvent`
//!   stream, framed exactly as the transport frames it, whose figures are
//!   copied from `tests/golden/stata18/core_surface.log`;
//! * `tests/golden/stata18/*.log` — StataMP 18.5's own output.
//!
//! [`the_expected_text_is_verbatim_from_the_golden_log`] is the test that keeps
//! that true: it takes the strings the scenarios assert on and looks for them in
//! the golden log. A scenario that starts asserting a number somebody typed from
//! memory fails there rather than passing quietly for two years.
//!
//! [`the_fixture_do_file_is_the_document_the_canned_stream_is_about`] closes the
//! other half: `tests/e2e/fixtures/scenario_a.do` must be the exact document
//! W07's block map has byte offsets into. Two files that are *supposed* to be
//! the same document and are not is how an e2e harness ends up asserting against
//! a caret position that means nothing.

use std::path::{Path, PathBuf};

use stratum_proto::engine::EngineEvent;
use stratum_proto::frame::{FrameKind, FrameReader};

use crate::actions::{Action, Chord};
use crate::snapshot::{Expect, Glyph};
use crate::{Capability, Scenario, ScenarioId, Step};

/// Repo-relative path of W07's committed event stream.
pub const MOCK_STREAM: &str = "tests/fixtures/mock/scenario_a.msgpack";
/// Where this unit's `.do` fixtures live.
pub const FIXTURE_DIR: &str = "tests/e2e/fixtures";
/// The golden capture the classic text in the stream came from.
pub const GOLDEN_CORE_SURFACE: &str = "tests/golden/stata18/core_surface.log";

/// Anything that stopped the corpus being readable.
#[derive(Debug, thiserror::Error)]
pub enum FixtureError {
    /// The repo root could not be located.
    #[error("no repo root above {0}: nothing containing docs/ownership.toml")]
    NoRepoRoot(PathBuf),
    /// A file would not read.
    #[error("reading {path}: {source}")]
    Io {
        /// The file.
        path: PathBuf,
        /// Why not.
        source: std::io::Error,
    },
    /// The canned stream did not decode.
    #[error("decoding {0}: {1}")]
    Decode(&'static str, String),
}

/// Walk up from this crate's manifest to the repo root.
///
/// Identified by `docs/ownership.toml`, exactly as `mock_engine::repo_root`
/// identifies it. A harness that guesses the root from the process cwd breaks
/// the moment somebody runs `cargo test` from inside a crate directory.
///
/// # Errors
/// When no ancestor contains `docs/ownership.toml`.
pub fn repo_root() -> Result<PathBuf, FixtureError> {
    let start = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    start
        .ancestors()
        .find(|p| p.join("docs/ownership.toml").is_file())
        .map(Path::to_path_buf)
        .ok_or(FixtureError::NoRepoRoot(start))
}

/// Read a repo-relative file.
///
/// # Errors
/// Root discovery and I/O failures.
pub fn read_repo_file(rel: &str) -> Result<Vec<u8>, FixtureError> {
    let path = repo_root()?.join(rel);
    std::fs::read(&path).map_err(|source| FixtureError::Io { path, source })
}

/// Read one of `tests/e2e/fixtures/*.do`.
///
/// # Errors
/// Root discovery and I/O failures.
pub fn fixture(name: &str) -> Result<String, FixtureError> {
    let bytes = read_repo_file(&format!("{FIXTURE_DIR}/{name}"))?;
    String::from_utf8(bytes).map_err(|e| FixtureError::Decode("fixture", e.to_string()))
}

/// Decode W07's committed stream.
///
/// Uses `stratum_proto::frame::FrameReader`, which is the same reader the
/// transport uses — a fixture read by a bespoke parser proves nothing about the
/// format everything else parses.
///
/// # Errors
/// Root discovery, I/O, framing and MessagePack failures.
pub fn mock_stream() -> Result<Vec<EngineEvent>, FixtureError> {
    let bytes = read_repo_file(MOCK_STREAM)?;
    let mut reader = FrameReader::new();
    reader.feed(&bytes);
    let mut out = Vec::new();
    loop {
        let frame = reader
            .next_frame()
            .map_err(|e| FixtureError::Decode(MOCK_STREAM, e.to_string()))?;
        let Some(frame) = frame else { break };
        if frame.kind != FrameKind::Event {
            return Err(FixtureError::Decode(
                MOCK_STREAM,
                format!("expected an event frame, got {:?}", frame.kind),
            ));
        }
        out.push(
            rmp_serde::from_slice(&frame.payload)
                .map_err(|e| FixtureError::Decode(MOCK_STREAM, e.to_string()))?,
        );
    }
    reader
        .end_of_stream()
        .map_err(|e| FixtureError::Decode(MOCK_STREAM, e.to_string()))?;
    Ok(out)
}

/// The canned stream, cut at run boundaries.
#[derive(Clone, PartialEq, Debug)]
pub struct Runs {
    /// Everything before the first `RunStarted`: engine health, the block map,
    /// and the initial per-block statuses. What a real engine emits on open.
    pub preamble: Vec<EngineEvent>,
    /// One entry per `RunStarted`..`RunFinished` span, in order.
    pub runs: Vec<Vec<EngineEvent>>,
}

/// Split a stream into its preamble and its runs.
#[must_use]
pub fn split_runs(events: &[EngineEvent]) -> Runs {
    let mut preamble = Vec::new();
    let mut runs: Vec<Vec<EngineEvent>> = Vec::new();
    let mut current: Option<Vec<EngineEvent>> = None;
    for ev in events {
        match ev {
            EngineEvent::RunStarted { .. } => {
                if let Some(done) = current.take() {
                    runs.push(done);
                }
                current = Some(vec![ev.clone()]);
            }
            EngineEvent::RunFinished { .. } => {
                let mut run = current.take().unwrap_or_default();
                run.push(ev.clone());
                runs.push(run);
            }
            other => match current.as_mut() {
                Some(run) => run.push(other.clone()),
                None => preamble.push(other.clone()),
            },
        }
    }
    if let Some(done) = current.take() {
        runs.push(done);
    }
    Runs { preamble, runs }
}

/// The block spans the canned block map declares, in block order.
#[must_use]
pub fn block_spans(events: &[EngineEvent]) -> Vec<(u32, u32)> {
    events
        .iter()
        .find_map(|e| match e {
            EngineEvent::BlockMapChanged { map, .. } => Some(
                map.regions
                    .iter()
                    .map(|r| (r.span.start, r.span.end))
                    .collect(),
            ),
            _ => None,
        })
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// The five scenario scripts (spec §38)
// ---------------------------------------------------------------------------

/// The `.do` file Scenarios A, B and E are about.
pub const SCENARIO_A_DO: &str = "scenario_a.do";

fn shift_enter() -> Chord {
    Chord::new("Shift+Enter")
}

/// `Shift+Enter` is bound `when: editorFocus`, and "the cursor is on the
/// summarize block" is the scenario's own premise — so the chord is resolved
/// under that context rather than against an empty one, which would read a
/// correctly gated binding as "bound to nothing".
fn in_the_editor() -> serde_json::Value {
    serde_json::json!({ "editorFocus": true, "selectionEmpty": true })
}

/// **Scenario A — notebook-like analysis.**
///
/// > open do-file → cursor on `summarize` → Shift+Enter → result appears
/// > underneath → cursor moves to next executable block → execute regression →
/// > result appears underneath → no source-code corruption.
///
/// # Errors
/// When the fixture corpus cannot be read.
pub fn scenario_a() -> Result<Scenario, FixtureError> {
    let text = fixture(SCENARIO_A_DO)?;
    let stream = mock_stream()?;
    let cut = split_runs(&stream);
    let spans = block_spans(&stream);
    let summarize_at = spans.get(1).map_or(0, |s| s.0);
    let run = |i: usize| cut.runs.get(i).cloned().unwrap_or_default();

    let steps = vec![
        Step::new(
            "open the analysis; every block starts never-run",
            Action::OpenDoc {
                fixture: SCENARIO_A_DO.to_owned(),
                text: text.clone(),
                feed: cut.preamble.clone(),
            },
        )
        .expect(Expect::DocEquals(text.clone()))
        .expect(Expect::BlockStatusIs(0, Glyph::NeverRun))
        .expect(Expect::BlockStatusIs(1, Glyph::NeverRun))
        .expect(Expect::BlockStatusIs(2, Glyph::NeverRun)),
        Step::new(
            "load the data",
            Action::Run {
                label: "sysuse auto, clear".to_owned(),
                verb: "run.blockAndAdvance".to_owned(),
                args: serde_json::Value::Null,
                chord: Some(shift_enter()),
                context: in_the_editor(),
                feed: run(0),
            },
        )
        .expect(Expect::BlockStatusIs(0, Glyph::Current))
        .expect(Expect::ResultsForBlock(0, 1))
        .expect(Expect::ResultRawContains(
            0,
            "(1978 automobile data)".to_owned(),
        )),
        Step::new(
            "put the cursor on the summarize block",
            Action::PlaceCaret {
                offset: summarize_at,
            },
        )
        .needs(&[Capability::Editor])
        .expect(Expect::CaretInBlock(1)),
        Step::new(
            "Shift+Enter — the result appears underneath",
            Action::Run {
                label: "summarize price mpg".to_owned(),
                verb: "run.blockAndAdvance".to_owned(),
                args: serde_json::Value::Null,
                chord: Some(shift_enter()),
                context: in_the_editor(),
                feed: run(1),
            },
        )
        .expect(Expect::BlockStatusIs(1, Glyph::Current))
        .expect(Expect::ResultsForBlock(1, 1))
        .expect(Expect::ResultPayloadIs(1, "summarize".to_owned()))
        // StataMP 18.5, core_surface.log line 75.
        .expect(Expect::ResultRawContains(
            1,
            "       price |         74    6165.257    2949.496       3291      15906".to_owned(),
        ))
        .expect(Expect::CardsForBlock(1, 1))
        .expect(Expect::CardHeaderIs(1, "summarize price mpg".to_owned()))
        .expect(Expect::CardBodyContains(1, "6165.257".to_owned()))
        .expect(Expect::GutterIs(1, Glyph::Current)),
        Step::new(
            "and the cursor has moved to the next executable block",
            Action::Observe {
                label: "caret after run.blockAndAdvance".to_owned(),
            },
        )
        .needs(&[Capability::Editor])
        .expect(Expect::CaretInBlock(2)),
        Step::new(
            "execute the regression",
            Action::Run {
                label: "regress price mpg weight foreign".to_owned(),
                verb: "run.blockAndAdvance".to_owned(),
                args: serde_json::Value::Null,
                chord: Some(shift_enter()),
                context: in_the_editor(),
                feed: run(2),
            },
        )
        .expect(Expect::BlockStatusIs(2, Glyph::Current))
        .expect(Expect::ResultsForBlock(2, 1))
        .expect(Expect::ResultPayloadIs(2, "estimation".to_owned()))
        // core_surface.log line 292.
        .expect(Expect::ResultRawContains(
            2,
            "     foreign |    3673.06   683.9783     5.37   0.000     2308.909    5037.212"
                .to_owned(),
        ))
        // The same estimate, read back out of W14's RENDERED card rather than
        // out of the payload — the claim §38-A actually makes, which is that the
        // number the user sees under the block is the number StataMP printed.
        //
        // Repair round 3 replaced `"R-squared"` here, which could never match
        // and was asserting the wrong surface. "R-squared" is the CLASSIC LOG's
        // spelling and it is pinned above, on the raw text, where it is true.
        // The card is Stratum's own UI: 06 §6.4 spells the same statistic `R²`,
        // and the card does not draw it at all today because
        // `EstimationPayload.scalars` carries `f64` with no display sibling —
        // W14's escalation in `renderers/estimation/index.tsx`, and a
        // `stratum-proto` change nobody has made. That gap is asserted by
        // `the_model_strip_is_still_owed_a_display_string` in
        // `tests/e2e/harness.rs`, which goes red the day the contract gains the
        // field so this step gets its `R²` assertion back. It is NOT asserted by
        // leaving a needle here that fails for a different reason than the one
        // that is true.
        .expect(Expect::CardBodyContains(2, "foreign".to_owned()))
        .expect(Expect::CardBodyContains(2, "3673.06".to_owned())),
        Step::new(
            "results appear underneath, in the order they were produced",
            Action::Observe {
                label: "card order".to_owned(),
            },
        )
        .expect(Expect::ResultOrderIs(vec![0, 1, 2]))
        .expect(Expect::CardOrderIs(vec![0, 1, 2])),
        Step::new(
            "and the source code was not corrupted",
            Action::Observe {
                label: "doc.toString() after three runs".to_owned(),
            },
        )
        .expect(Expect::DocEquals(text)),
    ];

    Ok(Scenario {
        id: ScenarioId::A,
        title: "notebook-like analysis (§38-A)",
        steps,
    })
}

/// **Scenario B — stale state.**
///
/// > execute transformation → execute dependent model → change transformation
/// > code → model output marked stale → rerun from changed block.
///
/// The two halves are owned by different layers and are asserted separately on
/// purpose. "The block I just edited displays stale" is `displayedStatus` in
/// `apps/desktop/src/state/doc.ts` — the frontend's own rule, W12's, and it runs
/// today. "The *dependent* model went stale because its input changed" is the
/// engine's judgement (`StatusChanged` is AUTHORITATIVE staleness, CONTRACTS §3)
/// and no engine exists yet, so it is blocked on W09 rather than simulated. A
/// harness that faked the propagation would be asserting its own arithmetic.
///
/// # Errors
/// When the fixture corpus cannot be read.
pub fn scenario_b() -> Result<Scenario, FixtureError> {
    let text = fixture(SCENARIO_A_DO)?;
    let stream = mock_stream()?;
    let cut = split_runs(&stream);
    let spans = block_spans(&stream);
    let first = spans.first().copied().unwrap_or((0, 0));
    let run = |i: usize| cut.runs.get(i).cloned().unwrap_or_default();

    let steps = vec![
        Step::new(
            "open the analysis",
            Action::OpenDoc {
                fixture: SCENARIO_A_DO.to_owned(),
                text,
                feed: cut.preamble.clone(),
            },
        ),
        Step::new(
            "execute the transformation",
            Action::Run {
                label: "sysuse auto, clear".to_owned(),
                verb: "run.block".to_owned(),
                args: serde_json::Value::Null,
                chord: Some(Chord::new("Mod+Enter")),
                context: in_the_editor(),
                feed: run(0),
            },
        )
        .expect(Expect::BlockStatusIs(0, Glyph::Current)),
        Step::new(
            "execute the dependent model",
            Action::Run {
                label: "regress price mpg weight foreign".to_owned(),
                verb: "run.block".to_owned(),
                args: serde_json::Value::Null,
                chord: Some(Chord::new("Mod+Enter")),
                context: in_the_editor(),
                feed: run(2),
            },
        )
        .expect(Expect::BlockStatusIs(2, Glyph::Current)),
        Step::new(
            "change the transformation's code",
            Action::Edit {
                span: (first.0, first.1),
                text: "sysuse auto".to_owned(),
            },
        )
        .expect(Expect::BlockStatusIs(0, Glyph::Stale)),
        Step::new(
            "the model's output is marked stale because its input changed",
            Action::Observe {
                label: "downstream staleness".to_owned(),
            },
        )
        // AUTHORITATIVE staleness is the engine's, not the frontend's.
        .needs(&[Capability::Engine])
        .expect(Expect::BlockStatusIs(2, Glyph::Stale))
        .expect(Expect::GutterIs(2, Glyph::Stale)),
        Step::new(
            "rerun from the changed block",
            Action::Run {
                label: "run.fromHere".to_owned(),
                verb: "run.fromHere".to_owned(),
                args: serde_json::Value::Null,
                chord: Some(Chord::new("Mod+Alt+Enter")),
                context: in_the_editor(),
                feed: Vec::new(),
            },
        )
        .needs(&[Capability::Engine])
        .expect(Expect::BlockStatusIs(0, Glyph::Current))
        .expect(Expect::BlockStatusIs(2, Glyph::Current)),
    ];

    Ok(Scenario {
        id: ScenarioId::B,
        title: "stale state (§38-B)",
        steps,
    })
}

/// **Scenario C — classic workflow.**
///
/// > switch to Classic layout → hide inline results → enter commands in Command
/// > pane → view output in Results → use Review history → open Data Editor →
/// > run an ordinary do-file. Must feel natural.
///
/// # Errors
/// When the fixture corpus cannot be read.
pub fn scenario_c() -> Result<Scenario, FixtureError> {
    let steps = vec![
        Step::new(
            "switch to the Classic layout",
            Action::verb_with(
                "layout.apply",
                serde_json::json!({ "id": "classic" }),
                Chord::new("Mod+Alt+2"),
            ),
        )
        .needs(&[Capability::Layout, Capability::Keymap])
        .expect(Expect::LayoutIs("classic".to_owned()))
        // 06 §8.3: the Classic preset's own default is `off`, so switching to it
        // is what hides inline results. Asserting the preset's default here is
        // what would catch somebody "improving" classic.json.
        .expect(Expect::InlineResultsIs("off".to_owned()))
        .expect(Expect::PaneVisible("history".to_owned()))
        .expect(Expect::PaneVisible("results".to_owned())),
        Step::new(
            "cycle inline results explicitly and land back on off",
            Action::verb("view.cycleInlineResults", Chord::new("Mod+Alt+I")),
        )
        .needs(&[Capability::Settings, Capability::Keymap])
        .expect(Expect::InlineResultsIs("always".to_owned())),
        Step::new(
            "focus the Command pane",
            Action::verb("commandbar.focus", Chord::new("Mod+L")),
        )
        .needs(&[Capability::Panes])
        .expect(Expect::FocusIs("commandbar".to_owned())),
        Step::new(
            "enter a command there",
            Action::Submit {
                text: "summarize price".to_owned(),
            },
        )
        .needs(&[Capability::Panes, Capability::Engine])
        .expect(Expect::HistoryTailIs(vec!["summarize price".to_owned()]))
        .expect(Expect::PaneContains(
            "results".to_owned(),
            "6165.257".to_owned(),
        )),
        Step::new(
            "a single click on a Review row loads it without running it",
            Action::Click {
                target: crate::Target::HistoryRow(0),
                clicks: 1,
            },
        )
        .needs(&[Capability::Panes])
        .expect(Expect::HistoryTailIs(vec!["summarize price".to_owned()])),
        Step::new(
            "a double click runs it again",
            Action::Click {
                target: crate::Target::HistoryRow(0),
                clicks: 2,
            },
        )
        .needs(&[Capability::Panes, Capability::Engine])
        .expect(Expect::HistoryTailIs(vec![
            "summarize price".to_owned(),
            "summarize price".to_owned(),
        ])),
        Step::new(
            "open the Data Editor",
            Action::verb("data.browse", Chord::new("Mod+Shift+D")),
        )
        .needs(&[Capability::DataEditor])
        .expect(Expect::PaneVisible("dataeditor".to_owned())),
        Step::new(
            "run an ordinary do-file from here",
            Action::Run {
                label: "run.file".to_owned(),
                verb: "run.file".to_owned(),
                args: serde_json::Value::Null,
                chord: None,
                context: serde_json::Value::Null,
                feed: Vec::new(),
            },
        )
        .needs(&[Capability::Engine])
        .expect(Expect::PaneContains(
            "results".to_owned(),
            "1978 automobile data".to_owned(),
        )),
    ];

    Ok(Scenario {
        id: ScenarioId::C,
        title: "classic workflow (§38-C)",
        steps,
    })
}

/// **Scenario D — interoperability.**
///
/// > save `.do` → inspect in a plain text editor → verify no embedded
/// > proprietary notebook data → run through the runtime → where applicable test
/// > in local licensed Stata.
///
/// **Most of D is not here, and that is deliberate.** D.1–D.3 are properties of
/// the written *bytes*, they belong to W26, and they are asserted — and pass
/// today — in `tests/e2e/scenario_d.rs`. Restating them here would create a
/// second, weaker copy of an assertion that already exists.
///
/// What is left for the harness is the part the byte-level tests cannot reach:
/// that the *app* holds the researcher's file unaltered and saves it through the
/// same writer. Only the first half is scripted, because the save verb has **no
/// id yet** — no keymap binds one and no unit has registered one; `doc_save` is
/// the CONTRACTS §11 command, not a frontend verb. Naming a plausible id here
/// would make this scenario fail as "unknown command" the day W13 lands, for a
/// reason that would be W25's invention rather than a defect. W13/W17 add the
/// step when they name the verb.
///
/// # Errors
/// When the fixture corpus cannot be read.
pub fn scenario_d() -> Result<Scenario, FixtureError> {
    let text = fixture(SCENARIO_A_DO)?;
    let steps = vec![
        Step::new(
            "open the analysis",
            Action::OpenDoc {
                fixture: SCENARIO_A_DO.to_owned(),
                text: text.clone(),
                feed: Vec::new(),
            },
        )
        .needs(&[Capability::Editor])
        .expect(Expect::DocEquals(text.clone())),
        Step::new(
            "the buffer the app holds is still the file the researcher wrote",
            Action::Observe {
                label: "doc.toString() after opening".to_owned(),
            },
        )
        .needs(&[Capability::Editor])
        .expect(Expect::DocEquals(text)),
    ];

    Ok(Scenario {
        id: ScenarioId::D,
        title: "interoperability (§38-D) — the app-level half; the byte-level half is \
                tests/e2e/scenario_d.rs",
        steps,
    })
}

/// **Scenario E — cross-platform.**
///
/// > build and launch equivalent packages on macOS, Windows and Linux; the same
/// > analysis file produces equivalent runtime results.
///
/// Packaging is W22's and launching is W17's. What the harness owns is the
/// second clause, and it owns it in a way that does not need either: E runs
/// Scenario A's own script on each OS in `e2e.yml`, and the workflow compares
/// the transcripts across the three platforms. `ScenarioReport::transcript` is
/// deliberately host- and timing-free, so a difference between two platforms is
/// a difference in what the app *did*.
///
/// # Errors
/// When the fixture corpus cannot be read.
pub fn scenario_e() -> Result<Scenario, FixtureError> {
    let mut a = scenario_a()?;
    a.id = ScenarioId::E;
    a.title = "cross-platform equivalence (§38-E) — Scenario A, transcript-compared across OSes";
    Ok(a)
}

/// All five, in order.
///
/// # Errors
/// When the fixture corpus cannot be read.
pub fn all() -> Result<Vec<Scenario>, FixtureError> {
    Ok(vec![
        scenario_a()?,
        scenario_b()?,
        scenario_c()?,
        scenario_d()?,
        scenario_e()?,
    ])
}

/// One scenario by letter.
///
/// # Errors
/// When the fixture corpus cannot be read.
pub fn by_id(id: ScenarioId) -> Result<Scenario, FixtureError> {
    match id {
        ScenarioId::A => scenario_a(),
        ScenarioId::B => scenario_b(),
        ScenarioId::C => scenario_c(),
        ScenarioId::D => scenario_d(),
        ScenarioId::E => scenario_e(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_canned_stream_decodes_through_the_real_frame_reader() {
        let events = mock_stream().expect("W07's committed stream");
        assert!(
            events.len() > 20,
            "expected a full three-run stream, got {}",
            events.len()
        );
        let cut = split_runs(&events);
        assert_eq!(cut.runs.len(), 3, "load, summarize, regress");
        assert!(
            !cut.preamble.is_empty(),
            "the engine's health and the block map arrive before any run"
        );
    }

    /// The whole reason W25 builds on W07's mock rather than inventing a second
    /// one: this asserts the two artifacts describe the SAME document. Every
    /// block span in the canned block map must cut exactly the text the canned
    /// `BlockStarted` says it ran.
    #[test]
    fn the_fixture_do_file_is_the_document_the_canned_stream_is_about() {
        let text = fixture(SCENARIO_A_DO).expect("the fixture");
        let events = mock_stream().expect("the stream");
        let spans = block_spans(&events);
        assert_eq!(spans.len(), 3);

        let mut checked = 0;
        for ev in &events {
            if let EngineEvent::BlockStarted {
                span, text: ran, ..
            } = ev
            {
                let start = span.start as usize;
                let end = span.end as usize;
                assert!(
                    end <= text.len(),
                    "block span {start}..{end} runs off the end of a {}-byte fixture",
                    text.len()
                );
                assert_eq!(
                    &text[start..end],
                    ran,
                    "the fixture's bytes at {start}..{end} are not the text the canned \
                     stream says ran there"
                );
                checked += 1;
            }
        }
        assert_eq!(checked, 3, "one BlockStarted per block");
    }

    /// ADR-017-style counter over the corpus: three runs, three results, three
    /// block spans. If W07's stream grows a fourth run, the scenarios that slice
    /// it by index have to be looked at, and this is what makes somebody look.
    #[test]
    fn the_corpus_has_the_shape_the_scenarios_index_into() {
        let events = mock_stream().expect("the stream");
        let results = events
            .iter()
            .filter(|e| matches!(e, EngineEvent::Result { .. }))
            .count();
        assert_eq!(results, 3);
        assert_eq!(block_spans(&events).len(), 3);
    }

    #[test]
    fn the_expected_text_is_verbatim_from_the_golden_log() {
        let golden = String::from_utf8(
            read_repo_file(GOLDEN_CORE_SURFACE).expect("the committed StataMP capture"),
        )
        .expect("utf-8");

        // Pulled out of the scenarios rather than retyped: the point is that
        // what the scenario asserts is what Stata printed.
        let a = scenario_a().expect("scenario A");
        let mut checked = 0;
        for step in &a.steps {
            for e in &step.expect {
                if let Expect::ResultRawContains(_, needle) = e {
                    // The `(1978 automobile data)` banner is `sysuse`'s and is
                    // not part of the tabular capture; everything else is.
                    if needle.starts_with('(') {
                        continue;
                    }
                    assert!(
                        golden.contains(needle.as_str()),
                        "scenario A asserts a line that is not in {GOLDEN_CORE_SURFACE}:\n{needle}"
                    );
                    checked += 1;
                }
            }
        }
        assert!(
            checked >= 2,
            "expected at least the summarize and regress rows"
        );
    }

    #[test]
    fn every_scenario_builds_and_asserts_something() {
        for s in all().expect("the five scenarios") {
            assert!(!s.steps.is_empty(), "scenario {} has no steps", s.id);
            let asserts: usize = s.steps.iter().map(|st| st.expect.len()).sum();
            assert!(
                asserts > 0,
                "scenario {} does nothing but press buttons",
                s.id
            );
        }
    }
}
