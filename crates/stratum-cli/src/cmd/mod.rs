//! The verbs, and the seam every executing verb goes through.
//!
//! # The engine seam, and the blocker behind it
//!
//! `run` and `exec` drive a **stream of [`EngineEvent`]s** and know nothing else
//! about the engine. That is not an abstraction for its own sake: `serve`
//! (W07) already defines `EngineBackend` for the request/response direction, the
//! desktop already drives the same event stream over a pipe, and CONTRACTS §7's
//! framing guarantees are stated over the stream rather than over any particular
//! producer of it.
//!
//! **Nothing is linked to it yet, and [`ENGINE_LINKED`] says so.**
//! `crates/stratum-exec` (W08), `crates/stratum-session` (W08b) and
//! `crates/stratum-runtime` (W06) are being written in the same wave as this
//! crate; the engine is a three-crate stack — `exec` declares `SessionHost` as a
//! *port* that `session` implements over `runtime` — and taking a path
//! dependency on three manifests that are mid-edit would make this package's
//! build red for reasons no change here can fix, which R0's "build and test only
//! your own package" exists to prevent. The edge is therefore left to the
//! integration that owns all three, and the seam is closed here with two
//! producers that *are* real:
//!
//! * [`Engine::Replay`] — a recorded capture, decoded with
//!   `stratum_proto::frame` (CONTRACTS §10 frames) or read as §7.1 NDJSON. This
//!   is the CLI half of R2 "mock-first, not integration-last", and it is what
//!   lets a renderer, a CI pipeline or a bug report drive the real writer over a
//!   real engine stream with no engine present.
//! * [`Engine::Absent`] — the honest answer when nothing is linked: a
//!   well-framed one-block-less run carrying one `STRATUM0010` diagnostic and
//!   `rc = 10`, so `stratum run` reports **"we are incomplete"** (exit 10) and
//!   never "we are wrong" (exit 1).
//!
//! ## The edit that closes it
//!
//! Four things change, and nothing else:
//!
//! 1. `Cargo.toml` gains `stratum-exec = { path = "../stratum-exec" }`.
//! 2. [`Engine`] gains a `Linked(stratum_exec::…)` variant whose
//!    [`RunEngine::next_event`] is a `recv()` on the session worker's event
//!    channel.
//! 3. [`Engine::open`] prefers it over [`Engine::Absent`].
//! 4. [`ENGINE_LINKED`] becomes `true`, which is what `doctor` and `version`
//!    report and what stops `doctor` listing the gap as a problem.
//!
//! Nothing in `output/**`, in the exit ladder, or in the framing guard moves,
//! because none of it knows where an event came from. `cmd/serve.rs`'s
//! [`serve::CliBackend`] is replaced the same way and for the same reason: it is
//! an `EngineBackend`, and `crate::serve` does not know which one it has.
//!
//! Three things are already staged against that day and should be re-run first:
//! `tests/smoke/expected.jsonl` (its README says what will change),
//! `tests/conformance/staged/*.do` (move them up one directory), and
//! `cargo xtask conformance`, whose thread-count property nothing in the corpus
//! can currently exercise.

pub mod check;
pub mod completions;
pub mod data;
pub mod describe;
pub mod doctor;
pub mod exec;
pub mod fmt;
pub mod init;
pub mod run;
pub mod serve;
pub mod version;

use std::io::Read;

use camino::{Utf8Path, Utf8PathBuf};
use stratum_proto::diagnostic::{Confidence, Diagnostic, Severity};
use stratum_proto::engine::{EngineEvent, STREAM_SCHEMA};
use stratum_proto::frame::{FrameKind, FrameReader, WireTag};
use stratum_proto::ids::{RunId, SessionId};
use stratum_proto::UnixMs;

use crate::cli::{ExitCode, RC_UNSUPPORTED};
use crate::serve::ndjson::{Line, NdjsonReader};

/// A command failed in a way the exit ladder has a name for.
///
/// Carrying the [`ExitCode`] on the error is what keeps design 08 §4.4 from
/// being reimplemented once per verb: every command returns
/// `Result<ExitCode, CmdError>` and `main` maps both arms through the same
/// place.
#[derive(Debug, thiserror::Error)]
pub enum CmdError {
    /// Input missing/unreadable, output path unwritable. Exit 3.
    #[error("{path}: {source}")]
    Io {
        /// What we were touching.
        path: Utf8PathBuf,
        /// Why it failed.
        #[source]
        source: std::io::Error,
    },
    /// A write to stdout/stderr failed. Exit 3.
    #[error("writing output: {0}")]
    Output(#[from] crate::output::json::OutputError),
    /// A capture could not be decoded. Exit 3 — it is an input file.
    #[error("{path} is not a recorded engine stream: {why}")]
    BadCapture {
        /// The capture.
        path: Utf8PathBuf,
        /// What the decoder said.
        why: String,
    },
    /// The file was never executed. Exit 4.
    #[error("{path} did not parse; nothing was executed")]
    Parse {
        /// The offending file.
        path: Utf8PathBuf,
    },
    /// A syntactically valid construct we have not implemented. Exit 10.
    #[error("unsupported in this version: {0}")]
    Unsupported(String),
    /// An invariant failed. Exit 9; always a bug.
    #[error("internal error: {0}")]
    Internal(String),
}

impl CmdError {
    /// Design 08 §4.4.
    #[must_use]
    pub fn exit_code(&self) -> ExitCode {
        match self {
            CmdError::Io { .. } | CmdError::BadCapture { .. } => ExitCode::Io,
            CmdError::Output(crate::output::json::OutputError::Io(_)) => ExitCode::Io,
            CmdError::Output(_) => ExitCode::Internal,
            CmdError::Parse { .. } => ExitCode::Parse,
            CmdError::Unsupported(_) => ExitCode::Unsupported,
            CmdError::Internal(_) => ExitCode::Internal,
        }
    }
}

/// Read a file, mapping the failure onto exit 3.
///
/// # Errors
/// [`CmdError::Io`].
pub fn read_to_string(path: &Utf8Path) -> Result<String, CmdError> {
    std::fs::read_to_string(path).map_err(|source| CmdError::Io {
        path: path.to_owned(),
        source,
    })
}

/// Milliseconds since the Unix epoch. The wire's only time representation (A2).
#[must_use]
pub fn now_ms() -> UnixMs {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// The session `stratum run` allocates.
///
/// **Deliberately a constant.** CONTRACTS §7.2 normalises the session segment
/// inside an asset path to `S0` but leaves `SessionId` itself verbatim, which is
/// only consistent if a clean CLI run allocates a deterministic one. A random id
/// here would make `--deterministic` output differ between two runs of the same
/// file, which is the one thing the flag exists to prevent.
pub const CLI_SESSION: SessionId = SessionId(1);

/// The run `stratum run` allocates. Deterministic for the same reason.
pub const CLI_RUN: RunId = RunId(1);

/// A producer of engine events.
///
/// One method, because that is the whole contract `run` needs: CONTRACTS §7's
/// guarantees are enforced downstream by [`crate::output::FramingGuard`], so a
/// producer cannot make the CLI emit a malformed stream however badly it
/// behaves.
pub trait RunEngine {
    /// The next event, or `None` at end of stream. A real engine blocks here on
    /// its session worker's channel; that is the intended shape.
    fn next_event(&mut self) -> Option<EngineEvent>;
}

/// Where a run's events come from. See this module's header for the blocker.
pub enum Engine {
    /// A recorded capture, replayed verbatim.
    Replay(std::vec::IntoIter<EngineEvent>),
    /// Nothing is linked; report incompleteness in a well-framed stream.
    Absent(AbsentEngine),
}

impl RunEngine for Engine {
    fn next_event(&mut self) -> Option<EngineEvent> {
        match self {
            Engine::Replay(it) => it.next(),
            Engine::Absent(a) => a.next_event(),
        }
    }
}

impl Engine {
    /// Choose a producer for this run.
    ///
    /// # Errors
    /// [`CmdError::BadCapture`] if `--replay` names something that is not a
    /// recorded stream.
    pub fn open(replay: Option<&Utf8Path>, plan: RunShape) -> Result<Self, CmdError> {
        match replay {
            Some(path) => Ok(Engine::Replay(read_capture(path)?.into_iter())),
            None => Ok(Engine::Absent(AbsentEngine::new(plan))),
        }
    }
}

/// What `run` knows about the work before an engine sees it: the entry file, how
/// many executable regions it has, and the run-level settings that go into
/// `RunStarted`.
#[derive(Clone, Debug)]
pub struct RunShape {
    /// The entry `.do`, absolute, or `None` for `exec`.
    pub source: Option<Utf8PathBuf>,
    /// Working directory the run would execute in.
    pub cwd: Utf8PathBuf,
    /// Executable regions found by `stratum_parse::segment`. Trivia excluded.
    pub plan_len: u32,
    /// `--seed`.
    pub seed: Option<u64>,
    /// Always true for `run` (design 08 §4.1: there is no `--dirty`).
    pub clean_state: bool,
}

/// The producer used when no engine is linked.
///
/// It emits a **complete, well-framed run**: `RunStarted`, one `Diagnostic`, and
/// `RunFinished` with `rc = 10`. Guarantee 1 says `RunFinished` is always last
/// "including on error, interrupt and timeout", and "the engine is not in this
/// build" is exactly such a case — so the stream a consumer sees is the same
/// shape it will see from the real engine, and only the payload changes.
///
/// The one exception is a source with **no executable region**: there is nothing
/// an engine would have done, so the diagnostic is omitted and `rc` is 0. See
/// [`AbsentEngine::next_event`].
///
/// It allocates **no `BlockId`s**. CONTRACTS §2 reserves that to `stratum-exec`,
/// and a CLI that minted ids the engine would later mint differently would break
/// the id-drift comparison §7.2 exists for.
pub struct AbsentEngine {
    shape: RunShape,
    seq: u64,
    step: u8,
}

/// Is the execution engine linked into this build?
///
/// **A `const` and not a Cargo feature, deliberately.** The obvious spelling is
/// `cfg!(feature = "engine")`, but a feature that gates no dependency can be
/// switched on from the command line, and `stratum doctor`/`stratum version`
/// would then report an engine that is not there. That is the same shape as the
/// green-mark-by-inference `stratum-proto`'s repro header forbids, moved from a
/// run to a build. This constant flips to `true` in the *same commit* that adds
/// the `stratum-exec` edge and the `Engine::Linked` variant, and there is no
/// other way to flip it.
pub const ENGINE_LINKED: bool = false;

/// ADR-016's code for "unsupported in this version". The CLI raises it for the
/// same reason `set linesize 120` does: the construct is valid Stata and we have
/// not implemented it.
pub const CODE_UNSUPPORTED: &str = "STRATUM0010";

/// What the absent engine says. Spelled out once so the message a user sees and
/// the message a test asserts on cannot drift.
pub const ENGINE_ABSENT: &str = "the execution engine (crates/stratum-exec, work unit W08) \
                                 is not linked into this build; \
                                 `stratum run --replay <capture>` drives the stream from \
                                 a recorded engine capture in the meantime";

impl AbsentEngine {
    fn new(shape: RunShape) -> Self {
        Self {
            shape,
            seq: 0,
            step: 0,
        }
    }

    /// The diagnostic this engine raises, as `check` and `describe` also want to
    /// be able to name it.
    #[must_use]
    pub fn diagnostic(file: Option<Utf8PathBuf>) -> Diagnostic {
        Diagnostic {
            severity: Severity::Error,
            code: CODE_UNSUPPORTED.to_owned(),
            stata_rc: Some(RC_UNSUPPORTED),
            message: ENGINE_ABSENT.to_owned(),
            file,
            span: None,
            offending_token: None,
            block: None,
            related: Vec::new(),
            suggestions: Vec::new(),
            notes: Vec::new(),
            confidence: Confidence::Exact,
        }
    }

    fn next_event(&mut self) -> Option<EngineEvent> {
        // **A file with no executable region is not something we are incomplete
        // about.** `sysuse auto` needs an engine; a file of comments and section
        // markers does not, and a real engine will emit exactly this stream —
        // `RunStarted`, `RunFinished`, `rc = 0`, no blocks — for it too. Saying
        // `rc = 10` here would claim a missing feature where there is no
        // feature, and it would make `stratum run empty.do` exit 10 forever.
        // It is also what lets such a file serve as a conformance case today:
        // its output does not change on the day W08 is linked.
        let nothing_to_run = self.shape.plan_len == 0;
        loop {
            let step = self.step;
            self.step = step.saturating_add(1);
            let ev = match step {
                0 => EngineEvent::RunStarted {
                    seq: self.seq,
                    schema: STREAM_SCHEMA,
                    run: CLI_RUN,
                    session: CLI_SESSION,
                    stratum_version: env!("CARGO_PKG_VERSION").to_owned(),
                    source: self.shape.source.clone(),
                    clean_state: self.shape.clean_state,
                    cwd: self.shape.cwd.clone(),
                    started_at_ms: now_ms(),
                    seed: self.shape.seed,
                    plan_len: self.shape.plan_len,
                },
                1 if nothing_to_run => continue,
                1 => EngineEvent::Diagnostic {
                    seq: self.seq,
                    exec: None,
                    diagnostic: Self::diagnostic(self.shape.source.clone()),
                },
                2 => EngineEvent::RunFinished {
                    seq: self.seq,
                    run: CLI_RUN,
                    rc: if nothing_to_run { 0 } else { RC_UNSUPPORTED },
                    blocks_run: 0,
                    blocks_failed: 0,
                    duration_us: 0,
                    finished_at_ms: now_ms(),
                },
                _ => {
                    self.step = step;
                    return None;
                }
            };
            self.seq += 1;
            return Some(ev);
        }
    }
}

/// Decode a recorded engine stream.
///
/// Two encodings, sniffed rather than declared: CONTRACTS §10 frames (what
/// `tests/fixtures/mock/scenario_a.msgpack` holds — a length-prefixed frame
/// always starts with a little-endian `u32` length, so byte 0 of a non-empty
/// capture is never `{`) and §7.1 NDJSON. Sniffing beats a flag because the file
/// a user has is the file a user has.
///
/// # Errors
/// [`CmdError::Io`] if it cannot be read, [`CmdError::BadCapture`] if it does not
/// decode.
pub fn read_capture(path: &Utf8Path) -> Result<Vec<EngineEvent>, CmdError> {
    let bytes = std::fs::read(path).map_err(|source| CmdError::Io {
        path: path.to_owned(),
        source,
    })?;
    if bytes.first() == Some(&b'{') {
        decode_ndjson_capture(path, &bytes)
    } else {
        decode_framed_capture(path, &bytes)
    }
}

fn decode_framed_capture(path: &Utf8Path, bytes: &[u8]) -> Result<Vec<EngineEvent>, CmdError> {
    let mut reader = FrameReader::new();
    reader.feed(bytes);
    let mut out = Vec::new();
    loop {
        let frame = reader.next_frame().map_err(|e| CmdError::BadCapture {
            path: path.to_owned(),
            why: e.to_string(),
        })?;
        let Some(frame) = frame else { break };
        if frame.kind != FrameKind::Event {
            return Err(CmdError::BadCapture {
                path: path.to_owned(),
                why: format!(
                    "frame {:?} is not an event; a capture is events only",
                    frame.kind
                ),
            });
        }
        out.push(
            rmp_serde::from_slice(&frame.payload).map_err(|e| CmdError::BadCapture {
                path: path.to_owned(),
                why: e.to_string(),
            })?,
        );
    }
    reader.end_of_stream().map_err(|e| CmdError::BadCapture {
        path: path.to_owned(),
        why: e.to_string(),
    })?;
    Ok(out)
}

fn decode_ndjson_capture(path: &Utf8Path, bytes: &[u8]) -> Result<Vec<EngineEvent>, CmdError> {
    let mut reader = NdjsonReader::new(WireTag::Event);
    reader.feed(bytes);
    let mut out = Vec::new();
    // §7.1: "a reader that does not recognise `t` or `body`'s tag MUST skip the
    // line and continue". A capture from a newer engine is therefore replayable
    // as far as this build understands it, which is the whole point of the rule.
    while let Some(line) = reader
        .next_line::<EngineEvent>()
        .map_err(|e| CmdError::BadCapture {
            path: path.to_owned(),
            why: e.to_string(),
        })?
    {
        if let Line::Ok { body, .. } = line {
            out.push(body);
        }
    }
    reader.end_of_stream().map_err(|e| CmdError::BadCapture {
        path: path.to_owned(),
        why: e.to_string(),
    })?;
    Ok(out)
}

/// Read every byte of stdin. Used by `exec --stdin`.
///
/// # Errors
/// [`CmdError::Io`].
pub fn read_stdin() -> Result<String, CmdError> {
    let mut s = String::new();
    std::io::stdin()
        .read_to_string(&mut s)
        .map_err(|source| CmdError::Io {
            path: Utf8PathBuf::from("<stdin>"),
            source,
        })?;
    Ok(s)
}

/// Executable regions of a source — the plan length `RunStarted` announces.
///
/// `Trivia` is not executable and has no run affordance (CONTRACTS §2), so it is
/// not in the plan. This is the same predicate the editor gutter uses to decide
/// whether to draw a Run arrow.
#[must_use]
pub fn executable_region_count(seg: &stratum_parse::Segmentation<'_>) -> u32 {
    seg.regions
        .iter()
        .filter(|r| !matches!(r.kind, stratum_parse::RegionShape::Trivia { .. }))
        .count() as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::fixture;

    #[test]
    fn a_framed_capture_round_trips_through_the_sniffing_reader() {
        let path = fixture::repo_root().join("tests/fixtures/mock/scenario_a.msgpack");
        let path = Utf8PathBuf::from_path_buf(path).expect("utf-8 repo path");
        let events = read_capture(&path).expect("W07's committed capture");
        assert_eq!(events.len(), fixture::scenario_a().len());
    }

    #[test]
    fn an_ndjson_capture_round_trips_too() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(dir.path().join("cap.jsonl")).unwrap();
        let events = fixture::scenario_a();
        {
            let file = std::fs::File::create(&path).unwrap();
            let mut w = crate::output::json::JsonSink::new(file);
            for ev in &events {
                w.event(ev).unwrap();
            }
            w.finish().unwrap();
        }
        assert_eq!(read_capture(&path).unwrap().len(), events.len());
    }

    #[test]
    fn a_capture_that_is_not_one_is_an_input_error_not_a_panic() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(dir.path().join("nope.bin")).unwrap();
        std::fs::write(&path, b"\xff\xff\xff\xffnot a stream").unwrap();
        let err = read_capture(&path).expect_err("garbage is not a capture");
        assert_eq!(err.exit_code(), ExitCode::Io);
    }

    /// The absent engine still obeys CONTRACTS §7: one `RunStarted` first, one
    /// `RunFinished` last, seq strictly increasing, no `BlockId` minted.
    #[test]
    fn the_absent_engine_emits_a_well_framed_run() {
        let mut e = Engine::open(
            None,
            RunShape {
                source: Some(Utf8PathBuf::from("/p/hello.do")),
                cwd: Utf8PathBuf::from("/p"),
                plan_len: 2,
                seed: None,
                clean_state: true,
            },
        )
        .unwrap();

        let mut guard = crate::output::FramingGuard::new();
        let mut seen = Vec::new();
        while let Some(ev) = e.next_event() {
            guard.admit(&ev).expect("well framed");
            seen.push(ev);
        }
        guard.finish().expect("the run closed");
        assert_eq!(seen.len(), 3);
        assert!(matches!(
            seen[0],
            EngineEvent::RunStarted { plan_len: 2, .. }
        ));
        let EngineEvent::Diagnostic { diagnostic, .. } = &seen[1] else {
            panic!("expected a diagnostic")
        };
        assert_eq!(diagnostic.code, CODE_UNSUPPORTED);
        assert_eq!(diagnostic.stata_rc, Some(RC_UNSUPPORTED));
        assert!(matches!(
            seen[2],
            EngineEvent::RunFinished {
                rc: RC_UNSUPPORTED,
                blocks_run: 0,
                ..
            }
        ));
        assert!(
            !seen
                .iter()
                .any(|e| matches!(e, EngineEvent::BlockStarted { .. })),
            "CONTRACTS §2: only stratum-exec allocates BlockIds"
        );
    }

    /// A file with nothing to execute is a *complete* run, not an incomplete
    /// one — and it is the shape that lets `tests/conformance/**` exercise the
    /// §7.2 normalizer end-to-end today, because this stream is the one the real
    /// engine will emit for the same file.
    #[test]
    fn an_empty_plan_finishes_clean_rather_than_claiming_incompleteness() {
        let mut e = Engine::open(
            None,
            RunShape {
                source: Some(Utf8PathBuf::from("/p/notes.do")),
                cwd: Utf8PathBuf::from("/p"),
                plan_len: 0,
                seed: None,
                clean_state: true,
            },
        )
        .unwrap();
        let mut guard = crate::output::FramingGuard::new();
        let mut seen = Vec::new();
        while let Some(ev) = e.next_event() {
            guard.admit(&ev).expect("well framed");
            seen.push(ev);
        }
        guard.finish().expect("the run closed");
        assert_eq!(seen.len(), 2, "no diagnostic: nothing was missing");
        assert!(matches!(
            seen[0],
            EngineEvent::RunStarted {
                seq: 0,
                plan_len: 0,
                ..
            }
        ));
        assert!(matches!(
            seen[1],
            EngineEvent::RunFinished {
                seq: 1,
                rc: 0,
                blocks_run: 0,
                ..
            }
        ));
        let mut tally = crate::output::Tally::default();
        for ev in &seen {
            tally.observe(ev);
        }
        assert_eq!(tally.outcome.exit_code(), ExitCode::Success);
    }

    #[test]
    fn trivia_is_not_in_the_plan() {
        let src = "* a comment\n\ndisplay 2+2\n// another\nsummarize price\n";
        let seg = stratum_parse::segment(src);
        assert_eq!(executable_region_count(&seg), 2, "{:#?}", seg.regions);
    }
}
