//! The MOCK ENGINE — W07's schedule-critical artifact.
//!
//! It replays a canned `EngineEvent` stream **over the real transport**: framed
//! MessagePack (§10) written into a `tokio::io::duplex` pipe, decoded by the
//! same [`crate::transport::Transport`] that talks to a real `stratum serve`.
//! W12–W16 therefore develop against the production IPC surface from day 4,
//! and the day the engine lands the only thing that changes is which process is
//! on the other end of the pipe.
//!
//! Three rules kept this honest:
//!
//! * **No shortcut around the codec.** The mock encodes with
//!   `rmp_serde::to_vec_named` and frames with `stratum_proto::frame`; if the
//!   framing were wrong, the mock would fail exactly as a real engine would.
//! * **Real numbers.** Every figure in [`scenario_a`] is copied from
//!   `tests/golden/stata18/core_surface.log` — StataMP 18.5's own output for
//!   `summarize price mpg` and `regress price mpg weight foreign`. A renderer
//!   built against invented numbers is a renderer that has never seen a real
//!   column width.
//! * **Failure modes are part of the surface.** [`MockBehaviour::Uninterruptible`]
//!   exists so the cancel ladder (ARCHITECTURE C21) has something to escalate
//!   against, and the mock can be told to die mid-run so the crash banner has
//!   something to appear for.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use stratum_proto::block::{BlockMap, Delimiter, RegionKind, RegionSummary};
use stratum_proto::complete::CompletionEnv;
use stratum_proto::data::DataEvent;
use stratum_proto::engine::{
    BulkRef, EngineEvent, EngineHealth, EngineRequest, EngineResponse, STREAM_SCHEMA,
};
use stratum_proto::exec::{ExecStatus, PlanItem, PlanReason, RunPlan};
use stratum_proto::frame::{encode_frame, FrameKind, FrameReader, Ping, CORR_UNSOLICITED};
use stratum_proto::ids::{
    BlockId, CodeHash, DatasetStateId, DocumentId, ExecutionId, LineRange, ResultId, RunId,
    SessionEpoch, SessionId, Span, StateId,
};
use stratum_proto::result::{
    AnovaTable, AssetRef, CardAction, DataChangeSummary, EstimationPayload, LayoutHint, RawRef,
    ResultEnvelope, ResultPayload, StyleId, StyledRun, SummarizeDisplay, SummarizePayload,
    SummarizeRow, Term, VarKind,
};
use stratum_proto::status::BlockStatus;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::transport::{encode_body, BulkCopyLedger, TransportError};

/// The session every canned event belongs to.
pub const MOCK_SESSION: SessionId = SessionId(1);
/// The document `auto.do`.
pub const MOCK_DOC: DocumentId = DocumentId(1);

/// How faithful the mock is to a *working* engine.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum MockBehaviour {
    #[default]
    Responsive,
    /// Acknowledges nothing: no response to `ExecCancel` at any level, and the
    /// replay keeps running. This is what the cancel ladder escalates against —
    /// without it, "Abort at 2000 ms, kill at 4000 ms" is untested code.
    Uninterruptible,
    /// Stops writing mid-replay and drops the pipe, so the supervisor sees EOF
    /// with a run outstanding: ARCHITECTURE §3's crash banner path.
    CrashMidRun,
}

#[derive(Clone, Debug)]
pub struct MockOptions {
    pub behaviour: MockBehaviour,
    /// Delay between replayed events. Zero in tests; ~40 ms makes a demo look
    /// like a machine doing work rather than a screenshot.
    pub pace: Duration,
    /// The canned stream. Defaults to [`scenario_a`].
    pub script: Vec<EngineEvent>,
    /// Directory for bulk segment files. `None` ⇒ the OS temp dir.
    pub bulk_dir: Option<std::path::PathBuf>,
}

impl Default for MockOptions {
    fn default() -> Self {
        Self {
            behaviour: MockBehaviour::Responsive,
            pace: Duration::ZERO,
            script: scenario_a(),
            bulk_dir: None,
        }
    }
}

// ---------------------------------------------------------------------------
// The fixture: `tests/fixtures/mock/scenario_a.msgpack`
// ---------------------------------------------------------------------------

/// Encode a canned stream as back-to-back §10 event frames.
///
/// The fixture is **frames, not a bare array**, so loading it exercises the same
/// `FrameReader` the transport uses: a fixture in a format nothing else parses
/// would prove nothing about the format everything else parses.
pub fn encode_stream(events: &[EngineEvent]) -> Result<Vec<u8>, TransportError> {
    let mut out = Vec::with_capacity(64 * 1024);
    for ev in events {
        let body = encode_body("EngineEvent", ev)?;
        encode_frame(FrameKind::Event, CORR_UNSOLICITED, &body, &mut out)?;
    }
    Ok(out)
}

/// Inverse of [`encode_stream`]. Used by `--mock` to load the committed fixture.
pub fn decode_stream(bytes: &[u8]) -> Result<Vec<EngineEvent>, TransportError> {
    let mut reader = FrameReader::new();
    reader.feed(bytes);
    let mut out = Vec::new();
    while let Some(frame) = reader.next_frame()? {
        if frame.kind != FrameKind::Event {
            return Err(TransportError::UnexpectedKind);
        }
        out.push(rmp_serde::from_slice(&frame.payload).map_err(|source| {
            TransportError::Decode {
                what: "EngineEvent",
                source,
            }
        })?);
    }
    reader.end_of_stream()?;
    Ok(out)
}

/// Repo-relative path of the committed fixture.
pub const SCENARIO_A_FIXTURE: &str = "tests/fixtures/mock/scenario_a.msgpack";

/// Walk up to the repo root, identified by `docs/ownership.toml`.
///
/// Three candidates, because this file is compiled from two places: the desktop
/// crate (where `CARGO_MANIFEST_DIR` is `apps/desktop/src-tauri`) and W07's
/// out-of-tree verification harness (where it is not under the repo at all, and
/// `file!()` is the absolute path of this file).
#[must_use]
pub fn repo_root() -> Option<std::path::PathBuf> {
    let mut candidates = vec![std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))];
    candidates.push(std::path::PathBuf::from(file!()));
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd);
    }
    candidates.iter().find_map(|start| {
        start
            .ancestors()
            .find(|p| p.join("docs/ownership.toml").is_file())
            .map(std::path::Path::to_path_buf)
    })
}

// ---------------------------------------------------------------------------
// Scenario A — `auto.do`, three blocks, StataMP 18.5's own numbers
// ---------------------------------------------------------------------------

/// The document the canned stream is about. W12–W16 open exactly this text.
pub const AUTO_DO: &str = "\
sysuse auto, clear

summarize price mpg

regress price mpg weight foreign
";

const B_SYSUSE: BlockId = BlockId(1);
const B_SUMMARIZE: BlockId = BlockId(2);
const B_REGRESS: BlockId = BlockId(3);

fn hash(seed: u8) -> CodeHash {
    CodeHash([seed; 16])
}

fn txt(text: &str) -> StyledRun {
    StyledRun {
        text: text.to_owned(),
        style: StyleId::Text,
    }
}

fn res(text: &str) -> StyledRun {
    StyledRun {
        text: text.to_owned(),
        style: StyleId::Result,
    }
}

fn raw_ref(session: SessionId, result: ResultId, head: &str) -> RawRef {
    let bytes = head.len() as u64;
    RawRef {
        bytes,
        lines: head.lines().count() as u32,
        head: head.to_owned(),
        truncated: false,
        asset: AssetRef {
            path: format!("result/{}/{}/raw", session.0, result.0),
            mime: "text/plain; charset=utf-8".to_owned(),
            bytes,
        },
    }
}

fn region(
    index: u32,
    start: u32,
    end: u32,
    line: u32,
    canonical: &str,
    est: bool,
) -> RegionSummary {
    RegionSummary {
        index,
        span: Span { start, end },
        outer_span: Span { start, end },
        lines: LineRange {
            start: line,
            end: line,
        },
        code_lines: LineRange {
            start: line,
            end: line,
        },
        kind: RegionKind::Simple,
        entry_delimiter: Delimiter::Cr,
        exit_delimiter: Delimiter::Cr,
        code_hash: hash(index as u8 + 1),
        hash_ordinal: 0,
        canonical: Some(canonical.to_owned()),
        is_estimation: est,
        has_macro_in_head: false,
        section: None,
    }
}

/// The block map for [`AUTO_DO`]. Byte offsets are real offsets into it.
#[must_use]
pub fn scenario_a_block_map() -> BlockMap {
    let sysuse = AUTO_DO.find("sysuse").unwrap() as u32;
    let summarize = AUTO_DO.find("summarize").unwrap() as u32;
    let regress = AUTO_DO.find("regress").unwrap() as u32;
    BlockMap {
        doc: MOCK_DOC,
        generation: 1,
        doc_version: 1,
        blocks: vec![B_SYSUSE, B_SUMMARIZE, B_REGRESS],
        regions: vec![
            region(0, sysuse, sysuse + 18, 1, "sysuse", false),
            region(1, summarize, summarize + 19, 3, "summarize", false),
            region(2, regress, regress + 32, 5, "regress", true),
        ],
        markers: Vec::new(),
        sections: Vec::new(),
        retired: Vec::new(),
        diagnostics: Vec::new(),
        end_delimiter: Delimiter::Cr,
    }
}

/// `summarize price mpg` — `tests/golden/stata18/core_surface.log` lines 95–99.
fn summarize_payload() -> SummarizePayload {
    SummarizePayload {
        detail: false,
        weight: None,
        qualifier: None,
        rows: vec![
            SummarizeRow {
                var: "price".to_owned(),
                label: Some("Price".to_owned()),
                format: "%8.0gc".to_owned(),
                obs: 74,
                missing: 0,
                mean: 6_165.256_756_756_757,
                sd: 2_949.495_884_768_919,
                min: 3291.0,
                max: 15906.0,
                sum: 456_229.0,
                display: SummarizeDisplay {
                    obs: "74".to_owned(),
                    mean: "6165.257".to_owned(),
                    sd: "2949.496".to_owned(),
                    min: "3291".to_owned(),
                    max: "15906".to_owned(),
                },
                detail: None,
                var_kind: VarKind::Numeric,
                sparkline: Some(vec![
                    22, 14, 9, 8, 5, 4, 3, 2, 2, 1, 1, 1, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1,
                ]),
            },
            SummarizeRow {
                var: "mpg".to_owned(),
                label: Some("Mileage (mpg)".to_owned()),
                format: "%8.0g".to_owned(),
                obs: 74,
                missing: 0,
                mean: 21.297_297_297_297_3,
                sd: 5.785_503_284_909_31,
                min: 12.0,
                max: 41.0,
                sum: 1576.0,
                display: SummarizeDisplay {
                    obs: "74".to_owned(),
                    mean: "21.2973".to_owned(),
                    sd: "5.785503".to_owned(),
                    min: "12".to_owned(),
                    max: "41".to_owned(),
                },
                detail: None,
                var_kind: VarKind::Numeric,
                sparkline: Some(vec![
                    1, 3, 6, 9, 11, 12, 8, 7, 5, 4, 3, 2, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1,
                ]),
            },
        ],
    }
}

const SUMMARIZE_CLASSIC: &str = "\
    Variable |        Obs        Mean    Std. dev.       Min        Max
-------------+---------------------------------------------------------
       price |         74    6165.257    2949.496       3291      15906
         mpg |         74     21.2973    5.785503         12         41
";

const REGRESS_CLASSIC: &str = "\
      Source |       SS           df       MS      Number of obs   =        74
-------------+----------------------------------   F(3, 70)        =     23.29
       Model |   317252881         3   105750960   Prob > F        =    0.0000
    Residual |   317812515        70  4540178.78   R-squared       =    0.4996
-------------+----------------------------------   Adj R-squared   =    0.4781
       Total |   635065396        73  8699525.97   Root MSE        =    2130.8

------------------------------------------------------------------------------
       price | Coefficient  Std. err.      t    P>|t|     [95% conf. interval]
-------------+----------------------------------------------------------------
         mpg |    21.8536   74.22114     0.29   0.769    -126.1758     169.883
      weight |   3.464706    .630749     5.49   0.000     2.206717    4.722695
     foreign |    3673.06   683.9783     5.37   0.000     2308.909    5037.212
       _cons |  -5853.696   3376.987    -1.73   0.087    -12588.88    881.4934
------------------------------------------------------------------------------
";

/// One coefficient row. `num` and `d` are the same six columns in the same
/// order — coefficient, std. err., t, P>|t|, ci_lo, ci_hi — so the raw value and
/// the string A6 says a renderer must print instead sit at the same index and a
/// transposition is visible at the call site rather than three screens away.
fn term(name: &str, num: [f64; 6], d: [&str; 6]) -> Term {
    let [b, se, t, p, ci_lo, ci_hi] = num;
    Term {
        eq: 0,
        name: name.to_owned(),
        display: name.to_owned(),
        b,
        se,
        t,
        p,
        ci_lo,
        ci_hi,
        display_num: d.map(str::to_owned),
        beta: None,
        omitted: false,
        base: false,
        empty: false,
    }
}

/// `regress price mpg weight foreign` — golden log lines 280–294.
fn estimation_payload() -> EstimationPayload {
    EstimationPayload {
        cmd: "regress".to_owned(),
        cmdline: "regress price mpg weight foreign".to_owned(),
        depvar: "price".to_owned(),
        n: 74,
        rank: 4,
        eq_names: vec![String::new()],
        terms: vec![
            term(
                "mpg",
                [21.853_59, 74.221_14, 0.29, 0.769, -126.175_8, 169.883],
                [
                    "21.8536",
                    "74.22114",
                    "0.29",
                    "0.769",
                    "-126.1758",
                    "169.883",
                ],
            ),
            term(
                "weight",
                [3.464_706, 0.630_749, 5.49, 0.000, 2.206_717, 4.722_695],
                [
                    "3.464706", ".630749", "5.49", "0.000", "2.206717", "4.722695",
                ],
            ),
            term(
                "foreign",
                [3673.06, 683.978_3, 5.37, 0.000, 2308.909, 5037.212],
                [
                    "3673.06", "683.9783", "5.37", "0.000", "2308.909", "5037.212",
                ],
            ),
            term(
                "_cons",
                [-5853.696, 3376.987, -1.73, 0.087, -12588.88, 881.493_4],
                [
                    "-5853.696",
                    "3376.987",
                    "-1.73",
                    "0.087",
                    "-12588.88",
                    "881.4934",
                ],
            ),
        ],
        scalars: vec![
            ("N".to_owned(), 74.0),
            ("df_m".to_owned(), 3.0),
            ("df_r".to_owned(), 70.0),
            ("F".to_owned(), 23.289_9),
            ("r2".to_owned(), 0.499_6),
            ("r2_a".to_owned(), 0.478_1),
            ("rmse".to_owned(), 2130.8),
            ("mss".to_owned(), 317_252_881.0),
            ("rss".to_owned(), 317_812_515.0),
        ],
        macros: vec![
            ("cmd".to_owned(), "regress".to_owned()),
            ("depvar".to_owned(), "price".to_owned()),
            ("vce".to_owned(), "ols".to_owned()),
        ],
        anova: Some(AnovaTable {
            mss: 317_252_881.0,
            df_m: 3.0,
            ms_m: 105_750_960.0,
            rss: 317_812_515.0,
            df_r: 70.0,
            ms_r: 4_540_178.78,
            tss: 635_065_396.0,
            df_t: 73.0,
            ms_t: 8_699_525.97,
            display: [
                "317252881",
                "3",
                "105750960",
                "317812515",
                "70",
                "4540178.78",
                "635065396",
                "73",
                "8699525.97",
            ]
            .map(str::to_owned),
        }),
        vce: "ols".to_owned(),
        ci_level: 95.0,
        estimates_name: None,
        sample_hash: 0x5354_4154_4131_3835,
        diagnostics: Vec::new(),
        cond_number: None,
    }
}

fn completion_env() -> CompletionEnv {
    CompletionEnv {
        generation: 1,
        frame: "default".to_owned(),
        frames: vec!["default".to_owned()],
        varnames: [
            "make",
            "price",
            "mpg",
            "rep78",
            "headroom",
            "trunk",
            "weight",
            "length",
            "turn",
            "displacement",
            "gear_ratio",
            "foreign",
        ]
        .iter()
        .map(|s| (*s).to_owned())
        .collect(),
        var_total: 12,
        truncated: false,
        locals: Vec::new(),
        globals: Vec::new(),
        scalars: Vec::new(),
        matrices: Vec::new(),
        programs: Vec::new(),
        e_names: vec!["e(N)".to_owned(), "e(r2)".to_owned(), "e(rmse)".to_owned()],
        r_names: vec!["r(mean)".to_owned(), "r(sd)".to_owned(), "r(N)".to_owned()],
        value_labels: Vec::new(),
        stored_estimates: Vec::new(),
        cwd: camino::Utf8PathBuf::from("/Users/ana/proj"),
    }
}

/// Sequence counter for a script: §7 guarantee 5 is "strictly increasing per
/// session", so the canned stream must satisfy it too or the desktop's own gap
/// detector will fire against the mock.
struct Seq(u64);

impl Seq {
    fn next(&mut self) -> u64 {
        self.0 += 1;
        self.0
    }
}

/// The canned stream. Three runs — load, summarize, regress — covering the
/// event shapes W12 (shell), W13 (editor gutter), W14 (renderers), W15
/// (staleness UI) and W16 (classic pane) all need.
#[must_use]
pub fn scenario_a() -> Vec<EngineEvent> {
    let mut s = Seq(0);
    let mut out = Vec::new();

    out.push(EngineEvent::EngineHealth {
        seq: s.next(),
        health: EngineHealth::Ready,
    });
    out.push(EngineEvent::BlockMapChanged {
        seq: s.next(),
        map: scenario_a_block_map(),
    });
    out.push(EngineEvent::StatusChanged {
        seq: s.next(),
        doc: MOCK_DOC,
        changed: vec![
            (B_SYSUSE, BlockStatus::NeverRun),
            (B_SUMMARIZE, BlockStatus::NeverRun),
            (B_REGRESS, BlockStatus::NeverRun),
        ],
    });

    // --- run 1: sysuse auto, clear -----------------------------------------
    let e1 = ExecutionId(1);
    out.push(EngineEvent::RunStarted {
        seq: s.next(),
        schema: STREAM_SCHEMA,
        run: RunId(1),
        session: MOCK_SESSION,
        stratum_version: "0.1.0-mock".to_owned(),
        source: Some(camino::Utf8PathBuf::from("auto.do")),
        clean_state: false,
        cwd: camino::Utf8PathBuf::from("/Users/ana/proj"),
        started_at_ms: 1_755_000_000_000,
        seed: None,
        plan_len: 1,
    });
    out.push(EngineEvent::BlockStarted {
        seq: s.next(),
        run: RunId(1),
        exec: e1,
        block: B_SYSUSE,
        doc: Some(MOCK_DOC),
        span: Span { start: 0, end: 18 },
        code_hash: hash(1),
        dataset_state_in: DatasetStateId(0),
        text: "sysuse auto, clear".to_owned(),
    });
    out.push(EngineEvent::Output {
        seq: s.next(),
        exec: e1,
        stream: stratum_proto::engine::OutputStream::Results,
        runs: vec![txt("(1978 automobile data)\n")],
    });
    out.push(EngineEvent::StateChanged {
        seq: s.next(),
        exec: e1,
        dataset_state: DatasetStateId(17),
        state: StateId(1),
        frame: "default".to_owned(),
        n_obs: 74,
        n_vars: 12,
        events: vec![
            DataEvent::FrameChanged {
                frame: "default".to_owned(),
                state: DatasetStateId(17),
            },
            DataEvent::ObsCountChanged {
                frame: "default".to_owned(),
                n_obs: 74,
            },
        ],
    });
    out.push(EngineEvent::Result {
        seq: s.next(),
        exec: e1,
        envelope: ResultEnvelope {
            result: ResultId(1),
            revision: 0,
            exec: e1,
            block: Some(B_SYSUSE),
            dataset_state: DatasetStateId(17),
            code_hash: hash(1),
            cmdline: "sysuse auto, clear".to_owned(),
            started_at_ms: 1_755_000_000_000,
            duration_us: 8_412,
            rc: 0,
            payloads: vec![ResultPayload::DataChanged(DataChangeSummary {
                frame: "default".to_owned(),
                obs_before: 0,
                obs_after: 74,
                vars_before: 0,
                vars_after: 12,
                created: Vec::new(),
                modified: Vec::new(),
                dropped: Vec::new(),
                renamed: Vec::new(),
                notes: vec!["(1978 automobile data)".to_owned()],
            })],
            raw: raw_ref(MOCK_SESSION, ResultId(1), "(1978 automobile data)\n"),
            layout_hint: LayoutHint {
                rows: 1,
                cols: 1,
                est_px: 48,
            },
            actions: vec![CardAction::RawOutput],
        },
    });
    out.push(EngineEvent::BlockFinished {
        seq: s.next(),
        run: RunId(1),
        exec: e1,
        block: B_SYSUSE,
        result: Some(ResultId(1)),
        status: ExecStatus::Succeeded,
        rc: 0,
        duration_us: 8_412,
        dataset_state_out: DatasetStateId(17),
    });
    out.push(EngineEvent::StatusChanged {
        seq: s.next(),
        doc: MOCK_DOC,
        changed: vec![(
            B_SYSUSE,
            BlockStatus::Current {
                exec: e1,
                dataset: DatasetStateId(17),
                duration_us: 8_412,
            },
        )],
    });
    out.push(EngineEvent::CompletionEnvChanged {
        seq: s.next(),
        env: completion_env(),
    });
    out.push(EngineEvent::RunFinished {
        seq: s.next(),
        run: RunId(1),
        rc: 0,
        blocks_run: 1,
        blocks_failed: 0,
        duration_us: 9_001,
        finished_at_ms: 1_755_000_000_009,
    });

    // --- run 2: summarize price mpg ----------------------------------------
    let e2 = ExecutionId(2);
    out.push(EngineEvent::RunStarted {
        seq: s.next(),
        schema: STREAM_SCHEMA,
        run: RunId(2),
        session: MOCK_SESSION,
        stratum_version: "0.1.0-mock".to_owned(),
        source: Some(camino::Utf8PathBuf::from("auto.do")),
        clean_state: false,
        cwd: camino::Utf8PathBuf::from("/Users/ana/proj"),
        started_at_ms: 1_755_000_001_000,
        seed: None,
        plan_len: 1,
    });
    out.push(EngineEvent::BlockStarted {
        seq: s.next(),
        run: RunId(2),
        exec: e2,
        block: B_SUMMARIZE,
        doc: Some(MOCK_DOC),
        span: Span { start: 20, end: 39 },
        code_hash: hash(2),
        dataset_state_in: DatasetStateId(17),
        text: "summarize price mpg".to_owned(),
    });
    out.push(EngineEvent::Output {
        seq: s.next(),
        exec: e2,
        stream: stratum_proto::engine::OutputStream::Results,
        runs: vec![
            txt("\n    Variable |        Obs        Mean    Std. dev.       Min        Max\n"),
            txt("-------------+---------------------------------------------------------\n"),
            txt("       price |"),
            res("         74    6165.257    2949.496       3291      15906"),
            txt("\n         mpg |"),
            res("         74     21.2973    5.785503         12         41"),
            txt("\n"),
        ],
    });
    out.push(EngineEvent::Result {
        seq: s.next(),
        exec: e2,
        envelope: ResultEnvelope {
            result: ResultId(2),
            revision: 0,
            exec: e2,
            block: Some(B_SUMMARIZE),
            dataset_state: DatasetStateId(17),
            code_hash: hash(2),
            cmdline: "summarize price mpg".to_owned(),
            started_at_ms: 1_755_000_001_000,
            duration_us: 1_204,
            rc: 0,
            payloads: vec![ResultPayload::Summarize(summarize_payload())],
            raw: raw_ref(MOCK_SESSION, ResultId(2), SUMMARIZE_CLASSIC),
            layout_hint: LayoutHint {
                rows: 2,
                cols: 6,
                est_px: 132,
            },
            actions: vec![CardAction::CopyTable, CardAction::RawOutput],
        },
    });
    out.push(EngineEvent::BlockFinished {
        seq: s.next(),
        run: RunId(2),
        exec: e2,
        block: B_SUMMARIZE,
        result: Some(ResultId(2)),
        status: ExecStatus::Succeeded,
        rc: 0,
        duration_us: 1_204,
        dataset_state_out: DatasetStateId(17),
    });
    out.push(EngineEvent::StatusChanged {
        seq: s.next(),
        doc: MOCK_DOC,
        changed: vec![(
            B_SUMMARIZE,
            BlockStatus::Current {
                exec: e2,
                dataset: DatasetStateId(17),
                duration_us: 1_204,
            },
        )],
    });
    out.push(EngineEvent::RunFinished {
        seq: s.next(),
        run: RunId(2),
        rc: 0,
        blocks_run: 1,
        blocks_failed: 0,
        duration_us: 1_400,
        finished_at_ms: 1_755_000_001_001,
    });

    // --- run 3: regress price mpg weight foreign ----------------------------
    let e3 = ExecutionId(3);
    out.push(EngineEvent::RunStarted {
        seq: s.next(),
        schema: STREAM_SCHEMA,
        run: RunId(3),
        session: MOCK_SESSION,
        stratum_version: "0.1.0-mock".to_owned(),
        source: Some(camino::Utf8PathBuf::from("auto.do")),
        clean_state: false,
        cwd: camino::Utf8PathBuf::from("/Users/ana/proj"),
        started_at_ms: 1_755_000_002_000,
        seed: None,
        plan_len: 1,
    });
    out.push(EngineEvent::BlockStarted {
        seq: s.next(),
        run: RunId(3),
        exec: e3,
        block: B_REGRESS,
        doc: Some(MOCK_DOC),
        span: Span { start: 41, end: 73 },
        code_hash: hash(3),
        dataset_state_in: DatasetStateId(17),
        text: "regress price mpg weight foreign".to_owned(),
    });
    out.push(EngineEvent::Progress {
        seq: s.next(),
        exec: e3,
        done: 74,
        total: Some(74),
        label: "accumulating cross-products".to_owned(),
    });
    out.push(EngineEvent::Output {
        seq: s.next(),
        exec: e3,
        stream: stratum_proto::engine::OutputStream::Results,
        runs: vec![txt(REGRESS_CLASSIC)],
    });
    out.push(EngineEvent::Result {
        seq: s.next(),
        exec: e3,
        envelope: ResultEnvelope {
            result: ResultId(3),
            revision: 0,
            exec: e3,
            block: Some(B_REGRESS),
            dataset_state: DatasetStateId(17),
            code_hash: hash(3),
            cmdline: "regress price mpg weight foreign".to_owned(),
            started_at_ms: 1_755_000_002_000,
            duration_us: 3_180,
            rc: 0,
            payloads: vec![ResultPayload::Estimation(estimation_payload())],
            raw: raw_ref(MOCK_SESSION, ResultId(3), REGRESS_CLASSIC),
            layout_hint: LayoutHint {
                rows: 4,
                cols: 7,
                est_px: 320,
            },
            actions: vec![
                CardAction::CopyTable,
                CardAction::PlotCoefficients,
                CardAction::RawOutput,
            ],
        },
    });
    out.push(EngineEvent::BlockFinished {
        seq: s.next(),
        run: RunId(3),
        exec: e3,
        block: B_REGRESS,
        result: Some(ResultId(3)),
        status: ExecStatus::Succeeded,
        rc: 0,
        duration_us: 3_180,
        dataset_state_out: DatasetStateId(17),
    });
    out.push(EngineEvent::StatusChanged {
        seq: s.next(),
        doc: MOCK_DOC,
        changed: vec![(
            B_REGRESS,
            BlockStatus::Current {
                exec: e3,
                dataset: DatasetStateId(17),
                duration_us: 3_180,
            },
        )],
    });
    out.push(EngineEvent::RunFinished {
        seq: s.next(),
        run: RunId(3),
        rc: 0,
        blocks_run: 1,
        blocks_failed: 0,
        duration_us: 3_400,
        finished_at_ms: 1_755_000_002_003,
    });
    out
}

// ---------------------------------------------------------------------------
// The mock engine itself
// ---------------------------------------------------------------------------

/// What the mock observed, for tests that need to assert on it.
#[derive(Debug, Default)]
pub struct MockStats {
    pub requests: AtomicU64,
    pub cancels: AtomicU64,
    /// Set once the mock has begun replaying and not yet finished.
    pub replaying: AtomicBool,
    pub events_sent: AtomicU64,
}

/// Serve one connection. `reader`/`writer` are the engine's end of the pipe:
/// the desktop's [`crate::transport::Transport`] is on the other side, so every
/// byte in between is the production format.
pub async fn serve<R, W>(
    reader: R,
    writer: W,
    opts: MockOptions,
    stats: Arc<MockStats>,
    ledger: Arc<BulkCopyLedger>,
) -> Result<(), TransportError>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (out_tx, mut out_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
    let pump = tokio::spawn(async move {
        let mut writer = writer;
        while let Some(bytes) = out_rx.recv().await {
            if writer.write_all(&bytes).await.is_err() || writer.flush().await.is_err() {
                break;
            }
        }
    });

    let cancelled = Arc::new(AtomicBool::new(false));
    let mut reader = reader;
    let mut frames = FrameReader::new();
    let mut chunk = vec![0_u8; 64 * 1024];
    let mut bulk = BulkWriter::new(opts.bulk_dir.clone(), Arc::clone(&ledger));

    'outer: loop {
        let n = reader.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        frames.feed(&chunk[..n]);
        while let Some(frame) = frames.next_frame()? {
            match frame.kind {
                FrameKind::Ping => {
                    let ping: Ping = rmp_serde::from_slice(&frame.payload).map_err(|source| {
                        TransportError::Decode {
                            what: "Ping",
                            source,
                        }
                    })?;
                    if !ping.pong {
                        send(
                            &out_tx,
                            FrameKind::Ping,
                            frame.corr,
                            &Ping {
                                nonce: ping.nonce,
                                pong: true,
                            },
                        )?;
                    }
                }
                FrameKind::Request => {
                    let req: EngineRequest =
                        rmp_serde::from_slice(&frame.payload).map_err(|source| {
                            TransportError::Decode {
                                what: "EngineRequest",
                                source,
                            }
                        })?;
                    stats.requests.fetch_add(1, Ordering::Relaxed);
                    if matches!(req, EngineRequest::Shutdown) {
                        send(
                            &out_tx,
                            FrameKind::Response,
                            frame.corr,
                            &EngineResponse::Ok,
                        )?;
                        break 'outer;
                    }
                    handle(
                        &req, frame.corr, &out_tx, &opts, &stats, &cancelled, &mut bulk,
                    )
                    .await?;
                }
                // A response or an event arriving at the engine is a desync.
                _ => return Err(TransportError::UnexpectedKind),
            }
        }
    }
    drop(out_tx);
    let _ = pump.await;
    Ok(())
}

fn send<T: serde::Serialize>(
    tx: &tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
    kind: FrameKind,
    corr: u32,
    body: &T,
) -> Result<(), TransportError> {
    let payload = encode_body("mock frame", body)?;
    let mut buf = Vec::with_capacity(payload.len() + 16);
    encode_frame(kind, corr, &payload, &mut buf)?;
    tx.send(buf).map_err(|_| TransportError::Closed)
}

#[allow(clippy::too_many_lines)]
async fn handle(
    req: &EngineRequest,
    corr: u32,
    tx: &tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
    opts: &MockOptions,
    stats: &Arc<MockStats>,
    cancelled: &Arc<AtomicBool>,
    bulk: &mut BulkWriter,
) -> Result<(), TransportError> {
    match req {
        EngineRequest::Hello { .. } => send(
            tx,
            FrameKind::Response,
            corr,
            &EngineResponse::Hello {
                engine: "stratum-mock".to_owned(),
                schema: STREAM_SCHEMA,
                target: std::env::consts::ARCH.to_owned(),
            },
        ),
        EngineRequest::SessionOpen { .. } => send(
            tx,
            FrameKind::Response,
            corr,
            &EngineResponse::SessionOpened {
                session: MOCK_SESSION,
                epoch: SessionEpoch(1),
            },
        ),
        EngineRequest::Blocks { .. } | EngineRequest::DocOpen { .. } => send(
            tx,
            FrameKind::Response,
            corr,
            &EngineResponse::BlockMap(scenario_a_block_map()),
        ),
        EngineRequest::CompletionEnv { .. } => send(
            tx,
            FrameKind::Response,
            corr,
            &EngineResponse::CompletionEnv(completion_env()),
        ),
        EngineRequest::DataPage { request, .. } => {
            let bytes = bulk.build_page(request.nrows, request.cols.len().max(1))?;
            send(
                tx,
                FrameKind::Response,
                corr,
                &EngineResponse::Bulk { bulk: bytes },
            )
        }
        EngineRequest::ExecCancel { .. } => {
            stats.cancels.fetch_add(1, Ordering::Relaxed);
            if opts.behaviour == MockBehaviour::Uninterruptible {
                // Deliberate silence. The supervisor must escalate on a timer,
                // not on a reply it will never get.
                return Ok(());
            }
            cancelled.store(true, Ordering::SeqCst);
            send(tx, FrameKind::Response, corr, &EngineResponse::Ok)
        }
        EngineRequest::ExecSubmit { .. } => {
            let run = RunId(1);
            send(
                tx,
                FrameKind::Response,
                corr,
                &EngineResponse::Submitted {
                    plan: RunPlan {
                        run,
                        items: vec![PlanItem {
                            block: B_SUMMARIZE,
                            span: Span { start: 20, end: 39 },
                            code_hash: hash(2),
                            reason: PlanReason::Requested,
                        }],
                        epoch_reset: false,
                        clean_state: false,
                        skipped: Vec::new(),
                        stale_upstream: Vec::new(),
                    },
                },
            )?;
            replay(tx, opts, stats, cancelled).await
        }
        // Everything else acks. The mock's job is to keep the UI moving, not to
        // pretend to be an engine.
        _ => send(tx, FrameKind::Response, corr, &EngineResponse::Ok),
    }
}

/// Replay the canned stream as unsolicited event frames.
async fn replay(
    tx: &tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
    opts: &MockOptions,
    stats: &Arc<MockStats>,
    cancelled: &Arc<AtomicBool>,
) -> Result<(), TransportError> {
    cancelled.store(false, Ordering::SeqCst);
    stats.replaying.store(true, Ordering::SeqCst);
    let crash_at = opts.script.len() / 2;
    for (i, ev) in opts.script.iter().enumerate() {
        if opts.behaviour == MockBehaviour::CrashMidRun && i == crash_at {
            // Drop the pipe mid-frame-boundary: EOF with a run outstanding.
            stats.replaying.store(false, Ordering::SeqCst);
            return Err(TransportError::Closed);
        }
        if opts.behaviour != MockBehaviour::Uninterruptible && cancelled.load(Ordering::SeqCst) {
            break;
        }
        send(tx, FrameKind::Event, CORR_UNSOLICITED, ev)?;
        stats.events_sent.fetch_add(1, Ordering::Relaxed);
        if !opts.pace.is_zero() {
            tokio::time::sleep(opts.pace).await;
        }
    }
    stats.replaying.store(false, Ordering::SeqCst);
    Ok(())
}

// ---------------------------------------------------------------------------
// Bulk: the engine half of §10's segment ring
// ---------------------------------------------------------------------------

/// Writes SDP1 pages into an mmap segment. The engine side of the two-copy
/// budget: building the page **into** the mapping is copy 1, and it is the only
/// copy this side is allowed.
pub struct BulkWriter {
    dir: std::path::PathBuf,
    segment: u32,
    epoch: u64,
    cursor: u64,
    map: Option<memmap2::MmapMut>,
    file: Option<std::fs::File>,
    ledger: Arc<BulkCopyLedger>,
}

/// 256 MiB, §10's stated default.
pub const SEGMENT_BYTES: u64 = 256 * 1024 * 1024;

impl BulkWriter {
    #[must_use]
    pub fn new(dir: Option<std::path::PathBuf>, ledger: Arc<BulkCopyLedger>) -> Self {
        Self {
            dir: dir.unwrap_or_else(std::env::temp_dir),
            segment: 0,
            epoch: 1,
            cursor: 0,
            map: None,
            file: None,
            ledger,
        }
    }

    #[must_use]
    pub fn segment_path(&self) -> std::path::PathBuf {
        crate::transport::BulkSegments::segment_path(&self.dir, MOCK_SESSION.0, self.segment)
    }

    fn ensure_segment(&mut self) -> Result<(), TransportError> {
        if self.map.is_some() {
            return Ok(());
        }
        let path = self.segment_path();
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)?;
        file.set_len(SEGMENT_BYTES)?;
        // SAFETY: this process is the only writer of this file, and it never
        // truncates it after mapping (§10: a segment is retired whole, by
        // bumping `epoch`).
        let map = unsafe { memmap2::MmapMut::map_mut(&file) }?;
        self.map = Some(map);
        self.file = Some(file);
        Ok(())
    }

    /// Build a `RenderMode::Edit` SDP1 page of `nrows` × `ncols` f64 columns
    /// directly into the segment, and return the `BulkRef` that names it.
    pub fn build_page(&mut self, nrows: u32, ncols: usize) -> Result<BulkRef, TransportError> {
        self.ensure_segment()?;
        let start = self.cursor;
        let header = sdp1_header(nrows, ncols);
        let map = self.map.as_mut().expect("segment mapped above");
        let mut at = start as usize;
        let total = sdp1_len(nrows, ncols);
        assert!(
            start + total <= SEGMENT_BYTES,
            "mock segment ring wrap is out of scope; W07 sizes one segment per page"
        );

        map[at..at + 4].copy_from_slice(b"SDP1");
        at += 4;
        map[at..at + 4].copy_from_slice(&(header.len() as u32).to_le_bytes());
        at += 4;
        map[at..at + header.len()].copy_from_slice(header.as_bytes());
        at += header.len();
        for c in 0..ncols {
            // Values are written straight into the mapping — no staging Vec.
            // That is what makes this ONE copy rather than two.
            for row in 0..nrows as usize {
                let v = (c * 1_000_000 + row) as f64;
                map[at..at + 8].copy_from_slice(&v.to_le_bytes());
                at += 8;
            }
            for _ in 0..nrows as usize {
                map[at] = TAG_PRESENT;
                at += 1;
            }
        }
        self.cursor = at as u64;
        self.ledger.record_engine_to_mmap(total);
        Ok(BulkRef {
            segment: self.segment,
            offset: start,
            len: total,
            epoch: self.epoch,
        })
    }
}

/// `255 = not missing` (§8.1).
pub const TAG_PRESENT: u8 = 255;

fn sdp1_header(nrows: u32, ncols: usize) -> String {
    let mut cols = String::new();
    let mut off = 0_u64;
    for c in 0..ncols {
        if c > 0 {
            cols.push(',');
        }
        let data_len = u64::from(nrows) * 8;
        let aux_len = u64::from(nrows);
        cols.push_str(&format!(
            r#"{{"idx":{c},"kind":"num","off":{off},"len":{data_len},"aux_off":{},"aux_len":{aux_len}}}"#,
            off + data_len
        ));
        off += data_len + aux_len;
    }
    let mut h = format!(r#"{{"state":17,"row0":0,"nrows":{nrows},"seq":1,"cols":[{cols}]}}"#);
    // §8.1's decoder builds `new Float64Array(buf, 8 + H, n)`, which throws
    // unless the payload starts 8-aligned. Pad the header, never the payload.
    while (8 + h.len()) % 8 != 0 {
        h.push(' ');
    }
    h
}

fn sdp1_len(nrows: u32, ncols: usize) -> u64 {
    let header = sdp1_header(nrows, ncols).len() as u64;
    8 + header + ncols as u64 * (u64::from(nrows) * 9)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_path() -> std::path::PathBuf {
        repo_root()
            .expect("the repo root is findable from CARGO_MANIFEST_DIR")
            .join(SCENARIO_A_FIXTURE)
    }

    /// §7's framing guarantees, asserted against the canned stream itself. A
    /// mock that violates them teaches the frontend the wrong invariants, which
    /// is worse than no mock at all.
    #[test]
    fn scenario_a_obeys_the_section_7_framing_guarantees() {
        let script = scenario_a();
        let mut seq = 0;
        let mut open_run: Option<RunId> = None;
        let mut open_block = false;
        let mut runs = 0;

        for ev in &script {
            let s = crate::transport::event_seq(ev);
            assert_eq!(s, seq + 1, "seq must be strictly increasing by one");
            seq = s;
            match ev {
                EngineEvent::RunStarted { run, .. } => {
                    assert!(open_run.is_none(), "a run started inside a run");
                    open_run = Some(*run);
                    runs += 1;
                }
                EngineEvent::BlockStarted { run, .. } => {
                    assert_eq!(open_run, Some(*run), "block outside its run");
                    assert!(!open_block, "BlockStarted pairs must not interleave");
                    open_block = true;
                }
                EngineEvent::BlockFinished { run, .. } => {
                    assert_eq!(open_run, Some(*run));
                    assert!(open_block, "BlockFinished without BlockStarted");
                    open_block = false;
                }
                EngineEvent::RunFinished { run, .. } => {
                    assert_eq!(open_run, Some(*run));
                    assert!(!open_block, "a run finished with a block still open");
                    open_run = None;
                }
                _ => {}
            }
        }
        assert!(open_run.is_none(), "the last run never finished");
        assert_eq!(runs, 3, "scenario A is load, summarize, regress");
    }

    /// The numbers in the canned cards are StataMP 18.5's, not ours. If the
    /// golden log moves, this fails and the fixture is regenerated — which is
    /// the only way a mock stays a useful target for W14's renderers.
    #[test]
    fn scenario_a_classic_text_is_verbatim_from_the_golden_log() {
        let log = std::fs::read_to_string(
            repo_root()
                .expect("repo root")
                .join("tests/golden/stata18/core_surface.log"),
        )
        .expect("the golden capture is committed");
        for (what, block) in [
            ("summarize", SUMMARIZE_CLASSIC),
            ("regress", REGRESS_CLASSIC),
        ] {
            for line in block.lines().filter(|l| !l.trim().is_empty()) {
                assert!(
                    log.contains(line),
                    "{what}: this line is not in the golden capture:\n{line}"
                );
            }
        }
        // And the payload's pre-formatted strings are the same digits the
        // classic text prints (A6: a renderer never reformats).
        let sum = summarize_payload();
        assert!(SUMMARIZE_CLASSIC.contains(&sum.rows[0].display.mean));
        assert!(SUMMARIZE_CLASSIC.contains(&sum.rows[1].display.sd));
        let est = estimation_payload();
        for t in &est.terms {
            for d in &t.display_num[..2] {
                assert!(
                    REGRESS_CLASSIC.contains(d.as_str()),
                    "{d} missing from the table"
                );
            }
        }
    }

    #[test]
    fn the_stream_round_trips_through_the_real_frame_reader() {
        let script = scenario_a();
        let bytes = encode_stream(&script).unwrap();
        assert_eq!(decode_stream(&bytes).unwrap(), script);

        // Byte-at-a-time, because a fixture loaded in one read proves nothing
        // about a fixture arriving down a pipe.
        let mut reader = FrameReader::new();
        let mut got = Vec::new();
        for b in &bytes {
            reader.feed(std::slice::from_ref(b));
            while let Some(f) = reader.next_frame().unwrap() {
                got.push(rmp_serde::from_slice::<EngineEvent>(&f.payload).unwrap());
            }
        }
        reader.end_of_stream().unwrap();
        assert_eq!(got, script);
    }

    /// The committed fixture is generated from [`scenario_a`] and diffed, the
    /// same contract `stratum-tokens`' generated source has: the bytes in git
    /// are the artifact, and this test is what stops them drifting from the
    /// code that produced them.
    #[test]
    fn committed_fixture_matches_the_script() {
        let path = fixture_path();
        let want = encode_stream(&scenario_a()).unwrap();
        if !path.exists() {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &want).unwrap();
        }
        let have = std::fs::read(&path).unwrap();
        assert_eq!(
            have.len(),
            want.len(),
            "{} is stale; delete it and re-run this test to regenerate",
            path.display()
        );
        assert!(
            have == want,
            "{} is stale; delete it and re-run",
            path.display()
        );
        assert_eq!(decode_stream(&have).unwrap(), scenario_a());
    }

    #[test]
    fn sdp1_payload_starts_eight_byte_aligned() {
        for (nrows, ncols) in [(1_u32, 1_usize), (40, 12), (4096, 3)] {
            let h = sdp1_header(nrows, ncols);
            assert_eq!(
                (8 + h.len()) % 8,
                0,
                "a Float64Array view over an unaligned offset throws"
            );
            assert!(serde_json::from_str::<serde_json::Value>(h.trim()).is_ok());
        }
    }
}
