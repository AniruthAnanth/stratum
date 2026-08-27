//! `SpanMap` — design 02 §3, the piece table that takes a byte offset in derived
//! text back to a byte offset in the original source.
//!
//! It is a piece table and not a per-byte map because the overwhelmingly common
//! logical line has exactly one piece (no comment was stripped, no continuation
//! spliced), so the map costs one 12-byte inline entry and answers in a compare.
//!
//! [`SpanMap::compose`] is what lets a runtime error inside a macro-expanded
//! expression be underlined at the right byte of the original `.do` file
//! (spec §21). The chain is
//! `source ──LogicalLine::map──▶ code ──Expansion::map──▶ expanded`, and each
//! link maps DERIVED offsets to the offsets it was derived from.

use smallvec::SmallVec;
use stratum_proto::Span;

/// One contiguous run: `dst..dst+len` in the derived text came from
/// `src..src+len` in the text it was derived from.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
struct Piece {
    dst: u32,
    src: u32,
    len: u32,
}

/// Sorted by `dst`, with no overlaps and no zero-length pieces.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct SpanMap {
    pieces: SmallVec<[Piece; 2]>,
}

impl SpanMap {
    /// The empty map. Every offset maps to `0`.
    pub fn new() -> Self {
        Self::default()
    }

    /// `0..len` in the derived text is `src_start..src_start+len` in the source.
    pub fn identity(src_start: u32, len: u32) -> Self {
        let mut m = Self::new();
        m.push(0, src_start, len);
        m
    }

    /// Append a run. Runs must be appended in ascending `dst` order; a run that
    /// continues the previous one in BOTH coordinates is coalesced, which is why
    /// a line with no comments ends up with exactly one piece.
    pub fn push(&mut self, dst: u32, src: u32, len: u32) {
        if len == 0 {
            return;
        }
        if let Some(last) = self.pieces.last_mut() {
            debug_assert!(dst >= last.dst + last.len, "SpanMap pieces must ascend");
            if last.dst + last.len == dst && last.src + last.len == src {
                last.len += len;
                return;
            }
        }
        self.pieces.push(Piece { dst, src, len });
    }

    /// Total length of the derived text this map covers.
    pub fn dst_len(&self) -> u32 {
        self.pieces.last().map_or(0, |p| p.dst + p.len)
    }

    /// True when no run has been pushed.
    pub fn is_empty(&self) -> bool {
        self.pieces.is_empty()
    }

    /// Derived offset → source offset. Binary search.
    ///
    /// An offset that falls in a GAP (text that was inserted, not copied) maps
    /// to the end of the preceding run: the error underline lands on the last
    /// real source byte rather than on nothing. An offset before the first run
    /// maps to that run's source start.
    pub fn to_source(&self, dst: u32) -> u32 {
        let Some(first) = self.pieces.first() else {
            return 0;
        };
        if dst < first.dst {
            return first.src;
        }
        let i = self.pieces.partition_point(|p| p.dst <= dst) - 1;
        let p = self.pieces[i];
        let off = dst - p.dst;
        p.src + off.min(p.len)
    }

    /// Derived span → the source spans it came from, in ascending order.
    ///
    /// A span that crosses a stripped comment genuinely IS two source ranges;
    /// returning one span from the first byte to the last would underline the
    /// comment as if it were part of the expression.
    pub fn span_to_source(&self, s: Span) -> SmallVec<[Span; 2]> {
        let mut out: SmallVec<[Span; 2]> = SmallVec::new();
        if s.end <= s.start {
            let at = self.to_source(s.start);
            out.push(Span { start: at, end: at });
            return out;
        }
        for p in &self.pieces {
            let lo = s.start.max(p.dst);
            let hi = s.end.min(p.dst + p.len);
            if lo >= hi {
                continue;
            }
            let piece = Span {
                start: p.src + (lo - p.dst),
                end: p.src + (hi - p.dst),
            };
            match out.last_mut() {
                Some(prev) if prev.end == piece.start => prev.end = piece.end,
                _ => out.push(piece),
            }
        }
        if out.is_empty() {
            let at = self.to_source(s.start);
            out.push(Span { start: at, end: at });
        }
        out
    }

    /// `self` maps A→B and `next` maps B→C; the result maps A→C.
    ///
    /// A single piece of `self` may straddle several pieces of `next`, so it is
    /// split at every boundary. `push` coalesces the parts back together when
    /// they turn out to be contiguous in C as well.
    pub fn compose(&self, next: &SpanMap) -> SpanMap {
        let mut out = SpanMap::new();
        for p in &self.pieces {
            let mut done = 0u32;
            while done < p.len {
                let b = p.src + done;
                let mapped = next.to_source(b);
                // How far can we go before `next` changes piece?
                let run = next
                    .pieces
                    .iter()
                    .find(|q| b >= q.dst && b < q.dst + q.len)
                    .map_or(p.len - done, |q| (q.dst + q.len - b).min(p.len - done));
                out.push(p.dst + done, mapped, run);
                done += run;
            }
        }
        out
    }
}

impl SpanMap {
    /// The same map with every SOURCE offset moved by `delta`.
    ///
    /// Used by incremental re-segmentation: the logical lines after the
    /// convergence point are byte-identical, they have simply moved, so their
    /// maps move with them rather than being rebuilt.
    pub fn shifted(&self, delta: i64) -> SpanMap {
        let mut out = self.clone();
        out.shift(delta);
        out
    }

    /// [`SpanMap::shifted`] without the copy.
    ///
    /// The keystroke path moves every line after the edit, and at ~35 000 lines
    /// per megabyte the difference between rewriting a map in place and cloning
    /// one is the difference between staying inside the A25 budget and not.
    pub fn shift(&mut self, delta: i64) {
        for p in &mut self.pieces {
            p.src = (i64::from(p.src) + delta) as u32;
        }
    }
}
