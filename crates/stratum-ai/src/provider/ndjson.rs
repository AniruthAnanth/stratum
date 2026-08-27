//! 07 §2.5 — Ollama's response framing.
//!
//! Ollama's native `/api/chat` is **NDJSON, not SSE**: one JSON object per line,
//! no `data:` prefix, no blank-line separator, and the terminator is a field
//! (`"done": true`) rather than a frame. A decoder that assumed SSE would find
//! no `data:` field and emit nothing, forever, which is exactly the failure mode
//! that makes "just reuse the SSE decoder" tempting and wrong.

/// Incremental NDJSON decoder.
#[derive(Debug, Default)]
pub struct NdjsonDecoder {
    buf: Vec<u8>,
    overflow: bool,
}

/// Refuse to buffer more than this for one line.
const MAX_LINE_BYTES: usize = 8 * 1024 * 1024;

impl NdjsonDecoder {
    /// A fresh decoder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether a line exceeded [`MAX_LINE_BYTES`].
    #[must_use]
    pub const fn overflowed(&self) -> bool {
        self.overflow
    }

    /// Feed a chunk; append every complete line (blank lines dropped) to `out`.
    pub fn push(&mut self, chunk: &[u8], out: &mut Vec<String>) {
        if self.overflow {
            return;
        }
        if self.buf.len() + chunk.len() > MAX_LINE_BYTES {
            self.overflow = true;
            self.buf.clear();
            return;
        }
        self.buf.extend_from_slice(chunk);
        while let Some(nl) = self.buf.iter().position(|b| *b == b'\n') {
            let raw: Vec<u8> = self.buf.drain(..=nl).collect();
            let line = String::from_utf8_lossy(&raw);
            let line = line.trim();
            if !line.is_empty() {
                out.push(line.to_owned());
            }
        }
    }

    /// End of stream: flush a final line with no trailing newline.
    pub fn finish(&mut self, out: &mut Vec<String>) {
        if self.buf.is_empty() {
            return;
        }
        let raw = std::mem::take(&mut self.buf);
        let line = String::from_utf8_lossy(&raw);
        let line = line.trim();
        if !line.is_empty() {
            out.push(line.to_owned());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_split_on_newline_regardless_of_chunking() {
        let mut d = NdjsonDecoder::new();
        let mut out = Vec::new();
        d.push(b"{\"a\":1}\n{\"b\"", &mut out);
        d.push(b":2}\n", &mut out);
        assert_eq!(out, vec!["{\"a\":1}".to_owned(), "{\"b\":2}".to_owned()]);
    }

    #[test]
    fn a_final_line_without_a_newline_is_flushed() {
        let mut d = NdjsonDecoder::new();
        let mut out = Vec::new();
        d.push(b"{\"done\":true}", &mut out);
        assert!(out.is_empty());
        d.finish(&mut out);
        assert_eq!(out, vec!["{\"done\":true}".to_owned()]);
    }

    #[test]
    fn blank_lines_are_dropped_not_emitted_as_empty_json() {
        let mut d = NdjsonDecoder::new();
        let mut out = Vec::new();
        d.push(b"\n\n{\"a\":1}\n\n", &mut out);
        assert_eq!(out, vec!["{\"a\":1}".to_owned()]);
    }
}
