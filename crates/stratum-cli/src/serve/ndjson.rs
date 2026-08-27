//! CONTRACTS.md §7.1 — the NDJSON envelope, on the engine side.
//!
//! **This is not JSON-RPC** (A9). One object per line: `{v,t,corr,body}`, LF,
//! UTF-8, no BOM, no pretty-printing, no `jsonrpc`, no `id`, no `method`. The
//! Rust variant name inside `body` *is* the method name, so there is no name
//! registry here to drift from `stratum-proto`.
//!
//! `--protocol json` and `stratum run --json` both write through
//! [`NdjsonWriter`]. §7 guarantee 4 — stdout carries the stream and nothing else
//! — is a property of who owns the handle, which is why this type takes the
//! writer rather than reaching for `println!`.

use std::io::{BufWriter, Write};

use serde::de::DeserializeOwned;
use serde::Serialize;
use stratum_proto::frame::{Envelope, FrameError, LineReader, WireTag};

/// Writes §7.1 lines. Flushes per line: a consumer piping into `jq` must see a
/// `RunStarted` before the run it announces has finished.
pub struct NdjsonWriter<W: Write> {
    out: BufWriter<W>,
    lines: u64,
}

impl<W: Write> NdjsonWriter<W> {
    pub fn new(out: W) -> Self {
        Self {
            out: BufWriter::new(out),
            lines: 0,
        }
    }

    /// # Errors
    /// Propagates the serializer's error and any write error on the sink.
    pub fn write<B: Serialize>(&mut self, env: &Envelope<B>) -> std::io::Result<()> {
        // `to_writer` would interleave a partial line with a concurrent write on
        // a shared handle; one buffer, one `write_all`, one line.
        let line = serde_json::to_vec(env)?;
        debug_assert!(
            !line.contains(&b'\n'),
            "serde_json escapes interior newlines; a raw one would split the record"
        );
        self.out.write_all(&line)?;
        self.out.write_all(b"\n")?;
        self.out.flush()?;
        self.lines += 1;
        Ok(())
    }

    pub fn event<B: Serialize>(&mut self, body: B) -> std::io::Result<()> {
        self.write(&Envelope::event(body))
    }

    pub fn response<B: Serialize>(&mut self, corr: u32, body: B) -> std::io::Result<()> {
        self.write(&Envelope::resp(corr, body))
    }

    #[must_use]
    pub fn lines_written(&self) -> u64 {
        self.lines
    }
}

/// One decoded line, or the reason it was skipped.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Line<B> {
    Ok {
        corr: Option<u32>,
        body: B,
    },
    /// §7.1: "A reader that does not recognise `t` or `body`'s tag MUST skip the
    /// line and continue." Reported rather than swallowed so `--protocol json`
    /// can log it once at debug level; it is not an error.
    Skipped {
        reason: SkipReason,
    },
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum SkipReason {
    /// `t` was not `req`/`resp`/`event`, or was not the one expected here.
    UnknownTag,
    /// `body`'s tag, or its shape, is from a schema this build does not have.
    UnknownBody,
    /// `v` is a different STREAM_SCHEMA major.
    ForeignSchema { v: u32 },
}

/// Reads §7.1 lines off any byte source.
pub struct NdjsonReader {
    lines: LineReader,
    expect: WireTag,
}

impl NdjsonReader {
    #[must_use]
    pub fn new(expect: WireTag) -> Self {
        Self {
            lines: LineReader::new(),
            expect,
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        self.lines.feed(bytes);
    }

    /// The next line, or `Ok(None)` when more bytes are needed.
    ///
    /// # Errors
    /// Only framing errors — an unterminated line past the size limit. A line
    /// this build cannot interpret is [`Line::Skipped`], never an error: that
    /// is what makes §15's additive-only rule usable by a third party.
    pub fn next_line<B: DeserializeOwned>(&mut self) -> Result<Option<Line<B>>, FrameError> {
        let Some(line) = self.lines.next_line()? else {
            return Ok(None);
        };
        Ok(Some(match serde_json::from_slice::<Envelope<B>>(&line) {
            Ok(env) if env.v != stratum_proto::engine::STREAM_SCHEMA => Line::Skipped {
                reason: SkipReason::ForeignSchema { v: env.v },
            },
            Ok(env) if env.t != self.expect => Line::Skipped {
                reason: SkipReason::UnknownTag,
            },
            Ok(env) => Line::Ok {
                corr: env.corr,
                body: env.body,
            },
            Err(_) => Line::Skipped {
                reason: SkipReason::UnknownBody,
            },
        }))
    }

    /// # Errors
    /// [`FrameError::TruncatedLine`] if the producer stopped mid-line.
    pub fn end_of_stream(&self) -> Result<(), FrameError> {
        self.lines.end_of_stream()
    }
}

#[cfg(test)]
mod tests {
    use stratum_proto::engine::{EngineEvent, EngineHealth, EngineRequest};
    use stratum_proto::ids::SessionId;

    use super::*;

    #[test]
    fn one_object_per_line_lf_no_bom() {
        let mut buf = Vec::new();
        {
            let mut w = NdjsonWriter::new(&mut buf);
            w.event(EngineEvent::EngineHealth {
                seq: 1,
                health: EngineHealth::Ready,
            })
            .unwrap();
            w.response(7, stratum_proto::engine::EngineResponse::Ok)
                .unwrap();
            assert_eq!(w.lines_written(), 2);
        }
        let text = String::from_utf8(buf).unwrap();
        assert!(!text.starts_with('\u{feff}'), "no BOM");
        assert!(!text.contains('\r'), "LF only");
        assert_eq!(text.lines().count(), 2);
        assert!(text.ends_with('\n'), "every record is terminated");
        assert!(text
            .lines()
            .next()
            .unwrap()
            .starts_with(r#"{"v":1,"t":"event","body":"#));
        assert!(text.lines().nth(1).unwrap().contains(r#""corr":7"#));
        for banned in ["jsonrpc", "\"method\"", "\"params\""] {
            assert!(!text.contains(banned), "{banned} in {text}");
        }
    }

    #[test]
    fn an_unknown_line_is_skipped_and_the_reader_continues() {
        let mut stream = Vec::new();
        {
            let mut w = NdjsonWriter::new(&mut stream);
            w.write(&Envelope::req(
                1,
                EngineRequest::Status {
                    session: SessionId(1),
                },
            ))
            .unwrap();
        }
        stream.extend_from_slice(
            br#"{"v":1,"t":"req","corr":2,"body":{"req":"quantum_regress","session":1}}"#,
        );
        stream.push(b'\n');
        stream.extend_from_slice(
            br#"{"v":2,"t":"req","corr":3,"body":{"req":"status","session":1}}"#,
        );
        stream.push(b'\n');
        {
            let mut w = NdjsonWriter::new(&mut stream);
            w.write(&Envelope::req(4, EngineRequest::Shutdown)).unwrap();
        }

        let mut r = NdjsonReader::new(WireTag::Req);
        r.feed(&stream);
        let mut got = Vec::new();
        while let Some(line) = r.next_line::<EngineRequest>().unwrap() {
            got.push(line);
        }
        r.end_of_stream().unwrap();
        assert_eq!(got.len(), 4);
        assert!(matches!(got[0], Line::Ok { corr: Some(1), .. }));
        assert_eq!(
            got[1],
            Line::Skipped {
                reason: SkipReason::UnknownBody
            }
        );
        assert_eq!(
            got[2],
            Line::Skipped {
                reason: SkipReason::ForeignSchema { v: 2 }
            }
        );
        assert!(matches!(
            got[3],
            Line::Ok {
                corr: Some(4),
                body: EngineRequest::Shutdown
            }
        ));
    }

    #[test]
    fn a_body_with_an_interior_newline_stays_on_one_line() {
        let mut buf = Vec::new();
        {
            let mut w = NdjsonWriter::new(&mut buf);
            w.event(EngineEvent::Progress {
                seq: 1,
                exec: stratum_proto::ids::ExecutionId(1),
                done: 1,
                total: None,
                label: "two\nlines\r\nand a \"quote\"".to_owned(),
            })
            .unwrap();
        }
        assert_eq!(String::from_utf8(buf).unwrap().lines().count(), 1);
    }
}
