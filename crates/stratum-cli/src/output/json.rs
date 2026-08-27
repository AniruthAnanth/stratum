//! `--json` — CONTRACTS §7.1 NDJSON on stdout, and nothing else on stdout.
//!
//! Guarantee 4 ("stdout carries only the NDJSON stream; all logging, progress
//! and human chatter goes to stderr") is what makes `stratum run x.do --json |
//! jq` work in CI, and it is enforced structurally rather than by discipline:
//! [`JsonSink`] owns a writer handed to it, nothing in this crate calls
//! `println!`, and the `tracing` subscriber is installed with
//! `.with_writer(std::io::stderr)` in `main`.
//!
//! Guarantees 1, 2 and 5 are enforced by [`FramingGuard`] on the way in, so a
//! malformed stream becomes exit 9 at the event that broke it rather than a
//! `jq: parse error` three commands downstream.

use std::io::Write;

use stratum_proto::engine::EngineEvent;
use stratum_proto::frame::Envelope;

use crate::output::deterministic::Normalizer;
use crate::output::{FramingError, FramingGuard, Tally};
use crate::serve::ndjson::NdjsonWriter;

/// Anything that can stop a stream reaching the user.
#[derive(Debug, thiserror::Error)]
pub enum OutputError {
    /// The producer broke CONTRACTS §7. Always a bug; exit 9.
    #[error(transparent)]
    Framing(#[from] FramingError),
    /// The sink went away, or the disk did.
    #[error("writing the stream: {0}")]
    Io(#[from] std::io::Error),
    /// An event would not serialise. Always a bug; exit 9.
    #[error("serialising an event: {0}")]
    Serde(#[from] serde_json::Error),
}

/// Writes §7.1 lines, checking §7's framing guarantees as it goes.
pub struct JsonSink<W: Write> {
    out: NdjsonWriter<W>,
    guard: FramingGuard,
    tally: Tally,
    /// `Some` under `--deterministic` (CONTRACTS §7.2).
    normalizer: Option<Normalizer>,
    /// ADR-017 counter: JSON round trips performed. Exactly one per line under
    /// `--deterministic`, and **zero** without it — the normalizer is not a
    /// second pass bolted onto a stream that has already been serialised, which
    /// is the shape that would double the per-event cost of every run.
    round_trips: u64,
}

impl<W: Write> JsonSink<W> {
    /// A plain §7.1 sink.
    pub fn new(out: W) -> Self {
        Self {
            out: NdjsonWriter::new(out),
            guard: FramingGuard::new(),
            tally: Tally::default(),
            normalizer: None,
            round_trips: 0,
        }
    }

    /// A sink that applies §7.2's substitution table. `base` is the entry file's
    /// parent when the caller knows it; `None` lets the first `RunStarted` fix
    /// it, exactly as `xtask normalize-ndjson` does when handed a bare stream.
    pub fn deterministic(out: W, base: Option<String>) -> Self {
        let mut s = Self::new(out);
        s.normalizer = Some(match base {
            Some(b) => Normalizer::with_base(b),
            None => Normalizer::new(),
        });
        s
    }

    /// Write one event.
    ///
    /// # Errors
    /// A framing violation, a serialisation failure, or a write error.
    pub fn event(&mut self, ev: &EngineEvent) -> Result<(), OutputError> {
        self.guard.admit(ev)?;
        self.tally.observe(ev);
        match &mut self.normalizer {
            None => self.out.event(ev)?,
            Some(n) => {
                // One `to_value` and one `to_string`, which is byte-for-byte
                // what `xtask normalize-ndjson` does to a line it has parsed.
                // That is what makes `--deterministic` output a FIXED POINT of
                // the other implementation of §7.2 rather than merely similar
                // to it, and it is the property `xtask conformance` asserts.
                let mut body = serde_json::to_value(ev)?;
                n.normalize(&mut body);
                self.round_trips += 1;
                self.out.write(&Envelope::event(body))?;
            }
        }
        Ok(())
    }

    /// End of stream.
    ///
    /// # Errors
    /// A run or a block left open — guarantee 1 says `RunFinished` is last
    /// *including on error, interrupt and timeout*, so this is never normal.
    pub fn finish(&mut self) -> Result<(), OutputError> {
        self.guard.finish().map_err(Into::into)
    }

    /// What the exit code and the §4.3 summary read.
    #[must_use]
    pub fn tally(&self) -> &Tally {
        &self.tally
    }

    /// The same tally, for the pump's own verdict.
    ///
    /// Interrupt and timeout are decided by the CLI, not by the engine: the
    /// stream carries no event that says "the user pressed Ctrl-C at the shell",
    /// so [`crate::cli::RunOutcome::interrupted`] and `timed_out` are set here
    /// after the closing pair is written. Everything else in the tally is folded
    /// in by [`Tally::observe`] and is never written from outside.
    pub fn tally_mut(&mut self) -> &mut Tally {
        &mut self.tally
    }

    /// Lines actually written to stdout. Compared against [`Tally::events`] by
    /// the counter test: one event in, one line out, never a drop and never a
    /// duplicate.
    #[must_use]
    pub fn lines_written(&self) -> u64 {
        self.out.lines_written()
    }

    /// ADR-017 counter — see [`JsonSink::round_trips`].
    #[must_use]
    pub fn round_trips(&self) -> u64 {
        self.round_trips
    }

    /// The framing state, for callers that want to assert on it.
    #[must_use]
    pub fn guard(&self) -> &FramingGuard {
        &self.guard
    }
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;
    use crate::output::fixture;

    fn drive(events: &[EngineEvent], deterministic: bool) -> (String, u64, u64) {
        let mut buf: Vec<u8> = Vec::new();
        let (lines, trips) = {
            let mut sink = if deterministic {
                JsonSink::deterministic(&mut buf, Some("/Users/ana/proj".to_owned()))
            } else {
                JsonSink::new(&mut buf)
            };
            for ev in events {
                sink.event(ev).expect("the capture is well framed");
            }
            sink.finish().expect("every run closed");
            assert_eq!(sink.tally().events, events.len() as u64);
            (sink.lines_written(), sink.round_trips())
        };
        (String::from_utf8(buf).expect("UTF-8"), lines, trips)
    }

    /// **The acceptance bullet, on a real stream.** Exactly one `RunStarted`
    /// first and one `RunFinished` last per run, never interleaved
    /// `BlockStarted`/`BlockFinished`, stdout NDJSON only.
    #[test]
    fn the_stream_is_well_framed_ndjson_and_nothing_else() {
        let events = fixture::scenario_a();
        let (text, lines, trips) = drive(&events, false);

        assert_eq!(lines, events.len() as u64, "one event in, one line out");
        assert_eq!(trips, 0, "no JSON round trip without --deterministic");
        assert!(!text.starts_with('\u{feff}'), "no BOM");
        assert!(!text.contains('\r'), "LF only");
        assert!(text.ends_with('\n'), "every record is terminated");

        // Every line is a §7.1 envelope — this is what `| jq` relies on.
        let parsed: Vec<Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("`{l}`: {e}")))
            .collect();
        assert_eq!(parsed.len(), events.len());
        for v in &parsed {
            assert_eq!(v["v"], 1, "v == STREAM_SCHEMA");
            assert_eq!(v["t"], "event", "a run emits events, never requests");
            assert!(v.get("corr").is_none(), "§7.1: events carry no corr");
            assert!(v["body"].get("event").is_some(), "internally tagged");
        }
        for banned in ["jsonrpc", "\"method\"", "\"params\""] {
            assert!(
                !text.contains(banned),
                "A9: this is not JSON-RPC ({banned})"
            );
        }

        // Framing, read back off the bytes rather than off the guard.
        let kinds: Vec<&str> = parsed
            .iter()
            .map(|v| v["body"]["event"].as_str().unwrap_or_default())
            .collect();
        let starts = kinds.iter().filter(|k| **k == "run_started").count();
        let finishes = kinds.iter().filter(|k| **k == "run_finished").count();
        assert_eq!(starts, finishes, "one RunFinished per RunStarted");
        let mut depth = 0i32;
        let mut block_depth = 0i32;
        for k in &kinds {
            match *k {
                "run_started" => depth += 1,
                "run_finished" => depth -= 1,
                "block_started" => block_depth += 1,
                "block_finished" => block_depth -= 1,
                _ => {}
            }
            assert!((0..=1).contains(&depth), "runs never nest");
            assert!(
                (0..=1).contains(&block_depth),
                "block pairs never interleave"
            );
        }
        assert_eq!(depth, 0);
        assert_eq!(block_depth, 0);
    }

    /// §7.2's output must be a fixed point of `xtask normalize-ndjson`. We
    /// cannot link xtask from here, so this asserts the half that is provable
    /// in-crate: normalising our own normalised output changes nothing, and the
    /// fields §7.2 names are the only ones that moved.
    #[test]
    fn deterministic_output_is_a_fixed_point() {
        let events = fixture::scenario_a();
        let (raw, _, _) = drive(&events, false);
        let (norm, lines, trips) = drive(&events, true);

        assert_eq!(trips, lines, "exactly one round trip per line");
        assert_ne!(raw, norm, "the capture carries timestamps and a version");

        let mut n = Normalizer::new();
        let again: String = norm
            .lines()
            .map(|l| {
                let mut v: Value = serde_json::from_str(l).unwrap();
                n.normalize(&mut v);
                format!("{}\n", serde_json::to_string(&v).unwrap())
            })
            .collect();
        assert_eq!(again, norm, "normalising twice must change nothing");

        let first: Value = serde_json::from_str(
            norm.lines()
                .find(|l| l.contains(r#""event":"run_started""#))
                .expect("the capture has a run"),
        )
        .unwrap();
        assert_eq!(first["body"]["started_at_ms"], 0);
        assert_eq!(first["body"]["stratum_version"], "<version>");
        assert_eq!(first["body"]["cwd"], "<cwd>");
        assert_eq!(first["body"]["source"], "auto.do");
        assert_eq!(first["body"]["session"], 1, "SessionId is NOT normalised");
        assert_eq!(first["body"]["seq"], 4, "seq is NOT normalised");
    }

    /// Two runs of the same stream must be byte-identical under
    /// `--deterministic` — ARCHITECTURE §8.9, and the property `xtask
    /// conformance` compares across three operating systems.
    #[test]
    fn two_passes_over_one_capture_are_byte_identical() {
        let events = fixture::scenario_a();
        assert_eq!(drive(&events, true).0, drive(&events, true).0);
    }

    #[test]
    fn a_malformed_stream_is_refused_at_the_event_that_broke_it() {
        let mut events = fixture::scenario_a();
        // Drop the first RunFinished: the second RunStarted now nests.
        let at = events
            .iter()
            .position(|e| matches!(e, EngineEvent::RunFinished { .. }))
            .expect("the capture has one");
        events.remove(at);

        let mut buf: Vec<u8> = Vec::new();
        let mut sink = JsonSink::new(&mut buf);
        let err = events
            .iter()
            .try_for_each(|e| sink.event(e))
            .expect_err("a nested run must be refused");
        assert!(
            matches!(err, OutputError::Framing(FramingError::NestedRun { .. })),
            "{err}"
        );
    }

    /// Guarantee 1 again, from the other end: a stream that stops mid-run is a
    /// framing error at `finish`, not a clean EOF.
    #[test]
    fn a_truncated_stream_fails_at_finish() {
        let events = fixture::scenario_a();
        let at = events
            .iter()
            .position(|e| matches!(e, EngineEvent::BlockStarted { .. }))
            .expect("the capture has one");
        let mut buf: Vec<u8> = Vec::new();
        let mut sink = JsonSink::new(&mut buf);
        for ev in &events[..=at] {
            sink.event(ev).unwrap();
        }
        assert!(matches!(
            sink.finish(),
            Err(OutputError::Framing(FramingError::UnfinishedBlock { .. }))
        ));
    }
}
