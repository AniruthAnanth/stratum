//! Stratum's deterministic intelligence — ARCHITECTURE C16, design 07.
//!
//! Everything in this crate is **synchronous, offline and pure**. It works with
//! no API key, no network and no configuration, because design 07 §0 counted the
//! spec's intelligence surfaces and found seventeen of the twenty-seven need no
//! model at all: `Did you mean 'income'?` is edit distance over the live
//! varlist, completion is a table lookup dispatched on a syntactic role, and all
//! twenty-six reproducibility checks are static analysis. Routing any of them
//! through a network call would make the product slower, more expensive, less
//! private and less reliable.
//!
//! The split is enforced by the crate graph rather than by convention:
//! `cargo xtask layering` asserts that nothing here reaches a network crate, an
//! async runtime, a platform crate, or any of `tokio` / `time` / `memmap2`, and
//! resolves that graph for `wasm32-unknown-unknown` — because this crate runs
//! inside the editor's wasm module, on the main thread, in the CodeMirror
//! transaction cycle.
//!
//! # What is here
//!
//! | Module | Design | Job |
//! |---|---|---|
//! | [`similarity`] | 07 §6.1 | Jaro–Winkler, Damerau–Levenshtein, path fuzz |
//! | [`complete`] | 07 §7.1 | the completion popup: synchronous, role-dispatched |
//! | [`diagnose`] | 07 §6.1 | return-code cards and did-you-mean quick fixes |
//! | [`lints`] | ARCH C14 | the `L001`–`L012` registry, and `L008`–`L012` themselves |
//! | [`repro`] | 07 §10, 03 §10 | `R001`–`R026` and the [`repro::ReproReport`] roll-up |
//! | [`comment_safety`] | 07 §8 | the four-gate auto-comment proof (spec §23) |
//! | [`partition`] | A15 | the section-move proof (spec §3) |
//! | [`narrative`] | 07 §9 | `//|` and `/*md` region detection |
//!
//! # The one hard dependency: [`ProgramIndex`]
//!
//! CONTRACTS §13 is emphatic that `lex` and `split_statements` must be *the
//! exact code the runtime executes with*. A second "close enough"
//! implementation voids the spec §23 auto-comment guarantee entirely, because
//! the proof is "the runtime sees the same token stream" and a private lexer
//! would only prove that *our copy* of the runtime sees the same token stream.
//!
//! **ESCALATION.** CONTRACTS §13 declares `ProgramIndex` as "implemented by
//! `stratum-parse`, consumed by `stratum-intel`". `stratum-parse` ships every
//! ingredient — [`stratum_parse::lex`] carries a doc comment naming itself
//! `ProgramIndex::lex` — but does not declare the trait, and R0 gives its files
//! to W04/W04b, so W20 cannot add it there. The trait is therefore declared
//! here, at the consumer, and [`ParseIndex`] is the single implementation: it
//! *delegates* to `stratum-parse` and reimplements nothing. If the architect
//! prefers the trait in `stratum-parse`, moving it is a one-line re-export
//! change here and no behaviour changes.

#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used)]
#![deny(clippy::expect_used)]
#![warn(missing_docs)]

pub mod comment_safety;
pub mod complete;
pub mod diagnose;
pub mod lints;
pub mod narrative;
pub mod partition;
pub mod repro;
pub mod similarity;

use stratum_parse::{CommandTable, Derived, DerivedText, LogicalLine, Segmentation};
use stratum_proto::block::RegionSummary;
use stratum_proto::token::{CanonToken, Token};
use stratum_proto::Span;

pub use comment_safety::{assert_comment_only, CommentEdit, CommentLine, CommentStyle};
pub use partition::assert_statement_partition_preserved;

/// What the caller knows about the world outside the buffer.
///
/// **Every field is "we don't know" by default, and that is the point.** This
/// crate cannot reach the filesystem, the session or the network — it runs
/// inside the editor's wasm module — so a check that needs the live varlist, the
/// project tree or the ado index is *given* them or reports `Unknown`. Design
/// 03 §10's honesty rule ("a green mark that was inferred from static analysis
/// is the single worst thing this feature could ship") is enforced here, in the
/// type: there is no field a check can quietly synthesise.
#[derive(Clone, Debug, Default)]
pub struct Env {
    /// The live variable layout, in storage order. `None` means no dataset is
    /// loaded *or* the caller could not supply one — the two are deliberately
    /// not distinguished, because both mean "do not claim a variable is
    /// missing".
    pub varnames: Option<Vec<String>>,
    /// Local macro names in scope.
    pub locals: Vec<String>,
    /// Global macro names in scope.
    pub globals: Vec<String>,
    /// `e()` member names, including `e(b)` column names.
    pub e_names: Vec<String>,
    /// `r()` member names.
    pub r_names: Vec<String>,
    /// `s()` member names.
    pub s_names: Vec<String>,
    /// Scalar names.
    pub scalars: Vec<String>,
    /// Matrix names.
    pub matrices: Vec<String>,
    /// Frame names.
    pub frames: Vec<String>,
    /// Value-label names.
    pub value_labels: Vec<String>,
    /// `estimates store` names.
    pub stored_estimates: Vec<String>,
    /// Names the user has already used in this session, **most recent first**.
    /// Design 07 §7.1's first completion tie-break. Empty is fine; it just
    /// means the tie falls through to the next criterion.
    pub recent: Vec<String>,
    /// Project root, when the buffer belongs to a project.
    pub project_root: Option<camino::Utf8PathBuf>,
    /// The current working directory the file would run in.
    pub cwd: Option<camino::Utf8PathBuf>,
    /// Every file the host is willing to tell us about, project-relative.
    /// Supplied by `stratum-workspace` or the CLI; this crate never walks a
    /// directory.
    pub project_files: Vec<camino::Utf8PathBuf>,
    /// Command names resolvable from the ado path. Empty means "unknown", which
    /// downgrades `R025` rather than flagging every ado command.
    pub installed_ado: Vec<String>,
    /// True when `project_files` is a complete listing rather than a sample.
    /// `R003`/`R004` only claim a file is missing when this is set.
    pub file_listing_is_complete: bool,
    /// Variables that *would* exist if the blocks above the cursor had been
    /// run, and the label of the block that creates each one.
    ///
    /// Design 07 §6.1: an r(111) on one of these gets the answer
    /// "not created yet — run block B4 first", which is a genuinely better
    /// reply than any model would give. It is only available because the
    /// execution ledger knows which blocks ran, so the caller supplies it.
    pub pending_vars: Vec<PendingVar>,
}

/// A variable an unexecuted block above the cursor would create.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingVar {
    /// The variable name.
    pub name: String,
    /// How to refer to the block that creates it, e.g. `"B4"`.
    pub block_label: String,
}

impl Env {
    /// Every completion name the environment can offer, for did-you-mean.
    #[must_use]
    pub fn all_names(&self) -> Vec<&str> {
        let mut out: Vec<&str> = Vec::new();
        out.extend(self.varnames.iter().flatten().map(String::as_str));
        out.extend(self.pending_vars.iter().map(|p| p.name.as_str()));
        for list in [
            &self.locals,
            &self.globals,
            &self.scalars,
            &self.matrices,
            &self.e_names,
            &self.r_names,
            &self.s_names,
            &self.value_labels,
            &self.stored_estimates,
        ] {
            out.extend(list.iter().map(String::as_str));
        }
        out
    }
}

/// The parser seam — CONTRACTS §13.
///
/// Every method is a question about a *source buffer*, answered by the same code
/// the runtime runs. Nothing in this crate lexes, splits or splices on its own.
pub trait ProgramIndex: Send + Sync {
    /// Tokenize, with the runtime's tokenizer.
    fn lex(&self, src: &str) -> Vec<Token>;

    /// The executable extent of every statement, in document order.
    ///
    /// A "statement" is one *logical* line: comments removed, `///`
    /// continuations spliced, `;`-delimited bodies folded. Trivia contributes
    /// nothing, so consecutive spans do **not** tile the source.
    fn split_statements(&self, src: &str) -> Vec<Span>;

    /// Spans no comment may be inserted into: block and line comments, string
    /// literals, compound quotes, and `input`/`mata`/`python`/`java` bodies.
    /// Sorted by start and non-overlapping.
    fn verbatim_regions(&self, src: &str) -> Vec<Span>;

    /// Spans of statements that occupy more than one physical line — `///`
    /// chains and `#delimit ;` bodies. Sorted by start and non-overlapping.
    fn continuation_chains(&self, src: &str) -> Vec<Span>;

    /// The logical executable regions (CONTRACTS §2).
    fn regions(&self, src: &str) -> Vec<RegionSummary>;

    /// The command table this build parses with.
    fn command_table(&self) -> &CommandTable;

    /// `(created, used)` variable names for one region.
    fn creates_and_uses(&self, r: &RegionSummary) -> (Vec<String>, Vec<String>);

    /// The canonical token stream behind that region's `CodeHash`.
    fn canonical_tokens(&self, r: &RegionSummary) -> Vec<CanonToken>;
}

/// The one [`ProgramIndex`], over `stratum-parse`.
///
/// It holds the buffer because two of §13's methods take a [`RegionSummary`] and
/// no source: a region summary carries spans, and a span without its buffer
/// answers nothing. Constructing one segments the source once; every method then
/// reads that segmentation instead of rescanning, which is what makes running
/// twelve lints and twenty-six repro checks over a file one pass rather than
/// thirty-eight.
pub struct ParseIndex<'a> {
    src: &'a str,
    seg: Segmentation<'a>,
    table: CommandTable,
}

impl<'a> ParseIndex<'a> {
    /// Segment `src` once.
    #[must_use]
    pub fn new(src: &'a str) -> Self {
        ParseIndex {
            src,
            seg: stratum_parse::segment(src),
            table: stratum_parse::table(),
        }
    }

    /// The buffer this index was built from.
    #[must_use]
    pub fn source(&self) -> &'a str {
        self.src
    }

    /// The cached segmentation.
    #[must_use]
    pub fn segmentation(&self) -> &Segmentation<'a> {
        &self.seg
    }

    /// The comment-stripped, continuation-spliced code of logical line `i`.
    #[must_use]
    pub fn line_code(&self, i: usize) -> &str {
        match (self.seg.lines.get(i), self.seg.derived.get(i)) {
            (Some(line), Some(d)) => line.code(self.seg.src, d.as_deref()),
            _ => "",
        }
    }

    /// Every non-empty logical line, as `(index, code)`.
    pub fn statements(&self) -> impl Iterator<Item = (usize, &LogicalLine, &str)> + '_ {
        self.seg
            .lines
            .iter()
            .enumerate()
            .filter_map(move |(i, line)| {
                let d: Option<&Derived> = self.seg.derived.get(i).and_then(|s| s.as_deref());
                let code = line.code(self.seg.src, d);
                (!code.trim().is_empty()).then_some((i, line, code))
            })
    }

    /// Map an offset inside logical line `i`'s code back to a source offset.
    #[must_use]
    pub fn to_source(&self, i: usize, off: u32) -> u32 {
        match (self.seg.lines.get(i), self.seg.derived.get(i)) {
            (Some(line), Some(d)) => line.to_source(d.as_deref(), off),
            _ => 0,
        }
    }

    /// Every comment extent in the buffer, sorted and disjoint.
    ///
    /// Derived from the scanner's own piece table, so `"http://x"` contributes
    /// nothing and `* a comment ///` correctly swallows the next line.
    #[must_use]
    pub fn comment_spans(&self) -> Vec<Span> {
        let mut out = Vec::new();
        comment_spans(&self.seg, &mut out);
        normalize(out)
    }

    /// Every string-literal and compound-quote extent, sorted and disjoint.
    #[must_use]
    pub fn string_spans(&self) -> Vec<Span> {
        let mut out = Vec::new();
        string_spans(&self.seg, &mut out);
        normalize(out)
    }

    /// Map a span inside logical line `i`'s code back to source coordinates.
    #[must_use]
    pub fn span_to_source(&self, i: usize, s: Span) -> Span {
        Span {
            start: self.to_source(i, s.start),
            end: self.to_source(i, s.end),
        }
    }
}

/// Segment an arbitrary buffer with `stratum-parse` and run `f` over it.
fn with_segmentation<T>(src: &str, f: impl FnOnce(&Segmentation<'_>) -> T) -> T {
    f(&stratum_parse::segment(src))
}

/// The `[start, end)` of every logical line that is a continuation chain.
fn chains_of(seg: &Segmentation<'_>) -> Vec<Span> {
    seg.lines
        .iter()
        .filter(|l| l.last_line > l.first_line && !l.is_trivia)
        .map(|l| l.span)
        .collect()
}

/// Comment spans: the complement, inside each logical line's extent, of the
/// source runs its code came from.
///
/// Derived from the scanner's own piece table rather than by re-scanning for
/// `//` and `/*`. Re-scanning is the second-lexer mistake CONTRACTS §13 exists
/// to forbid, one level down: a private comment scanner that disagreed with the
/// runtime about whether `"http://x"` contains a comment would let Gate 2 pass
/// an insertion into the middle of a string literal.
fn comment_spans(seg: &Segmentation<'_>, out: &mut Vec<Span>) {
    let src = seg.src;
    // A gap between code runs is a comment only if it holds something. The gaps
    // also hold a line's indentation and its terminator, and counting those put
    // the first byte of every indented line — and the whole of every blank line —
    // inside Gate 2's verbatim cover, which then refused a comment above any
    // statement inside a `foreach { }`. Trimming to the non-whitespace extent is
    // still the piece table's answer about where code is; it is not a second
    // comment scanner, which is the thing this function exists to avoid.
    let mut push = |from: u32, to: u32| {
        let Some(text) = src.get(from as usize..to as usize) else {
            return;
        };
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return;
        }
        let lead = (text.len() - text.trim_start().len()) as u32;
        out.push(Span {
            start: from + lead,
            end: from + lead + trimmed.len() as u32,
        });
    };
    for (i, line) in seg.lines.iter().enumerate() {
        let derived: Option<&Derived> = seg.derived.get(i).and_then(|d| d.as_deref());
        let map = line.map(derived);
        // Code runs, in source coordinates, ascending.
        let mut runs: Vec<Span> = Vec::new();
        let len = map.dst_len();
        if len > 0 {
            runs = map.span_to_source(Span { start: 0, end: len }).to_vec();
            runs.sort_by_key(|s| s.start);
        }
        let mut cursor = line.span.start;
        for r in &runs {
            if r.start > cursor {
                push(cursor, r.start);
            }
            cursor = cursor.max(r.end);
        }
        if line.span.end > cursor {
            push(cursor, line.span.end);
        }
    }
}

/// String-literal and compound-quote spans, in source coordinates.
fn string_spans(seg: &Segmentation<'_>, out: &mut Vec<Span>) {
    use stratum_proto::token::TokenKind;
    for (i, line) in seg.lines.iter().enumerate() {
        if line.is_trivia {
            continue;
        }
        let derived: Option<&Derived> = seg.derived.get(i).and_then(|d| d.as_deref());
        let code = line.code(seg.src, derived);
        if code.is_empty() {
            continue;
        }
        for tok in stratum_parse::lex(code) {
            if !matches!(tok.kind, TokenKind::StrLit | TokenKind::CompoundQuote) {
                continue;
            }
            for s in line.span_to_source(derived, tok.span) {
                out.push(s);
            }
        }
    }
}

/// `input`/`mata`/`python`/`java` bodies — regions whose contents are not Stata
/// commands at all and where a `//` line may not even be a comment.
fn verbatim_block_spans(seg: &Segmentation<'_>, out: &mut Vec<Span>) {
    use stratum_parse::RegionShape;
    use stratum_proto::block::EndBlockOpener;
    for r in &seg.regions {
        if let RegionShape::EndBlock { opener } = r.kind {
            if matches!(
                opener,
                EndBlockOpener::Input
                    | EndBlockOpener::Mata
                    | EndBlockOpener::Python
                    | EndBlockOpener::Java
            ) {
                out.push(r.span);
            }
        }
    }
}

/// Merge overlapping/adjacent spans into a sorted, disjoint cover.
fn normalize(mut spans: Vec<Span>) -> Vec<Span> {
    spans.retain(|s| s.end > s.start);
    spans.sort_by_key(|s| (s.start, s.end));
    let mut out: Vec<Span> = Vec::with_capacity(spans.len());
    for s in spans {
        match out.last_mut() {
            Some(prev) if s.start <= prev.end => prev.end = prev.end.max(s.end),
            _ => out.push(s),
        }
    }
    out
}

/// True when `pos` falls inside one of `spans` (half-open).
#[must_use]
pub fn spans_contain(spans: &[Span], pos: u32) -> bool {
    // The vector is sorted and disjoint, so the candidate is the last span
    // starting at or before `pos`.
    match spans.binary_search_by(|s| s.start.cmp(&pos)) {
        Ok(_) => true,
        Err(0) => false,
        Err(i) => spans.get(i - 1).is_some_and(|s| pos < s.end),
    }
}

impl ProgramIndex for ParseIndex<'_> {
    fn lex(&self, src: &str) -> Vec<Token> {
        stratum_parse::lex(src)
    }

    fn split_statements(&self, src: &str) -> Vec<Span> {
        with_segmentation(src, |seg| {
            seg.lines
                .iter()
                .enumerate()
                .filter_map(|(i, l)| {
                    let d: Option<&Derived> = seg.derived.get(i).and_then(|s| s.as_deref());
                    (!l.code(seg.src, d).trim().is_empty()).then_some(l.code_span)
                })
                .collect()
        })
    }

    fn verbatim_regions(&self, src: &str) -> Vec<Span> {
        with_segmentation(src, |seg| {
            let mut out = Vec::new();
            comment_spans(seg, &mut out);
            string_spans(seg, &mut out);
            verbatim_block_spans(seg, &mut out);
            normalize(out)
        })
    }

    fn continuation_chains(&self, src: &str) -> Vec<Span> {
        with_segmentation(src, |seg| normalize(chains_of(seg)))
    }

    fn regions(&self, src: &str) -> Vec<RegionSummary> {
        with_segmentation(src, |seg| seg.summaries())
    }

    fn command_table(&self) -> &CommandTable {
        &self.table
    }

    fn creates_and_uses(&self, r: &RegionSummary) -> (Vec<String>, Vec<String>) {
        let mut created = Vec::new();
        let mut used = Vec::new();
        for (i, line) in self.seg.lines.iter().enumerate() {
            if line.code_span.start < r.span.start || line.code_span.end > r.span.end {
                continue;
            }
            let d: Option<&Derived> = self.seg.derived.get(i).and_then(|s| s.as_deref());
            let code = line.code(self.seg.src, d);
            lints::dataflow::creates_and_uses_line(code, &mut created, &mut used);
        }
        created.sort();
        created.dedup();
        used.sort();
        used.dedup();
        (created, used)
    }

    fn canonical_tokens(&self, r: &RegionSummary) -> Vec<CanonToken> {
        // The region's own logical lines, sliced out of the cached segmentation
        // by source extent — `RegionSummary::index` is explicitly "NOT stable
        // across edits", so it is not used as a key.
        let mut lines: Vec<LogicalLine> = Vec::new();
        let mut derived: Vec<DerivedText> = Vec::new();
        for (i, line) in self.seg.lines.iter().enumerate() {
            if line.span.start >= r.outer_span.start && line.span.end <= r.outer_span.end {
                lines.push(*line);
                derived.push(self.seg.derived.get(i).and_then(|d| d.clone()));
            }
        }
        stratum_parse::canonical_tokens(self.seg.src, &lines, &derived)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;

    const SRC: &str = "\
* a leading comment
sysuse auto, clear
gen ln_price = ln(price)   // trailing
regress ln_price mpg ///
    weight
di \"a // not a comment\"
";

    #[test]
    fn statements_are_logical_lines_not_physical_ones() {
        let idx = ParseIndex::new(SRC);
        let stmts = idx.split_statements(SRC);
        // sysuse, gen, regress (one statement across two physical lines), di.
        assert_eq!(stmts.len(), 4, "{stmts:?}");
    }

    #[test]
    fn the_continuation_chain_is_one_span() {
        let idx = ParseIndex::new(SRC);
        let chains = idx.continuation_chains(SRC);
        assert_eq!(chains.len(), 1, "{chains:?}");
        let c = chains[0];
        let text = SRC.get(c.start as usize..c.end as usize).unwrap_or("");
        assert!(text.contains("///"), "{text:?}");
        assert!(text.contains("weight"), "{text:?}");
    }

    #[test]
    fn a_slash_slash_inside_a_string_is_not_a_comment() {
        let idx = ParseIndex::new(SRC);
        let verbatim = idx.verbatim_regions(SRC);
        let at = SRC.find("a // not a comment").unwrap_or(0) as u32;
        assert!(
            spans_contain(&verbatim, at),
            "the string body must be verbatim: {verbatim:?}"
        );
        // ... and the real trailing comment is verbatim too.
        let tc = SRC.find("// trailing").unwrap_or(0) as u32;
        assert!(spans_contain(&verbatim, tc + 3), "{verbatim:?}");
    }

    #[test]
    fn verbatim_spans_are_sorted_and_disjoint() {
        let idx = ParseIndex::new(SRC);
        for spans in [idx.verbatim_regions(SRC), idx.continuation_chains(SRC)] {
            for w in spans.windows(2) {
                assert!(w[0].end <= w[1].start, "{spans:?}");
            }
        }
    }

    #[test]
    fn spans_contain_is_half_open() {
        let spans = vec![Span { start: 4, end: 8 }, Span { start: 10, end: 12 }];
        assert!(!spans_contain(&spans, 3));
        assert!(spans_contain(&spans, 4));
        assert!(spans_contain(&spans, 7));
        assert!(!spans_contain(&spans, 8));
        assert!(spans_contain(&spans, 10));
        assert!(!spans_contain(&spans, 12));
    }
}
