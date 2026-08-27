//! `--format text` — a faithful classic Stata log on stdout (spec §17: "View
//! raw/classic output. Compatibility is never hidden").
//!
//! **Everything goes through [`stratum_proto::styled::to_plain`]** (A12). The
//! statistics crates return `Vec<StyledRun>` rather than a `String` because
//! style cannot be recovered from plain text after the fact, and there is
//! exactly one sanctioned way to throw the style away again. A second
//! flattening here — one that trimmed, re-wrapped, or joined runs with a
//! separator — is precisely how the CLI's log and the byte-exactness goldens
//! drift apart, and it would do so silently, because both would still "look
//! like" Stata output.
//!
//! So this module contains no formatting logic at all for `Output` events. It
//! concatenates, and it counts how many times it did.

use std::io::Write;

use stratum_proto::engine::{EngineEvent, OutputStream};
use stratum_proto::styled::to_plain;

use crate::output::json::OutputError;
use crate::output::{FramingGuard, Tally};

/// Renders a run as a classic log.
pub struct TextSink<W: Write> {
    out: W,
    guard: FramingGuard,
    tally: Tally,
    /// ADR-017 counter: calls to [`to_plain`]. Must equal the number of
    /// `Output` events — one flatten per event, never a re-flatten of text this
    /// sink has already written.
    flattens: u64,
    /// Bytes written to the log.
    bytes: u64,
    /// Echo each block's source as `. command`, the way a Stata log does.
    echo: bool,
}

impl<W: Write> TextSink<W> {
    /// A log that echoes commands, as `stata -b` does.
    pub fn new(out: W) -> Self {
        Self {
            out,
            guard: FramingGuard::new(),
            tally: Tally::default(),
            flattens: 0,
            bytes: 0,
            echo: true,
        }
    }

    /// Suppress the `. command` echo — what `exec` wants, because the user just
    /// typed the command.
    #[must_use]
    pub fn without_echo(mut self) -> Self {
        self.echo = false;
        self
    }

    /// Write one event.
    ///
    /// # Errors
    /// A framing violation, or a write error.
    pub fn event(&mut self, ev: &EngineEvent) -> Result<(), OutputError> {
        self.guard.admit(ev)?;
        self.tally.observe(ev);
        match ev {
            EngineEvent::BlockStarted { text, .. } if self.echo => {
                // Stata's log prefix. The text is the block's source, verbatim,
                // as snapshotted at enqueue time.
                for line in text.lines() {
                    self.write(". ")?;
                    self.write(line)?;
                    self.write("\n")?;
                }
            }
            // `Trace` is `set trace on` output and belongs in the log beside the
            // results, exactly as it does in Stata. `Error` too: a classic log
            // contains the error text; the *human summary* is what goes to
            // stderr, and that is `human::summary`'s job, not this sink's.
            EngineEvent::Output { runs, stream, .. } if goes_in_the_log(*stream) => {
                self.flattens += 1;
                let plain = to_plain(runs);
                self.write(&plain)?;
            }
            _ => {}
        }
        Ok(())
    }

    fn write(&mut self, s: &str) -> Result<(), OutputError> {
        self.out.write_all(s.as_bytes())?;
        self.bytes += s.len() as u64;
        Ok(())
    }

    /// End of stream.
    ///
    /// # Errors
    /// A run or a block left open.
    pub fn finish(&mut self) -> Result<(), OutputError> {
        self.out.flush()?;
        self.guard.finish().map_err(Into::into)
    }

    /// What the exit code and the §4.3 summary read.
    #[must_use]
    pub fn tally(&self) -> &Tally {
        &self.tally
    }

    /// The same tally, for the pump's own verdict. See [`crate::output::json::JsonSink::tally_mut`].
    pub fn tally_mut(&mut self) -> &mut Tally {
        &mut self.tally
    }

    /// The framing state, so the pump can see what it has to close.
    #[must_use]
    pub fn guard(&self) -> &FramingGuard {
        &self.guard
    }

    /// ADR-017 counter — see [`TextSink::flattens`].
    #[must_use]
    pub fn flattens(&self) -> u64 {
        self.flattens
    }

    /// Bytes of log written.
    #[must_use]
    pub fn bytes(&self) -> u64 {
        self.bytes
    }
}

/// The `OutputStream` a run of text came from, as the log-file writer needs it.
/// Kept here so the one mapping from stream to destination is written once.
#[must_use]
pub fn goes_in_the_log(stream: OutputStream) -> bool {
    match stream {
        OutputStream::Results | OutputStream::Error | OutputStream::Trace => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::fixture;

    fn render(echo: bool) -> (String, u64, u64) {
        let events = fixture::scenario_a();
        let mut buf: Vec<u8> = Vec::new();
        let (flattens, outputs) = {
            let mut sink = TextSink::new(&mut buf);
            if !echo {
                sink = sink.without_echo();
            }
            for ev in &events {
                sink.event(ev).expect("the capture is well framed");
            }
            sink.finish().expect("every run closed");
            (
                sink.flattens(),
                events
                    .iter()
                    .filter(|e| matches!(e, EngineEvent::Output { .. }))
                    .count() as u64,
            )
        };
        (String::from_utf8(buf).expect("UTF-8"), flattens, outputs)
    }

    /// **The acceptance bullet, as a property of this crate.** The CLI's text
    /// mode renders `Vec<StyledRun>` through `styled::to_plain` and does nothing
    /// else to it: the bytes on stdout are, exactly, the concatenation of
    /// `to_plain` over every `Output` event, in order.
    ///
    /// This is the half that is *ours*. A test that only compared the result to
    /// a golden would pass just as happily if this sink trimmed a line and the
    /// producer happened to have padded it; byte-identity to a golden is a
    /// property of the pair, and A12 is a property of the sink.
    #[test]
    fn to_plain_is_the_only_transformation_applied() {
        let events = fixture::scenario_a();
        let want: String = events
            .iter()
            .filter_map(|e| match e {
                EngineEvent::Output { runs, stream, .. } if goes_in_the_log(*stream) => {
                    Some(to_plain(runs))
                }
                _ => None,
            })
            .collect();
        let (log, _, _) = render(false);
        assert_eq!(log, want, "the log is `to_plain` and nothing else");
        assert!(!want.is_empty());
    }

    /// The other half: those bytes are StataMP 18.5's.
    ///
    /// W05's `.txt` goldens do not exist yet (`crates/stratum-stats` has not
    /// landed), so the oracle here is the stronger one they will be derived
    /// from — `tests/golden/stata18/core_surface.log`, captured from the
    /// licensed StataMP 18.5 and committed. Every non-blank line this sink
    /// writes must appear in that log **verbatim**, byte for byte, including the
    /// column positions.
    ///
    /// **One line does not, and it is W07's fixture rather than this renderer.**
    /// `tests/fixtures/mock/scenario_a.msgpack` transcribes the ANOVA header of
    /// `regress` without its six leading spaces. The exception is named here
    /// rather than tolerated as a fuzzy match, and the assertion is equality
    /// against a one-element list: if the fixture is corrected, or if a second
    /// line ever diverges, this fails.
    #[test]
    fn every_line_is_stata_18_5s_except_the_one_the_fixture_mistranscribed() {
        let (log, _, _) = render(false);
        let golden = std::fs::read_to_string(
            fixture::repo_root().join("tests/golden/stata18/core_surface.log"),
        )
        .expect("the committed StataMP 18.5 log");
        let golden: std::collections::HashSet<&str> = golden.lines().collect();

        let mut checked = 0usize;
        let mut diverged: Vec<&str> = Vec::new();
        for line in log.lines() {
            if line.trim().is_empty() {
                continue;
            }
            checked += 1;
            if !golden.contains(line) {
                diverged.push(line);
            }
        }
        assert!(
            checked >= 19,
            "only {checked} lines were compared; the capture should render the \
             summarize table and the whole regress block"
        );
        assert_eq!(
            diverged,
            vec!["Source |       SS           df       MS      Number of obs   =        74"],
            "escalation: W07's capture drops the six leading spaces of the ANOVA \
             header. StataMP 18.5 writes `      Source |`. If this list is now \
             empty the fixture has been fixed — delete the exception."
        );
        // Spot-check the two lines whose column alignment is the entire point.
        for want in [
            "       price |         74    6165.257    2949.496       3291      15906",
            "      weight |   3.464706    .630749     5.49   0.000     2.206717    4.722695",
        ] {
            assert!(log.contains(want), "missing {want:?}");
        }
    }

    /// ADR-017 counter: one flatten per `Output` event. A sink that re-walked
    /// its own buffer — to re-wrap, to strip, to colourise — would show up here
    /// as a count that is not one-to-one, and would be free to disagree with the
    /// goldens above.
    #[test]
    fn to_plain_is_called_exactly_once_per_output_event() {
        let (_, flattens, outputs) = render(true);
        assert_eq!(flattens, outputs);
        assert!(outputs >= 3, "the capture has three blocks of output");
    }

    #[test]
    fn echo_writes_the_command_the_way_a_stata_log_does() {
        let (with, _, _) = render(true);
        let (without, _, _) = render(false);
        assert!(with.contains(". sysuse auto, clear\n"));
        assert!(!without.contains(". sysuse auto, clear\n"));
        assert!(with.len() > without.len());
    }

    #[test]
    fn every_output_stream_reaches_the_log() {
        // Classic Stata's log contains results, errors and trace alike; the
        // human summary is what goes to stderr.
        for s in [
            OutputStream::Results,
            OutputStream::Error,
            OutputStream::Trace,
        ] {
            assert!(goes_in_the_log(s));
        }
    }
}
