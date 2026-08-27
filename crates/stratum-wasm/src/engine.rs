//! W11b — the real segmentation backend behind [`crate::Segmenter`].
//!
//! There is exactly one block-segmentation algorithm in this workspace and it
//! lives in `stratum-parse` (design 02 §5). This module does not contain a
//! second one; it contains the *plumbing* that lets the editor reach that one:
//! an incremental cache, the edit descriptor `stratum_parse::resegment` needs,
//! and the projection onto the flat rows CONTRACTS §14 puts on the wire.
//!
//! # Why there is a cache at all
//!
//! [`crate::Segmenter::resegment`] is handed the whole document and nothing
//! else. `stratum_parse::resegment` is handed the *previous* segmentation and a
//! `SourceEdit`, and that is what buys A25's "≤ 8 regions re-hashed" — the
//! difference between re-hashing a handful of regions and blake3-ing every
//! region below the cursor on every keystroke. Recovering the edit means keeping
//! the previous document and the previous segmentation, so this module keeps
//! both.
//!
//! # Two consequences, both escalated in W11b's report
//!
//! 1. **The edit is rediscovered by diffing.** `Engine::splice` knew exactly
//!    which bytes changed and the trait threw it away, so [`derive_edit`] finds
//!    it again with a chunked `memcmp`. That is O(bytes after the edit) with
//!    memcmp constants, against the O(regions) blake3 it saves — a good trade,
//!    but a trade that would not exist if the seam carried the splice.
//! 2. **The cache is self-referential**, because `stratum_parse::Segmentation`
//!    borrows the buffer it was built from and the crate exposes no owned form.
//!    [`cell`] is the whole of the workaround, in one auditable place.

use std::ops::Range;

use stratum_parse::{SegmentOptions, Segmentation as ParseSegmentation, SourceEdit};
use stratum_proto::{CompletionEnv, Diagnostic, Span, Suggestion};

use crate::{complete, regions, tokens, CompletionList, Segmentation, Segmenter};

/// What one [`ParseSegmenter::resegment`] pass actually did.
///
/// ADR-017: the gates in this crate are counters, never wall clocks. A25's gate
/// is [`PassStats::rescanned`] — regions freshly grouped *and freshly hashed* —
/// and `benches/resegment.rs` asserts it while merely *recording* the duration.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub struct PassStats {
    /// The document was segmented from scratch: no cache, or the incremental
    /// path bailed out.
    pub cold: bool,
    /// Regions freshly grouped and freshly hashed. **The A25 counter.**
    pub rescanned: u32,
    /// Regions reused unchanged from before the edit.
    pub reused_prefix: u32,
    /// Regions reused from after the edit, spans shifted, hashes kept.
    pub reused_suffix: u32,
    /// Source bytes the scanner walked.
    pub bytes_scanned: u32,
    /// The scanner re-converged instead of running to end of source.
    pub converged: bool,
    /// Bytes [`derive_edit`] had to compare to rediscover the edit. This is the
    /// cost the seam imposes by not carrying the splice; it is recorded so the
    /// escalation has a number attached to it.
    pub bytes_diffed: u32,
    /// Regions in the resulting segmentation.
    pub regions: u32,
}

/// The one real [`Segmenter`]: `stratum-parse`, incrementally.
#[derive(Default)]
pub struct ParseSegmenter {
    cache: Option<cell::Cache>,
    opts: SegmentOptions,
    /// Instrumentation for the most recent pass. Read by the bench and the
    /// tests; never by anything that produces output, so it cannot make a
    /// segmentation depend on how it was reached.
    last: PassStats,
}

impl ParseSegmenter {
    /// Counters for the most recent [`Segmenter::resegment`].
    #[must_use]
    pub fn last_pass(&self) -> PassStats {
        self.last
    }

    /// Bring the cache up to date with `doc`, incrementally when it can be.
    ///
    /// Returns without touching the cache when the document has not changed —
    /// which is the common case for `tokens`/`complete`, both of which are
    /// entitled to be called between transactions.
    fn sync(&mut self, doc: &str) {
        let Some(cached) = self.cache.take() else {
            self.cache = Some(cell::Cache::cold(doc, &self.opts));
            self.last = PassStats {
                cold: true,
                regions: self.region_count_of_cache(),
                ..PassStats::default()
            };
            // `region_count_of_cache` needs the cache installed; the line above
            // ran before it was, so fill it in now.
            self.last.regions = self.region_count_of_cache();
            return;
        };

        let diff = derive_edit(cached.src(), doc);
        let Some((edit, bytes_diffed)) = diff else {
            self.cache = Some(cached);
            return;
        };

        let (next, stats) = cached.step(doc, edit);
        self.cache = Some(next);
        self.last = PassStats {
            // `resegment_with_stats` returns a default `ResegmentStats` when it
            // gives up and redoes the file, and a give-up is exactly a cold
            // pass. `reused_prefix == 0` is that signal: a real incremental
            // pass always reuses at least the region before the edit.
            cold: stats.reused_prefix == 0,
            rescanned: stats.rescanned,
            reused_prefix: stats.reused_prefix,
            reused_suffix: stats.reused_suffix,
            bytes_scanned: stats.bytes_scanned,
            converged: stats.converged,
            bytes_diffed,
            regions: self.region_count_of_cache(),
        };
    }

    fn region_count_of_cache(&self) -> u32 {
        self.cache
            .as_ref()
            .map_or(0, |c| c.get().regions.len() as u32)
    }

    /// The cached segmentation, whatever document it was built from.
    ///
    /// The callers that take it are `&self` and cannot sync; each one checks it
    /// against the document it was handed before trusting it.
    fn cached(&self) -> Option<&ParseSegmentation<'_>> {
        self.cache.as_ref().map(cell::Cache::get)
    }

    /// The current segmentation, after syncing to `doc`.
    fn synced(&mut self, doc: &str) -> &ParseSegmentation<'_> {
        self.sync(doc);
        self.cache
            .as_ref()
            .expect("sync always installs a cache")
            .get()
    }
}

impl Segmenter for ParseSegmenter {
    fn resegment(&mut self, doc: &str, out: &mut Segmentation) {
        regions::project(self.synced(doc), out);
    }

    fn tokens(&mut self, doc: &str, range: Range<usize>, out: &mut Vec<i32>) {
        tokens::project(self.synced(doc), range, out);
    }

    fn complete(&self, doc: &str, env: &CompletionEnv, pos: usize) -> CompletionList {
        // `&self`, so the cache cannot be synced here. The one thing completion
        // reads out of it — the logical line under the cursor — is recovered
        // from `doc` directly for exactly that reason, and the result is
        // therefore correct even between transactions.
        complete::complete(doc, env, pos)
    }

    fn quick_fixes(&self, doc: &str, pos: usize) -> Vec<Suggestion> {
        complete::quick_fixes(self.cached(), doc, pos)
    }

    fn lints(&self, doc: &str) -> Vec<Diagnostic> {
        complete::lints(self.cached(), doc)
    }
}

/// Recover the edit between two documents as one replaced range.
///
/// Returns `None` when they are identical, and `Some((edit, compared))` with the
/// number of bytes actually looked at — the counter behind the seam escalation.
///
/// One window, not a diff algorithm: every CodeMirror transaction that reaches
/// `Engine::splice` is a set of replacements, and the smallest range covering
/// them all is always a legal `SourceEdit`. A multi-cursor edit therefore costs
/// a wider rescan than it needed to, and a single-caret edit — the keystroke
/// path the budget is written for — costs exactly the right one.
///
/// The scan is chunked so the comparison runs as `memcmp` rather than as a
/// per-byte loop. On the 2 MB corpus the byte-at-a-time version measured over a
/// millisecond for the suffix alone, which is the whole budget.
fn derive_edit(old: &str, new: &str) -> Option<(SourceEdit, u32)> {
    /// Wide enough that the tail loop is short, small enough that a one-byte
    /// edit does not overshoot into a second cache line of comparison.
    const CHUNK: usize = 64;

    let ob = old.as_bytes();
    let nb = new.as_bytes();
    let max = ob.len().min(nb.len());

    let mut p = 0;
    while p + CHUNK <= max && ob[p..p + CHUNK] == nb[p..p + CHUNK] {
        p += CHUNK;
    }
    while p < max && ob[p] == nb[p] {
        p += 1;
    }
    if p == max && ob.len() == nb.len() {
        return None;
    }

    // The common suffix may not reach back past the common prefix, or the two
    // would describe overlapping halves of the same bytes and the replaced range
    // would come out inverted.
    let limit = max - p;
    let mut s = 0;
    while s + CHUNK <= limit
        && ob[ob.len() - s - CHUNK..ob.len() - s] == nb[nb.len() - s - CHUNK..nb.len() - s]
    {
        s += CHUNK;
    }
    while s < limit && ob[ob.len() - 1 - s] == nb[nb.len() - 1 - s] {
        s += 1;
    }
    let compared = (p + s) as u32;

    // Snap to character boundaries in BOTH documents. `LineIndex::patch` slices
    // `new_src` at these offsets, and an offset inside a multi-byte character
    // panics there — the same class of bug `Doc::splice` rejects on the way in,
    // arriving by a different door because a two-byte character can make the
    // naive prefix stop one byte short of a boundary.
    let mut start = p;
    while start > 0 && !(old.is_char_boundary(start) && new.is_char_boundary(start)) {
        start -= 1;
    }
    let mut old_end = ob.len() - s;
    let mut new_end = nb.len() - s;
    while !(old.is_char_boundary(old_end) && new.is_char_boundary(new_end)) {
        old_end += 1;
        new_end += 1;
    }

    Some((
        SourceEdit {
            range: Span {
                start: start as u32,
                end: old_end as u32,
            },
            new_len: (new_end - start) as u32,
        },
        compared,
    ))
}

/// A `stratum_parse::Segmentation` stored together with the buffer it borrows.
///
/// # Why this module exists
///
/// `Segmentation<'a>` borrows the source it was segmented from, `resegment`
/// consumes the previous one, and `stratum-parse` exposes no owned form and no
/// way to rebuild one from parts (`hash_counts` is private). A backend that must
/// keep the previous segmentation across calls therefore needs a struct that
/// owns a `String` and a value borrowing it — the shape `self_cell` and
/// `ouroboros` exist for. Neither is in the workspace dependency table and the
/// wasm bundle has a 700 KB budget, so the pattern is written out here instead,
/// once, in fifty lines that can be read in one sitting.
///
/// **The crate's `unsafe_code` lint is `deny`, not `forbid`, and that is the
/// affordance being used** — the same one `stratum-dta`'s reader takes, with the
/// same obligation: an audit note per `unsafe` block. W11b's report asks
/// `stratum-parse` for an owned segmentation so this module can be deleted.
///
/// # The invariant
///
/// `seg.src` is always exactly `buf.as_str()`, and `buf` is never mutated while
/// `seg` holds a borrow of it. Both halves are enforced by this module's API:
/// nothing outside it can reach either field, [`Cache::step`] is the only thing
/// that mutates `buf`, and it does so only after the borrow has been consumed.
mod cell {
    use stratum_parse::{
        resegment_with_stats, segment_with, ResegmentStats, SegmentOptions, Segmentation,
        SourceEdit,
    };

    pub(super) struct Cache {
        /// **Declared first so it is dropped first.** Rust drops fields in
        /// declaration order; `Segmentation` reads nothing through `src` on
        /// drop, but an invariant that holds only because of what a foreign
        /// crate happens not to do is not an invariant.
        seg: Segmentation<'static>,
        /// The buffer `seg.src` points into. A `String`'s bytes live on the
        /// heap, so moving this struct moves the header and not the bytes, and
        /// the borrow survives the move.
        buf: String,
    }

    impl Cache {
        /// Segment `doc` from scratch.
        pub(super) fn cold(doc: &str, opts: &SegmentOptions) -> Cache {
            let buf = doc.to_owned();
            // Detached before `buf` moves into the struct: after `detach` the
            // value holds no borrow at all, so the move is an ordinary move.
            let seg = detach(segment_with(&buf, opts));
            let mut cache = Cache { seg, buf };
            cache.rehome();
            cache
        }

        /// Apply one edit, reusing everything the edit did not disturb.
        pub(super) fn step(self, doc: &str, edit: SourceEdit) -> (Cache, ResegmentStats) {
            let Cache { seg: prev, mut buf } = self;

            // `prev` is typed `Segmentation<'static>` and genuinely borrows
            // `buf`. That is sound to pass here, and `buf` is provably free
            // afterwards, because of the SIGNATURE of the function being called:
            //
            //     resegment_with_stats<'a>(prev: Segmentation<'_>,
            //                              new_src: &'a str,
            //                              edit: SourceEdit) -> Segmentation<'a>
            //
            // `prev`'s lifetime is unconstrained and unrelated to `'a`, so the
            // borrow checker *inside* `stratum-parse` has already proved that
            // nothing reachable from `prev` can appear in the result. The result
            // borrows `doc` and only `doc`.
            let (fresh, stats) = resegment_with_stats(prev, doc, edit);
            let fresh = detach(fresh);

            // Now — and only now, with every borrow of `buf` consumed — the
            // buffer is refilled. `clear` + `push_str` keeps the allocation, so
            // a steady-state keystroke is one `memcpy` and no allocator traffic.
            buf.clear();
            buf.push_str(doc);

            let mut cache = Cache { seg: fresh, buf };
            cache.rehome();
            (cache, stats)
        }

        /// The cached segmentation, re-tied to this borrow of `self`.
        ///
        /// The `'static` in the field is an implementation detail and must not
        /// escape: returning `&Segmentation<'_>` shortens it back to `&self`, so
        /// a caller cannot hold a region past the next edit.
        pub(super) fn get(&self) -> &Segmentation<'_> {
            &self.seg
        }

        /// The document this segmentation was built from.
        pub(super) fn src(&self) -> &str {
            &self.buf
        }

        /// Point `seg.src` back at the buffer this struct owns.
        fn rehome(&mut self) {
            let src: &str = &self.buf;
            // SAFETY: this is the one place the invariant is asserted rather
            // than proved by the compiler. `src` borrows `self.buf`, whose bytes
            // are heap-allocated and are freed only when `self` is dropped —
            // after `self.seg`, which is declared first. `self.buf` is mutated
            // in exactly one place (`step`), and only after the value holding
            // this borrow has been consumed. No `&Segmentation<'static>` escapes
            // this module: `get` reborrows it at `&self`.
            #[allow(unsafe_code, clippy::undocumented_unsafe_blocks)]
            let src: &'static str = unsafe { std::mem::transmute::<&str, &'static str>(src) };
            self.seg.src = src;
        }
    }

    /// Drop a segmentation's borrow, so its lifetime parameter can be widened.
    ///
    /// This is the transmute that is *provable* rather than merely audited: the
    /// only lifetime-carrying field of `Segmentation<'a>` is `src: &'a str` —
    /// every other field is an owned `Vec`, an owned map, or a `Copy` value over
    /// `u32`s — and the assignment below has just replaced it with a genuine
    /// `&'static str`. At the point of the transmute the value contains no
    /// reference with lifetime `'a`, so widening `'a` cannot extend the life of
    /// any borrow. The two types differ only in an erased lifetime and therefore
    /// have identical layout.
    ///
    /// **If `Segmentation` ever grows a second borrowed field, this stops being
    /// true.** Adding one without adding a line here is the bug; the report asks
    /// `stratum-parse` for an owned form so the question stops being asked.
    fn detach(mut seg: Segmentation<'_>) -> Segmentation<'static> {
        seg.src = "";
        // SAFETY: see the doc comment above — the value holds no `'a` reference
        // at this point, and the two types have identical layout.
        #[allow(unsafe_code, clippy::undocumented_unsafe_blocks)]
        unsafe {
            std::mem::transmute::<Segmentation<'_>, Segmentation<'static>>(seg)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::golden_json;

    /// Two segmentations compared as the bytes the editor would receive.
    ///
    /// `crate::Segmentation` is a set of flat vectors with no `PartialEq` —
    /// deliberately, since it is a wire buffer and not a value — so the
    /// comparison that matters is the one the webview makes, which is over the
    /// rows themselves. `golden_json` is that projection, and it is the same
    /// function `tests/parity.rs` pins.
    fn same(a: &Segmentation, b: &Segmentation) -> bool {
        golden_json(a) == golden_json(b)
    }

    fn edit_of(old: &str, new: &str) -> Option<SourceEdit> {
        derive_edit(old, new).map(|(e, _)| e)
    }

    /// Every derived edit must satisfy the consistency rule `resegment` checks,
    /// or it silently falls back to a cold pass and the A25 counter is a lie.
    fn assert_consistent(old: &str, new: &str) {
        let Some(e) = edit_of(old, new) else {
            assert_eq!(old, new, "no edit reported for two different documents");
            return;
        };
        assert!(e.range.start <= e.range.end);
        assert!(e.range.end as usize <= old.len());
        let after = old.len() - (e.range.end - e.range.start) as usize + e.new_len as usize;
        assert_eq!(
            after,
            new.len(),
            "edit does not account for the length change"
        );
        let rebuilt = format!(
            "{}{}{}",
            &old[..e.range.start as usize],
            &new[e.range.start as usize..e.range.start as usize + e.new_len as usize],
            &old[e.range.end as usize..]
        );
        assert_eq!(
            rebuilt, new,
            "applying the derived edit does not give `new`"
        );
    }

    #[test]
    fn an_unchanged_document_derives_no_edit() {
        assert_eq!(edit_of("summarize price\n", "summarize price\n"), None);
        assert_eq!(edit_of("", ""), None);
    }

    #[test]
    fn a_typed_character_is_a_one_byte_insertion() {
        let e = edit_of("summarize pric\n", "summarize price\n").unwrap();
        assert_eq!(e.range, Span { start: 14, end: 14 });
        assert_eq!(e.new_len, 1);
    }

    #[test]
    fn the_derived_edit_rebuilds_the_document() {
        // Bound outside the array: a `String` temporary inside an array
        // expression is dropped at the end of the statement, before the loop.
        let long = "x".repeat(300);
        let longer = "x".repeat(301);
        let cases = [
            ("", "list\n"),
            ("list\n", ""),
            ("list\nlist\n", "list\n"),
            ("regress price mpg\n", "regress price weight\n"),
            ("a\nb\nc\n", "a\nB\nc\n"),
            ("aaaa", "aaaaaaaa"),
            (long.as_str(), longer.as_str()),
        ];
        for (old, new) in cases {
            assert_consistent(old, new);
        }
    }

    /// The failure this snapping exists for: the naive prefix scan stops at the
    /// first differing BYTE, which inside `é` is not a character boundary, and
    /// `LineIndex::patch` slices there.
    #[test]
    fn edits_snap_to_character_boundaries() {
        let old = "label var x \"café\"\n";
        let new = "label var x \"cafè\"\n";
        let e = edit_of(old, new).unwrap();
        assert!(old.is_char_boundary(e.range.start as usize));
        assert!(new.is_char_boundary(e.range.start as usize));
        assert!(old.is_char_boundary(e.range.end as usize));
        assert_consistent(old, new);
    }

    #[test]
    fn a_multi_byte_insertion_snaps_on_the_right_too() {
        assert_consistent("di \"aé\"\n", "di \"aéé\"\n");
        assert_consistent("di \"ée\"\n", "di \"éée\"\n");
        assert_consistent("di \"日本\"\n", "di \"日\"\n");
    }

    /// The cache must be indistinguishable from a cold pass — that is the whole
    /// claim `resegment` makes, and the whole reason the editor and the engine
    /// can be trusted to agree.
    #[test]
    fn incremental_and_cold_agree() {
        let base = "sysuse auto, clear\nsummarize price\nforeach v of varlist mpg weight {\n    display \"`v'\"\n}\nregress price mpg\n";
        let mut typed = String::new();
        let mut inc = ParseSegmenter::default();
        for ch in base.chars() {
            typed.push(ch);
            let mut a = Segmentation::default();
            inc.resegment(&typed, &mut a);

            let mut cold = ParseSegmenter::default();
            let mut b = Segmentation::default();
            cold.resegment(&typed, &mut b);

            assert!(same(&a, &b), "incremental diverged from cold at {typed:?}");
        }
    }

    #[test]
    fn the_cache_survives_a_deletion_back_to_empty() {
        let mut seg = ParseSegmenter::default();
        let mut doc = String::from("list\nsummarize price\nregress price mpg\n");
        while !doc.is_empty() {
            doc.pop();
            let mut a = Segmentation::default();
            seg.resegment(&doc, &mut a);
            let mut b = Segmentation::default();
            ParseSegmenter::default().resegment(&doc, &mut b);
            assert!(same(&a, &b), "diverged at {doc:?}");
        }
    }

    #[test]
    fn a_pass_over_an_unchanged_document_does_no_work() {
        let mut seg = ParseSegmenter::default();
        let doc = "list\nsummarize price\n";
        let mut out = Segmentation::default();
        seg.resegment(doc, &mut out);
        let first = seg.last_pass();
        seg.resegment(doc, &mut out);
        assert_eq!(
            seg.last_pass(),
            first,
            "an unchanged document changed the counters"
        );
    }
}
