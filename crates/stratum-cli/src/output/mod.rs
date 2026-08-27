//! How a stream reaches the user: NDJSON ([`json`]), a classic Stata log
//! ([`text`]), the stderr chatter ([`human`]), and §7.2's normalizer
//! ([`deterministic`]).
//!
//! The one thing that lives *here* rather than in one of those is
//! [`FramingGuard`]. CONTRACTS §7's framing guarantees are what a consumer is
//! allowed to rely on — "exactly one `RunStarted` first and one `RunFinished`
//! last per run", "`BlockStarted`…`BlockFinished` never interleave", "`seq` is
//! strictly increasing" — and W09's acceptance is stated in exactly those terms.
//! Testing them after the fact, on a golden, only proves them for the streams
//! somebody wrote a golden for. Checking them **as the bytes are written**
//! proves them for every stream the binary will ever emit, including the ones a
//! replayed capture or a future engine produces, and turns a violation into
//! exit 9 (internal error, always a bug) instead of a malformed pipe that `jq`
//! reports three commands later.
//!
//! The guard is not free, and it is meant not to be: it is O(1) per event, with
//! no allocation and no copy of the payload.

pub mod deterministic;
pub mod human;
pub mod json;
pub mod text;

use stratum_proto::engine::EngineEvent;
use stratum_proto::exec::ExecStatus;
use stratum_proto::ids::{BlockId, ExecutionId, RunId};

use crate::cli::{RunOutcome, RC_UNSUPPORTED};

/// A stream that broke CONTRACTS §7. Always a bug in whoever produced it.
#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum FramingError {
    #[error("seq went backwards: {seq} after {previous} (CONTRACTS §7 g5 — seq is strictly increasing per session)")]
    SeqNotIncreasing { previous: u64, seq: u64 },
    #[error("run {new} started while run {open} was still open (CONTRACTS §7 g1)")]
    NestedRun { open: u64, new: u64 },
    #[error("{what} arrived with no run open (CONTRACTS §7 g1 — RunStarted is always first)")]
    OutsideRun { what: &'static str },
    #[error("RunFinished for run {got} while run {open} was open (CONTRACTS §7 g1)")]
    RunMismatch { open: u64, got: u64 },
    #[error("block {new} started while block {open} was still running (CONTRACTS §7 g2 — pairs never interleave)")]
    InterleavedBlock { open: u64, new: u64 },
    #[error(
        "BlockFinished for execution {got} while execution {open} was running (CONTRACTS §7 g2)"
    )]
    BlockMismatch { open: u64, got: u64 },
    #[error("the stream ended with run {open} still open (CONTRACTS §7 g1 — RunFinished is always last, including on error, interrupt and timeout)")]
    UnfinishedRun { open: u64 },
    #[error("the stream ended with block {open} still running (CONTRACTS §7 g2)")]
    UnfinishedBlock { open: u64 },
}

/// CONTRACTS §7's framing guarantees, enforced as the stream is written.
///
/// Guarantees 3 (byte order within `Output`), 4 (stdout carries only NDJSON) and
/// 6 (additive-only) are not checkable here: 3 is a property of the producer's
/// chunking, 4 is a property of who owns the file handle — which is why every
/// sink in this module takes its writer as an argument instead of reaching for
/// `println!` — and 6 is a property of the schema, not of a run.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct FramingGuard {
    last_seq: Option<u64>,
    open_run: Option<RunId>,
    open_block: Option<(ExecutionId, BlockId)>,
    runs_started: u32,
    runs_finished: u32,
}

impl FramingGuard {
    /// A guard over a fresh stream.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Admit one event, or refuse it.
    ///
    /// # Errors
    /// Any violation of guarantee 1, 2 or 5.
    pub fn admit(&mut self, ev: &EngineEvent) -> Result<(), FramingError> {
        let seq = event_seq(ev);
        if let Some(previous) = self.last_seq {
            if seq <= previous {
                return Err(FramingError::SeqNotIncreasing { previous, seq });
            }
        }
        self.last_seq = Some(seq);

        match ev {
            EngineEvent::RunStarted { run, .. } => {
                if let Some(open) = self.open_run {
                    return Err(FramingError::NestedRun {
                        open: open.0,
                        new: run.0,
                    });
                }
                self.open_run = Some(*run);
                self.runs_started += 1;
            }
            EngineEvent::BlockStarted { exec, block, .. } => {
                if self.open_run.is_none() {
                    return Err(FramingError::OutsideRun {
                        what: "BlockStarted",
                    });
                }
                if let Some((open, _)) = self.open_block {
                    return Err(FramingError::InterleavedBlock {
                        open: open.0,
                        new: exec.0,
                    });
                }
                self.open_block = Some((*exec, *block));
            }
            EngineEvent::BlockFinished { exec, .. } => {
                let Some((open, _)) = self.open_block else {
                    return Err(FramingError::OutsideRun {
                        what: "BlockFinished",
                    });
                };
                if open != *exec {
                    return Err(FramingError::BlockMismatch {
                        open: open.0,
                        got: exec.0,
                    });
                }
                self.open_block = None;
            }
            EngineEvent::RunFinished { run, .. } => {
                let Some(open) = self.open_run else {
                    return Err(FramingError::OutsideRun {
                        what: "RunFinished",
                    });
                };
                if let Some((block, _)) = self.open_block {
                    return Err(FramingError::UnfinishedBlock { open: block.0 });
                }
                if open != *run {
                    return Err(FramingError::RunMismatch {
                        open: open.0,
                        got: run.0,
                    });
                }
                self.open_run = None;
                self.runs_finished += 1;
            }
            // Session-scoped events legitimately appear outside a run: the
            // engine announces health, block maps and staleness whether or not
            // anything is executing. A17 makes that explicit for StatusChanged,
            // which must arrive DURING a long command.
            _ => {}
        }
        Ok(())
    }

    /// The stream is over.
    ///
    /// # Errors
    /// A run or a block that was never closed — the failure mode guarantee 1
    /// exists to forbid, and the one a crashed engine produces.
    pub fn finish(&self) -> Result<(), FramingError> {
        if let Some((block, _)) = self.open_block {
            return Err(FramingError::UnfinishedBlock { open: block.0 });
        }
        if let Some(run) = self.open_run {
            return Err(FramingError::UnfinishedRun { open: run.0 });
        }
        Ok(())
    }

    /// Complete `RunStarted`…`RunFinished` pairs seen so far.
    ///
    /// Reported by the pump as a `tracing` counter (ADR-017) and asserted by the
    /// framing tests. It is *not* what enforces guarantee 1 — [`Self::admit`]
    /// and [`Self::finish`] do that, on every event — so nothing in the shipped
    /// path branches on it.
    #[must_use]
    pub fn runs_finished(&self) -> u32 {
        self.runs_finished
    }

    /// `RunStarted`s seen so far. See [`Self::runs_finished`].
    #[must_use]
    pub fn runs_started(&self) -> u32 {
        self.runs_started
    }

    /// The `seq` of the last admitted event, or `None` on an empty stream.
    ///
    /// Read by the pump when it has to synthesise the closing pair for a stream
    /// it truncated: guarantee 5 says `seq` is strictly increasing, so the
    /// synthetic events have to continue the engine's numbering rather than
    /// restart it.
    #[must_use]
    pub fn last_seq(&self) -> Option<u64> {
        self.last_seq
    }

    /// The run currently open, if any.
    #[must_use]
    pub fn open_run(&self) -> Option<RunId> {
        self.open_run
    }

    /// The block currently running, if any.
    #[must_use]
    pub fn open_block(&self) -> Option<(ExecutionId, BlockId)> {
        self.open_block
    }
}

/// Every event carries a `seq` (CONTRACTS §7, stamped before fan-out). This is
/// the only place that fact is spelled out, so a new variant that forgets it
/// fails to compile here rather than silently escaping guarantee 5.
#[must_use]
pub fn event_seq(ev: &EngineEvent) -> u64 {
    match ev {
        EngineEvent::RunStarted { seq, .. }
        | EngineEvent::BlockStarted { seq, .. }
        | EngineEvent::Output { seq, .. }
        | EngineEvent::OutputTruncated { seq, .. }
        | EngineEvent::Result { seq, .. }
        | EngineEvent::Diagnostic { seq, .. }
        | EngineEvent::Progress { seq, .. }
        | EngineEvent::StateChanged { seq, .. }
        | EngineEvent::BlockFinished { seq, .. }
        | EngineEvent::StatusChanged { seq, .. }
        | EngineEvent::BlockMapChanged { seq, .. }
        | EngineEvent::RunFinished { seq, .. }
        | EngineEvent::CompletionEnvChanged { seq, .. }
        | EngineEvent::EngineHealth { seq, .. } => *seq,
    }
}

/// Everything the exit code and the §4.3 summary line depend on, accumulated
/// while the stream is written.
///
/// **It never re-walks the stream.** An event is inspected once, on its way to
/// the writer, and what survives is eight machine words — which is what makes
/// `stratum run` on a script with a million `Output` events not accumulate a
/// million events in memory to count them afterwards.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Tally {
    /// Events handed to the sink.
    pub events: u64,
    /// `BlockFinished` seen.
    pub blocks_run: u32,
    /// `BlockFinished` whose status was not `Succeeded`.
    pub blocks_failed: u32,
    /// Blocks skipped by the plan.
    pub blocks_skipped: u32,
    /// Bytes of `Output` payload that passed through.
    pub output_bytes: u64,
    /// The outcome the exit code is computed from.
    pub outcome: RunOutcome,
    /// Total `duration_us` reported by `RunFinished`. Recorded, never asserted
    /// on (ADR-017).
    pub duration_us: u64,
}

impl Tally {
    /// Fold one event in. O(1), no allocation.
    pub fn observe(&mut self, ev: &EngineEvent) {
        self.events += 1;
        match ev {
            EngineEvent::Output { runs, .. } => {
                self.output_bytes += runs.iter().map(|r| r.text.len() as u64).sum::<u64>();
            }
            EngineEvent::BlockFinished { status, rc, .. } => {
                self.blocks_run += 1;
                match status {
                    ExecStatus::Succeeded => {}
                    ExecStatus::Interrupted { .. } => {
                        self.blocks_failed += 1;
                        self.outcome.interrupted = true;
                    }
                    ExecStatus::Skipped { .. } => {
                        self.blocks_run -= 1;
                        self.blocks_skipped += 1;
                    }
                    ExecStatus::Failed { .. } | ExecStatus::Queued | ExecStatus::Running => {
                        self.blocks_failed += 1;
                        if *rc == RC_UNSUPPORTED {
                            self.outcome.had_unsupported = true;
                        } else if *rc != 0 {
                            self.outcome.had_real_error = true;
                        }
                    }
                }
            }
            EngineEvent::RunFinished {
                rc, duration_us, ..
            } => {
                self.outcome.rc = *rc;
                self.duration_us += *duration_us;
            }
            _ => {}
        }
    }
}

#[cfg(test)]
pub(crate) mod fixture {
    //! The one real event stream this crate can test against.
    //!
    //! `tests/fixtures/mock/scenario_a.msgpack` is W07's committed capture of
    //! `auto.do` — `sysuse auto`, `summarize price mpg`,
    //! `regress price mpg weight foreign` — and **every number in it is StataMP
    //! 18.5's**, copied from `tests/golden/stata18/core_surface.log`. Using it
    //! here rather than a hand-written stream is the same argument W07 made for
    //! writing it in the first place: a renderer built against invented numbers
    //! is a renderer that has never seen a real column width, and a framing
    //! check built against an invented stream has never seen a real engine's
    //! event order.
    //!
    //! It is READ ONLY here. W07 owns the file and the script that generates it.

    use std::path::PathBuf;

    use stratum_proto::engine::EngineEvent;
    use stratum_proto::frame::{FrameKind, FrameReader};

    /// Repo root, from this crate's manifest directory.
    pub fn repo_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
    }

    /// Decode W07's capture with `stratum_proto::frame` — the same decoder the
    /// desktop points at a real `stratum serve`.
    pub fn scenario_a() -> Vec<EngineEvent> {
        let path = repo_root().join("tests/fixtures/mock/scenario_a.msgpack");
        let bytes = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("W07's committed fixture at {}: {e}", path.display()));
        let mut reader = FrameReader::new();
        reader.feed(&bytes);
        let mut out = Vec::new();
        while let Some(frame) = reader.next_frame().expect("the fixture is well framed") {
            assert_eq!(frame.kind, FrameKind::Event, "the capture is events only");
            out.push(rmp_serde::from_slice(&frame.payload).expect("an EngineEvent"));
        }
        reader.end_of_stream().expect("no truncated frame");
        out
    }
}

#[cfg(test)]
mod tests {
    use stratum_proto::engine::EngineHealth;
    use stratum_proto::ids::{DatasetStateId, DocumentId, RunId, SessionId, Span};

    use super::*;

    fn health(seq: u64) -> EngineEvent {
        EngineEvent::EngineHealth {
            seq,
            health: EngineHealth::Ready,
        }
    }

    fn run_started(seq: u64, run: u64) -> EngineEvent {
        EngineEvent::RunStarted {
            seq,
            schema: stratum_proto::engine::STREAM_SCHEMA,
            run: RunId(run),
            session: SessionId(1),
            stratum_version: "test".to_owned(),
            source: None,
            clean_state: true,
            cwd: camino::Utf8PathBuf::from("/tmp"),
            started_at_ms: 0,
            seed: None,
            plan_len: 1,
        }
    }

    fn run_finished(seq: u64, run: u64) -> EngineEvent {
        EngineEvent::RunFinished {
            seq,
            run: RunId(run),
            rc: 0,
            blocks_run: 1,
            blocks_failed: 0,
            duration_us: 0,
            finished_at_ms: 0,
        }
    }

    fn block_started(seq: u64, run: u64, exec: u64) -> EngineEvent {
        EngineEvent::BlockStarted {
            seq,
            run: RunId(run),
            exec: ExecutionId(exec),
            block: BlockId(exec),
            doc: Some(DocumentId(1)),
            span: Span { start: 0, end: 1 },
            code_hash: stratum_proto::ids::CodeHash([0; 16]),
            dataset_state_in: DatasetStateId(0),
            text: "display 2+2".to_owned(),
        }
    }

    fn block_finished(seq: u64, run: u64, exec: u64) -> EngineEvent {
        EngineEvent::BlockFinished {
            seq,
            run: RunId(run),
            exec: ExecutionId(exec),
            block: BlockId(exec),
            result: None,
            status: ExecStatus::Succeeded,
            rc: 0,
            duration_us: 0,
            dataset_state_out: DatasetStateId(0),
        }
    }

    #[test]
    fn a_well_formed_run_is_admitted() {
        let mut g = FramingGuard::new();
        for ev in [
            health(0),
            run_started(1, 7),
            block_started(2, 7, 1),
            block_finished(3, 7, 1),
            run_finished(4, 7),
        ] {
            g.admit(&ev).expect("well framed");
        }
        g.finish().expect("nothing left open");
        assert_eq!(g.runs_started(), 1);
        assert_eq!(g.runs_finished(), 1);
    }

    #[test]
    fn interleaved_block_pairs_are_refused() {
        let mut g = FramingGuard::new();
        g.admit(&run_started(1, 7)).unwrap();
        g.admit(&block_started(2, 7, 1)).unwrap();
        assert_eq!(
            g.admit(&block_started(3, 7, 2)),
            Err(FramingError::InterleavedBlock { open: 1, new: 2 })
        );
    }

    #[test]
    fn a_nested_run_is_refused() {
        let mut g = FramingGuard::new();
        g.admit(&run_started(1, 7)).unwrap();
        assert_eq!(
            g.admit(&run_started(2, 8)),
            Err(FramingError::NestedRun { open: 7, new: 8 })
        );
    }

    #[test]
    fn a_block_outside_a_run_is_refused() {
        let mut g = FramingGuard::new();
        assert_eq!(
            g.admit(&block_started(1, 7, 1)),
            Err(FramingError::OutsideRun {
                what: "BlockStarted"
            })
        );
    }

    #[test]
    fn seq_must_strictly_increase() {
        let mut g = FramingGuard::new();
        g.admit(&health(5)).unwrap();
        assert_eq!(
            g.admit(&health(5)),
            Err(FramingError::SeqNotIncreasing {
                previous: 5,
                seq: 5
            })
        );
        assert_eq!(
            g.admit(&health(4)),
            Err(FramingError::SeqNotIncreasing {
                previous: 5,
                seq: 4
            })
        );
    }

    /// A crashed engine leaves the run open. Guarantee 1 says `RunFinished` is
    /// always last "including on error, interrupt, and timeout", so this is a
    /// framing violation and not a normal end of stream.
    #[test]
    fn a_truncated_run_is_refused_at_end_of_stream() {
        let mut g = FramingGuard::new();
        g.admit(&run_started(1, 7)).unwrap();
        assert_eq!(g.finish(), Err(FramingError::UnfinishedRun { open: 7 }));

        let mut g = FramingGuard::new();
        g.admit(&run_started(1, 7)).unwrap();
        g.admit(&block_started(2, 7, 1)).unwrap();
        assert_eq!(g.finish(), Err(FramingError::UnfinishedBlock { open: 1 }));
    }

    #[test]
    fn a_run_cannot_finish_with_a_block_still_running() {
        let mut g = FramingGuard::new();
        g.admit(&run_started(1, 7)).unwrap();
        g.admit(&block_started(2, 7, 1)).unwrap();
        assert_eq!(
            g.admit(&run_finished(3, 7)),
            Err(FramingError::UnfinishedBlock { open: 1 })
        );
    }

    /// W07's committed capture, decoded and pushed through the guard. Three
    /// runs, thirty-odd events, real StataMP numbers — and the framing
    /// guarantees hold over all of it. This is guarantee 1, 2 and 5 asserted
    /// against a stream nobody wrote for this test.
    #[test]
    fn the_committed_engine_capture_obeys_every_framing_guarantee() {
        let events = fixture::scenario_a();
        assert!(events.len() > 20, "the capture decoded to {}", events.len());
        let mut g = FramingGuard::new();
        let mut tally = Tally::default();
        for ev in &events {
            g.admit(ev)
                .unwrap_or_else(|e| panic!("seq {}: {e}", event_seq(ev)));
            tally.observe(ev);
        }
        g.finish().expect("every run closed");
        assert_eq!(g.runs_started(), 3, "auto.do: sysuse, summarize, regress");
        assert_eq!(g.runs_started(), g.runs_finished());
        assert_eq!(tally.events, events.len() as u64);
        assert_eq!(tally.blocks_run, 3);
        assert_eq!(tally.blocks_failed, 0);
        assert_eq!(tally.outcome.rc, 0);
        assert_eq!(tally.outcome.exit_code(), crate::cli::ExitCode::Success);
        assert!(tally.output_bytes > 1_000, "{}", tally.output_bytes);
    }

    #[test]
    fn a_failed_block_with_rc_ten_is_incomplete_not_wrong() {
        let mut t = Tally::default();
        t.observe(&EngineEvent::BlockFinished {
            seq: 1,
            run: RunId(1),
            exec: ExecutionId(1),
            block: BlockId(1),
            result: None,
            status: ExecStatus::Failed {
                rc: RC_UNSUPPORTED,
                message: "unsupported".to_owned(),
                span: None,
            },
            rc: RC_UNSUPPORTED,
            duration_us: 0,
            dataset_state_out: DatasetStateId(0),
        });
        assert!(t.outcome.had_unsupported);
        assert!(!t.outcome.had_real_error);
        assert_eq!(t.outcome.exit_code(), crate::cli::ExitCode::Unsupported);
    }
}
