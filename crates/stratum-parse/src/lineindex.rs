//! Physical-line index — the byte↔line half of design 02 §5.1.
//!
//! Built once per segmentation pass and consulted for every region's
//! [`LineRange`]. Property 5 of 02 §5.4 ("line/byte agreement") is a statement
//! about this type and [`crate::scan::Region`] agreeing, so the two derive their
//! answers from the same table rather than counting newlines twice.

use stratum_proto::{LineRange, Span};

/// 0-based physical line starts, in ascending order. `starts[0]` is always `0`,
/// so a line number is an index into this vector.
///
/// A source file with a trailing newline ends with an empty last line, exactly
/// as an editor shows it: `"a\n"` is two lines, the second of which is empty.
/// The gutter has to agree with CodeMirror about this or every marker below the
/// last command is off by one.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LineIndex {
    starts: Vec<u32>,
}

impl Default for LineIndex {
    fn default() -> Self {
        Self { starts: vec![0] }
    }
}

impl LineIndex {
    /// O(n) single pass. `src.len()` must fit in a `u32`; the editor's document
    /// model is `u32`-indexed end to end (CONTRACTS §1.1 `Span`), so a source
    /// larger than 4 GiB is out of contract everywhere, not just here.
    pub fn new(src: &str) -> Self {
        debug_assert!(u32::try_from(src.len()).is_ok(), "source exceeds u32 spans");
        let b = src.as_bytes();
        let mut starts = Vec::with_capacity(b.len() / 24 + 1);
        starts.push(0);
        // Eight bytes at a time. About one byte in twenty-five of a do-file is a
        // newline, so the overwhelmingly common eight-byte window contains none,
        // and the SWAR zero-byte test rejects it in four instructions instead of
        // eight loads, eight compares and eight unpredictable branches. Measured
        // 3.0x on the 1 MB corpus, and this runs once per cold pass over the
        // whole buffer.
        const ONES: u64 = 0x0101_0101_0101_0101;
        const NLS: u64 = 0x0a0a_0a0a_0a0a_0a0a;
        let mut i = 0usize;
        while i + 8 <= b.len() {
            let w = u64::from_le_bytes(b[i..i + 8].try_into().expect("eight bytes"));
            let x = w ^ NLS;
            if x.wrapping_sub(ONES) & !x & (ONES << 7) != 0 {
                for (k, byte) in b[i..i + 8].iter().enumerate() {
                    if *byte == b'\n' {
                        starts.push((i + k) as u32 + 1);
                    }
                }
            }
            i += 8;
        }
        for (k, byte) in b[i..].iter().enumerate() {
            if *byte == b'\n' {
                starts.push((i + k) as u32 + 1);
            }
        }
        Self { starts }
    }

    /// Number of physical lines. Never zero: an empty document has one empty
    /// line.
    #[inline]
    pub fn line_count(&self) -> u32 {
        self.starts.len() as u32
    }

    /// 0-based line containing `byte`. A byte one past the end of the source
    /// belongs to the last line.
    #[inline]
    pub fn line_of(&self, byte: u32) -> u32 {
        match self.starts.binary_search(&byte) {
            Ok(line) => line as u32,
            // `Err(0)` is impossible: `starts[0] == 0` and `byte` is unsigned.
            Err(next) => next as u32 - 1,
        }
    }

    /// Byte offset of the first byte of `line`, clamped to the last line.
    #[inline]
    pub fn line_start(&self, line: u32) -> u32 {
        let last = self.starts.len() - 1;
        self.starts[(line as usize).min(last)]
    }

    /// 0-based column of `byte`, counted in BYTES from the line start. Callers
    /// that need a display column must widen it themselves; this type does not
    /// know about grapheme clusters and must not pretend to.
    #[inline]
    pub fn col_of(&self, byte: u32) -> u32 {
        byte - self.line_start(self.line_of(byte))
    }

    /// Half-open line range covering `span`.
    ///
    /// An empty span is an empty range on its own line, which is what makes
    /// `line_of(span.start) == lines.start` hold for a `Trivia` region with no
    /// executable extent as well as for a command.
    pub fn lines_of(&self, span: Span) -> LineRange {
        let start = self.line_of(span.start);
        if span.end <= span.start {
            return LineRange { start, end: start };
        }
        // `span.end` is exclusive, so the last byte IN the span decides the last
        // line. Without the `- 1` a region ending exactly at a newline claims the
        // whole line below it and the gutter paints one row too many.
        LineRange {
            start,
            end: self.line_of(span.end - 1) + 1,
        }
    }
}

impl LineIndex {
    /// The index for a source in which `range` was replaced by `new_len` bytes,
    /// patched IN PLACE.
    ///
    /// Rebuilding from scratch is a full pass over the buffer, which on the
    /// keystroke path is the single largest cost in incremental re-segmentation —
    /// larger than the rescan it exists to avoid. Line starts at or before the
    /// edit are unchanged, line starts inside it are gone, and the rest simply
    /// move, so the work is proportional to the edit plus one shift of the tail.
    ///
    /// **Consuming, and one pass.** The by-reference spelling this replaced had
    /// to allocate a second table the size of the first — 285 KB on a 2 MB
    /// document, per keystroke, through the allocator's large path and its page
    /// faults — and copy the untouched prefix into it for nothing.
    /// [`crate::resegment`] owns the previous segmentation, so the table it is
    /// replacing is free to be the table it returns: the tail moves and shifts
    /// in the same pass, and the only allocation left is the rare one where the
    /// edit added more line starts than the vector has spare capacity.
    pub fn patch(mut self, new_src: &str, range: Span, new_len: u32) -> LineIndex {
        // Two's complement: a negative delta added as a wrapped `u32` is the
        // subtraction, and no line start can be moved below zero by a legal
        // edit. The i64 round trip this replaced was two extra instructions on
        // every element of the tail.
        let delta = (i64::from(new_len) - i64::from(range.end - range.start)) as u32;
        let keep = self.starts.partition_point(|&p| p <= range.start);
        let drop_to = self.starts.partition_point(|&p| p <= range.end);
        let b = new_src.as_bytes();
        let to = (range.start + new_len) as usize;
        let inserted = b[range.start as usize..to]
            .iter()
            .filter(|c| **c == b'\n')
            .count();

        let old_len = self.starts.len();
        let new_total = keep + inserted + (old_len - drop_to);
        // `copy_within` then a slice walk, NOT one indexed loop that does both.
        // The indexed loop is a scalar read-modify-write with two bounds checks
        // per element and LLVM will not vectorise it; these two are a `memmove`
        // and a loop over a slice of known length, and together they measured
        // 40 µs faster on the 2 MB tail than the single loop did.
        let tail = if new_total > old_len {
            let k = new_total - old_len;
            self.starts.resize(new_total, 0);
            self.starts.copy_within(drop_to..old_len, drop_to + k);
            drop_to + k
        } else {
            let k = old_len - new_total;
            self.starts.copy_within(drop_to..old_len, drop_to - k);
            self.starts.truncate(new_total);
            drop_to - k
        };
        for p in &mut self.starts[tail..] {
            *p = p.wrapping_add(delta);
        }

        let mut w = keep;
        for (off, byte) in b[range.start as usize..to].iter().enumerate() {
            if *byte == b'\n' {
                self.starts[w] = range.start + off as u32 + 1;
                w += 1;
            }
        }
        self
    }
}
