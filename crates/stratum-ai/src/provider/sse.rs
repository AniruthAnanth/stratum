//! 07 §1.1 / §2.4 — Server-Sent Events framing.
//!
//! Hand-rolled rather than `eventsource-stream`, which 07 §1.1 named. The design
//! note there says the crate's reconnect behaviour is explicitly unwanted
//! ("Messages API responses are not resumable"), and reconnect is most of what
//! that crate is. What is left is: accumulate bytes, split on a blank line,
//! read `event:` and `data:` fields. That is this file, it has no transitive
//! tree, and it is the piece that has to special-case OpenAI's `data: [DONE]`
//! terminator anyway — which is not a typed event and must be caught **before**
//! the JSON parser sees it.

/// One decoded SSE frame.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum SseFrame {
    /// A frame carrying data.
    Event {
        /// The `event:` field, when the server sent one. Anthropic always does;
        /// OpenAI-compatible endpoints usually do not.
        name: Option<String>,
        /// The concatenated `data:` lines, joined with `\n` per the spec.
        data: String,
    },
    /// The literal `data: [DONE]` terminator used by OpenAI-compatible
    /// endpoints. Not JSON, and a decoder that hands it to `serde_json` reports
    /// a protocol error at the exact moment the stream succeeded.
    Done,
}

/// Incremental SSE decoder.
///
/// Byte-oriented because `reqwest`'s `bytes_stream()` chunk boundaries fall
/// wherever TCP put them — mid-field, mid-UTF-8-sequence, mid-anything.
#[derive(Debug, Default)]
pub struct SseDecoder {
    buf: Vec<u8>,
    event: Option<String>,
    data: String,
    /// Guards against a hostile or broken server streaming an unbounded single
    /// frame. 8 MiB is far above any legitimate Messages API frame.
    overflow: bool,
}

/// Refuse to buffer more than this for one frame.
const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

impl SseDecoder {
    /// A fresh decoder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether a frame exceeded [`MAX_FRAME_BYTES`]. The caller turns this into
    /// a protocol error rather than growing without bound.
    #[must_use]
    pub const fn overflowed(&self) -> bool {
        self.overflow
    }

    /// Feed a chunk; append every completed frame to `out`.
    pub fn push(&mut self, chunk: &[u8], out: &mut Vec<SseFrame>) {
        if self.overflow {
            return;
        }
        if self.buf.len() + chunk.len() > MAX_FRAME_BYTES {
            self.overflow = true;
            self.buf.clear();
            return;
        }
        self.buf.extend_from_slice(chunk);

        // Lines are terminated by \n; \r\n is tolerated because some proxies
        // rewrite line endings.
        while let Some(nl) = self.buf.iter().position(|b| *b == b'\n') {
            let raw: Vec<u8> = self.buf.drain(..=nl).collect();
            let line = String::from_utf8_lossy(&raw);
            let line = line.trim_end_matches(['\n', '\r']);
            self.line(line, out);
        }
    }

    /// End of stream: flush a frame the server did not terminate with a blank
    /// line. Real servers do terminate; a proxy that closes early does not, and
    /// dropping the last token of an answer would be a silent truncation.
    pub fn finish(&mut self, out: &mut Vec<SseFrame>) {
        if !self.buf.is_empty() {
            let raw = std::mem::take(&mut self.buf);
            let line = String::from_utf8_lossy(&raw);
            let line = line.trim_end_matches(['\n', '\r']).to_owned();
            self.line(&line, out);
        }
        self.dispatch(out);
    }

    fn line(&mut self, line: &str, out: &mut Vec<SseFrame>) {
        if line.is_empty() {
            self.dispatch(out);
            return;
        }
        // A line starting with ':' is a comment/keep-alive.
        if line.starts_with(':') {
            return;
        }
        let (field, value) = match line.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (line, ""),
        };
        match field {
            "event" => self.event = Some(value.to_owned()),
            "data" => {
                if !self.data.is_empty() {
                    self.data.push('\n');
                }
                self.data.push_str(value);
            }
            // `id` and `retry` exist in the SSE spec and mean nothing to a
            // non-resumable stream. Ignored, not an error.
            _ => {}
        }
    }

    fn dispatch(&mut self, out: &mut Vec<SseFrame>) {
        if self.data.is_empty() && self.event.is_none() {
            return;
        }
        let data = std::mem::take(&mut self.data);
        let name = self.event.take();
        if data.trim() == "[DONE]" {
            out.push(SseFrame::Done);
        } else {
            out.push(SseFrame::Event { name, data });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(chunks: &[&[u8]]) -> Vec<SseFrame> {
        let mut d = SseDecoder::new();
        let mut out = Vec::new();
        for c in chunks {
            d.push(c, &mut out);
        }
        d.finish(&mut out);
        out
    }

    #[test]
    fn a_named_frame_decodes() {
        let frames = decode(&[b"event: message_start\ndata: {\"a\":1}\n\n"]);
        assert_eq!(
            frames,
            vec![SseFrame::Event {
                name: Some("message_start".to_owned()),
                data: "{\"a\":1}".to_owned()
            }]
        );
    }

    #[test]
    fn chunk_boundaries_fall_anywhere_and_the_frame_still_decodes() {
        // The whole reason this decoder is byte-oriented: TCP splits wherever
        // it likes, including inside a field name.
        let frames = decode(&[b"eve", b"nt: text\nda", b"ta: hel", b"lo\n", b"\n"]);
        assert_eq!(
            frames,
            vec![SseFrame::Event {
                name: Some("text".to_owned()),
                data: "hello".to_owned()
            }]
        );
    }

    #[test]
    fn openai_done_is_caught_before_json_parsing() {
        let frames = decode(&[b"data: {\"x\":1}\n\ndata: [DONE]\n\n"]);
        assert_eq!(
            frames,
            vec![
                SseFrame::Event {
                    name: None,
                    data: "{\"x\":1}".to_owned()
                },
                SseFrame::Done
            ]
        );
    }

    #[test]
    fn multiple_data_lines_join_with_newline_per_the_spec() {
        let frames = decode(&[b"data: a\ndata: b\n\n"]);
        assert_eq!(
            frames,
            vec![SseFrame::Event {
                name: None,
                data: "a\nb".to_owned()
            }]
        );
    }

    #[test]
    fn comments_and_crlf_are_tolerated() {
        let frames = decode(&[b": keep-alive\r\nevent: e\r\ndata: d\r\n\r\n"]);
        assert_eq!(
            frames,
            vec![SseFrame::Event {
                name: Some("e".to_owned()),
                data: "d".to_owned()
            }]
        );
    }

    #[test]
    fn an_unterminated_final_frame_is_flushed_rather_than_dropped() {
        // A proxy that closes without the trailing blank line must not cost the
        // user the last token of the answer.
        let frames = decode(&[b"data: tail"]);
        assert_eq!(
            frames,
            vec![SseFrame::Event {
                name: None,
                data: "tail".to_owned()
            }]
        );
    }

    #[test]
    fn an_unbounded_frame_trips_the_overflow_guard_instead_of_growing() {
        let mut d = SseDecoder::new();
        let mut out = Vec::new();
        let blob = vec![b'x'; 1024 * 1024];
        for _ in 0..9 {
            d.push(&blob, &mut out);
        }
        assert!(d.overflowed());
        assert!(out.is_empty());
    }
}
