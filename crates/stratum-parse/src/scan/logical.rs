//! The logical-line reader — design 02 §§2.1–2.3 and §3.1.
//!
//! A *logical line* is the unit Stata reads, macro-expands and executes. This is
//! the only place in the workspace that decides where one ends, and every trap
//! in 02 §2.1 that was verified against StataMP 18.5 is handled here rather than
//! in the segmenter:
//!
//! * `/* … */` NESTS — a depth counter, not a flag. `di "x" /* a /* b */ "tail"`
//!   silently swallows the rest of the file, and a flag would have re-opened the
//!   line at the first `*/`.
//! * Three or more slashes is a CONTINUATION, not a comment, and it splices with
//!   **no** inserted separator: `local t 1 ///⏎   2` is `1` + one space + three
//!   spaces + `2`.
//! * `* comment ///` continues the COMMENT onto the next line, swallowing it.
//! * `//` works at column 0 with nothing before it.
//! * Comments inside strings are not comments (`di "a // b"` prints `a // b`).
//! * An unterminated `"` closes at end of physical line, without error.
//! * In `;` mode a newline is ordinary whitespace, `//` still runs to the end of
//!   the PHYSICAL line, and `*` comments run to the `;`.
//! * `#` directives are always `cr`-terminated in either mode.

use smallvec::SmallVec;
use stratum_proto::{Delimiter, DirectiveKind, Span, Unterminated};

use crate::spanmap::SpanMap;

#[inline]
fn count_nl(b: &[u8]) -> u32 {
    b.iter().filter(|c| **c == b'\n').count() as u32
}

/// Bytes the scanner has to look at once a command has started. Everything else
/// is emitted verbatim, so the hot loop skips it in a tight `while` instead of
/// running the fifteen-branch decision chain per byte. Measured: this is a third
/// of the whole scanning pass on a real do-file, because the overwhelming
/// majority of bytes in Stata source are letters, digits and spaces.
///
/// `*` is deliberately absent: it only matters at command start, which is
/// handled before the fast loop is entered.
static INTERESTING: [bool; 256] = {
    let mut t = [false; 256];
    t[b'\n' as usize] = true;
    t[b';' as usize] = true;
    t[b'"' as usize] = true;
    t[b'`' as usize] = true;
    t[b'/' as usize] = true;
    t[b'{' as usize] = true;
    t[b'}' as usize] = true;
    t
};

/// Code that is NOT one contiguous run of the source: the line spliced a `///`
/// continuation or stepped over an interior comment, so its text had to be
/// joined and carries a piece table back to the source.
///
/// **`map` is relative to the line's `span.start`, not to the source.** That is
/// what makes it survive an edit above it for nothing: a logical line that moved
/// but did not change has the same map, so the keystroke path never touches one.
/// [`LogicalLine::source_of`] is the only correct way to read it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Derived {
    text: Box<str>,
    map: SpanMap,
}

impl Derived {
    /// The piece table, in coordinates relative to the line's `span.start`.
    pub fn map(&self) -> &SpanMap {
        &self.map
    }
}

/// One slot of the table that runs parallel to [`crate::Segmentation::lines`]:
/// entry `i` is line `i`'s spliced text, and `None` — about nine lines in ten of
/// real Stata — means the line's code is exactly `src[code_span]`.
///
/// It is a SECOND VECTOR rather than a field of [`LogicalLine`] for one measured
/// reason: an owned `Box` in the line makes `LogicalLine` neither `Copy` nor
/// free of drop glue, and the keystroke path then has to move 71 000 lines out
/// of their slots one at a time instead of handing the run to `memmove`.
/// Measured on the 2 MB corpus, that one difference is 159 µs against 92 µs —
/// and this table, which needs no rebasing at all because the map is relative,
/// moves with one `Vec::splice`.
pub type DerivedText = Option<Box<Derived>>;

/// One logical line: the extent Stata reads, with comments removed and `///`
/// continuations spliced.
///
/// **The line does not borrow the source.** Its code is `src[code_span]`
/// whenever the line contributed exactly one contiguous run of source bytes,
/// which is the overwhelmingly common case, and is reached through
/// [`LogicalLine::code`] with the buffer the enclosing [`crate::Segmentation`]
/// was built from. That is a deliberate reversal of the obvious `Cow<'a, str>`:
/// a borrowed line has to be re-pointed at the new buffer after every edit, and
/// at ~35 000 lines per megabyte re-pointing the whole document was 40 % of the
/// entire keystroke path — for lines whose bytes did not change.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct LogicalLine {
    /// Full extent in the original source, INCLUDING comments, continuations and
    /// the terminator that ended the line. Consecutive `span`s tile the source.
    pub span: Span,
    /// First code byte .. last code byte. Empty (`start == end`) for trivia.
    /// This is what a region's executable `span` is built from.
    pub code_span: Span,
    /// 0-based physical line of `span.start`.
    pub first_line: u32,
    /// 0-based physical line of the last byte of `span`.
    pub last_line: u32,
    /// 0-based physical line of `code_span.start`. Equal to `first_line` unless
    /// a comment or a blank prologue pushed the first code byte onto a later
    /// physical line.
    pub code_first_line: u32,
    /// 0-based physical line of the last byte of `code_span`.
    pub code_last_line: u32,
    /// Delimiter mode in force when the line started.
    pub entry_delimiter: Delimiter,
    /// Delimiter mode in force after the line — differs only for `#delimit`.
    pub exit_delimiter: Delimiter,
    /// Net brace delta, quote- and comment-aware.
    pub brace_delta: i32,
    /// Minimum running brace depth reached inside the line. `} else {` is
    /// delta 0, min -1, which is how the grouping rule tells it from a line that
    /// neither opens nor closes.
    pub brace_min: i32,
    /// `code` is empty after stripping.
    pub is_trivia: bool,
    /// Trivia with no comment text at all — a blank or whitespace-only line.
    /// A blank line breaks the "comments directly above a command" attachment.
    pub is_blank: bool,
    /// `// %% Title` or `* %% Title` (spec §3). Always begins a new group.
    pub is_cell_marker: bool,
    /// Set on the last line of a source that ended inside a construct that can
    /// never be closed.
    pub open_at_end: Option<Unterminated>,
    /// `Some` when `code` starts with `#`.
    pub directive: Option<DirectiveKind>,
}

impl LogicalLine {
    /// Comment-stripped, continuation-spliced code.
    ///
    /// `src` must be the buffer this line was scanned from — the one the
    /// enclosing [`crate::Segmentation`] carries in `src`. Handing it a
    /// different buffer is the same class of mistake as handing `segment` a
    /// fragment without its entry delimiter, and the borrow on `Segmentation`
    /// is what keeps the pair together.
    #[inline]
    pub fn code<'s>(&'s self, src: &'s str, derived: Option<&'s Derived>) -> &'s str {
        match derived {
            Some(d) => &d.text,
            None => &src[self.code_span.start as usize..self.code_span.end as usize],
        }
    }

    /// Maps every byte offset in [`LogicalLine::code`] back to an offset in the
    /// original source.
    ///
    /// A line with one contiguous run needs no stored table: the map IS its
    /// `code_span`, so it is built here instead of occupying 32 bytes in every
    /// line of the document.
    #[inline]
    pub fn map(&self, derived: Option<&Derived>) -> SpanMap {
        match derived {
            // Stored relative to the line (see [`Derived`]); a caller asking for
            // the map wants source coordinates.
            Some(d) => d.map.shifted(i64::from(self.span.start)),
            None => SpanMap::identity(
                self.code_span.start,
                self.code_span.end - self.code_span.start,
            ),
        }
    }

    /// `code` span → the source spans it came from. See
    /// [`SpanMap::span_to_source`].
    pub fn span_to_source(&self, derived: Option<&Derived>, s: Span) -> SmallVec<[Span; 2]> {
        self.map(derived).span_to_source(s)
    }

    /// `code` with leading and trailing ASCII whitespace removed. Interior
    /// whitespace is left alone — `///` splicing is observable in macro values.
    #[inline]
    pub fn trimmed<'s>(&'s self, src: &'s str, derived: Option<&'s Derived>) -> &'s str {
        self.code(src, derived)
            .trim_matches(|c: char| c.is_ascii_whitespace())
    }

    /// A byte offset in this line's code, as a byte offset in the source. The
    /// piece table is stored relative to `span.start` (see [`Derived`]), so this
    /// is where the base is put back.
    pub fn to_source(self, derived: Option<&Derived>, off: u32) -> u32 {
        match derived {
            Some(d) => self.span.start + d.map.to_source(off),
            None => self.code_span.start + off,
        }
    }

    /// A line that stands in for a slot being moved through — see
    /// `splice_rebase`. Never observed.
    fn vacant() -> Self {
        LogicalLine {
            span: Span { start: 0, end: 0 },
            code_span: Span { start: 0, end: 0 },
            first_line: 0,
            last_line: 0,
            code_first_line: 0,
            code_last_line: 0,
            entry_delimiter: Delimiter::Cr,
            exit_delimiter: Delimiter::Cr,
            brace_delta: 0,
            brace_min: 0,
            is_trivia: true,
            is_blank: true,
            is_cell_marker: false,
            open_at_end: None,
            directive: None,
        }
    }

    /// Move this line by `delta` source bytes and `line_delta` physical lines.
    ///
    /// The keystroke path moves every line after the edit. Because the line does
    /// not borrow the source and its piece table is relative, this is the WHOLE
    /// of that work: six `u32`s of a `Copy` 48-byte struct.
    ///
    /// Two's complement: a wrapped `u32` add is the subtraction for a negative
    /// delta, and the i64 round trip it replaces was two more instructions on
    /// every one of the 71 000 lines a keystroke moves.
    pub(crate) fn shift(&mut self, delta: u32, line_delta: u32) {
        self.span.start = self.span.start.wrapping_add(delta);
        self.span.end = self.span.end.wrapping_add(delta);
        self.code_span.start = self.code_span.start.wrapping_add(delta);
        self.code_span.end = self.code_span.end.wrapping_add(delta);
        self.first_line = self.first_line.wrapping_add(line_delta);
        self.last_line = self.last_line.wrapping_add(line_delta);
        self.code_first_line = self.code_first_line.wrapping_add(line_delta);
        self.code_last_line = self.code_last_line.wrapping_add(line_delta);
    }
}

impl crate::scan::region::Vacant for LogicalLine {
    fn vacant() -> Self {
        LogicalLine::vacant()
    }
}

/// Read every logical line of `src`, in `cr` mode. Exposed for the phase
/// benchmark that separates scanning cost from grouping cost.
#[doc(hidden)]
pub fn read_all(src: &str) -> (Vec<LogicalLine>, Vec<DerivedText>) {
    let mut out = Vec::with_capacity(src.len() / 24 + 1);
    let mut derived = Vec::with_capacity(src.len() / 24 + 1);
    let mut at = 0u32;
    let mut delim = Delimiter::Cr;
    let mut line = 0u32;
    while (at as usize) < src.len() {
        let (l, d) = read_logical_line(src, &mut at, &mut delim, &mut line);
        out.push(l);
        derived.push(d);
    }
    (out, derived)
}

/// Read one logical line starting at `*at`, advancing `*at` past it and
/// updating `*delim` if the line was a `#delimit`.
///
/// Panics never: every branch either advances `i` or breaks.
pub(crate) fn read_logical_line(
    src: &str,
    at: &mut u32,
    delim: &mut Delimiter,
    line: &mut u32,
) -> (LogicalLine, DerivedText) {
    let start = *at as usize;
    let entry = *delim;

    let mut scan = LineScan::new(src.as_bytes(), start, entry);
    scan.run();

    // 02 §2.3: `#` directives are ALWAYS cr-terminated, in either mode. Detected
    // after the fact rather than by peeking, so that `/* c */ #delimit cr` is
    // recognised too.
    if entry == Delimiter::Semi && scan.first_code_is_hash(src.as_bytes()) {
        scan = LineScan::new(src.as_bytes(), start, Delimiter::Cr);
        // The line may begin with the newline that ended the previous `;`
        // statement, and in `cr` mode that newline would terminate it before the
        // `#` is ever reached. In `;` mode a newline is ordinary whitespace, so
        // whitespace BEFORE the first code byte keeps that meaning; only the
        // newline that ends the directive terminates the line.
        scan.lenient_prologue = true;
        scan.run();
    }

    let (derived, code_span) = scan.finish(src);
    let code: &str = match &derived {
        Some(d) => &d.text,
        None => &src[code_span.start as usize..code_span.end as usize],
    };
    let end = scan.i as u32;
    let span = Span {
        start: start as u32,
        end,
    };

    let is_trivia = code.is_empty();
    let directive = directive_of(code);
    let exit = match directive {
        Some(DirectiveKind::DelimitCr) => Delimiter::Cr,
        Some(DirectiveKind::DelimitSemi) => Delimiter::Semi,
        _ => entry,
    };
    *delim = exit;
    *at = end;

    // Physical line numbers are carried forward rather than looked up: a binary
    // search into `LineIndex` per line is ~30 ns and there are one of these per
    // line of the document, twice.
    //
    // `scan.nl` is the newline count of the whole line, accumulated by the
    // scanner as it walked those bytes anyway. The two remaining counts are over
    // the PROLOGUE (whitespace and comments before the first code byte) and the
    // TRAILER (a trailing comment plus the terminator) — a handful of bytes on a
    // real line, where counting over `code_span` directly would re-walk it.
    let b = src.as_bytes();
    let first_line = *line;
    let newlines = scan.nl;
    debug_assert_eq!(newlines, count_nl(&b[start..end as usize]));
    let ends_with_nl = end as usize > start && b[end as usize - 1] == b'\n';
    let last_line = first_line + newlines.saturating_sub(u32::from(ends_with_nl));
    *line = first_line + newlines;
    let (code_first_line, code_last_line) = if code_span.start == code_span.end {
        // Trivia: `code_span` sits at the end of the line, so both numbers are
        // the line the terminator left us on.
        let at = first_line + newlines;
        (at, at)
    } else {
        let before = count_nl(&b[start..code_span.start as usize]);
        let after = count_nl(&b[code_span.end as usize..end as usize]);
        (first_line + before, first_line + newlines - after)
    };

    let l = LogicalLine {
        span,
        code_span,
        first_line,
        last_line,
        code_first_line,
        code_last_line,
        entry_delimiter: entry,
        exit_delimiter: exit,
        brace_delta: scan.brace_delta,
        brace_min: scan.brace_min,
        is_trivia,
        is_blank: is_trivia && !scan.saw_comment,
        is_cell_marker: is_trivia && crate::scan::marker::is_cell_marker(&src[start..end as usize]),
        open_at_end: scan.open_at_end,
        directive,
    };
    (l, derived)
}

/// `#delimit ;` / `#delimit cr`, and every other `#` line.
///
/// Stata accepts abbreviations of `delimit`; `#d ;` is the form that appears in
/// real code, so any non-empty prefix of `delimit` is honoured. A `#` line we do
/// not recognise is still a `Directive` region: it executes and produces no
/// output, which is exactly what `DirectiveKind::Other` means.
fn directive_of(code: &str) -> Option<DirectiveKind> {
    let t = code.trim_start_matches(|c: char| c.is_ascii_whitespace());
    let rest = t.strip_prefix('#')?;
    let word_end = rest
        .find(|c: char| c.is_ascii_whitespace())
        .unwrap_or(rest.len());
    let (word, tail) = rest.split_at(word_end);
    if word.is_empty() || !"delimit".starts_with(word) {
        return Some(DirectiveKind::Other);
    }
    let tail = tail.trim_matches(|c: char| c.is_ascii_whitespace());
    if tail.starts_with(';') {
        Some(DirectiveKind::DelimitSemi)
    } else if tail.starts_with("cr") {
        Some(DirectiveKind::DelimitCr)
    } else {
        Some(DirectiveKind::Other)
    }
}

/// The scanner state of 02 §3.1, as one struct so the `#` re-read is a second
/// construction rather than a rewind.
struct LineScan<'b> {
    b: &'b [u8],
    /// Byte offset this logical line started at.
    start: usize,
    i: usize,
    delim: Delimiter,
    /// Emitted source runs, in ascending order.
    runs: SmallVec<[(u32, u32); 2]>,
    /// Start of the run currently being emitted.
    open: Option<usize>,
    brace_delta: i32,
    brace_min: i32,
    /// Newlines inside `start..i`. Counted here because the scanner already
    /// looks at every one of them; the alternative is three more passes over the
    /// line in `read_logical_line`, which measured a fifth of the whole scan.
    nl: u32,
    saw_comment: bool,
    at_command_start: bool,
    open_at_end: Option<Unterminated>,
    /// A newline before the first code byte is whitespace, not a terminator.
    /// Set only on the forced-`cr` re-read of a `#` directive entered in `;`
    /// mode; see [`read_logical_line`].
    lenient_prologue: bool,
}

impl<'b> LineScan<'b> {
    fn new(b: &'b [u8], start: usize, delim: Delimiter) -> Self {
        Self {
            b,
            start,
            i: start,
            delim,
            runs: SmallVec::new(),
            open: None,
            brace_delta: 0,
            brace_min: 0,
            nl: 0,
            saw_comment: false,
            at_command_start: true,
            open_at_end: None,
            lenient_prologue: false,
        }
    }

    #[inline]
    fn at(&self, k: usize) -> u8 {
        if k < self.b.len() {
            self.b[k]
        } else {
            0
        }
    }

    /// Step over one byte, counting it if it is a newline.
    #[inline]
    fn bump(&mut self) {
        if self.b[self.i] == b'\n' {
            self.nl += 1;
        }
        self.i += 1;
    }

    /// Step over the newline that ends the physical line at `i`, if there is
    /// one. `skip_to_eol` stops ON the newline, never past it.
    #[inline]
    fn eat_eol(&mut self) {
        if self.i < self.b.len() {
            debug_assert_eq!(self.b[self.i], b'\n');
            self.nl += 1;
            self.i += 1;
        }
    }

    #[inline]
    fn open_run(&mut self) {
        if self.open.is_none() {
            self.open = Some(self.i);
        }
    }

    #[inline]
    fn close_run(&mut self) {
        if let Some(s) = self.open.take() {
            if self.i > s {
                self.runs.push((s as u32, self.i as u32));
            }
        }
    }

    /// `//` is a comment when it is at column 0 or preceded by whitespace. `:` is
    /// not whitespace, which is the whole reason `https://x` is not a comment —
    /// it is not a special case anywhere in this scanner.
    ///
    /// **The start of a LOGICAL line counts as column 0.** In `cr` mode the two
    /// are the same thing. In `;` mode they differ for exactly one input,
    /// `di 1 ;// note`, where the `//` follows a `;` with no blank between. 02
    /// §2.1's rule is written about physical columns, so read literally that is
    /// a command named `//`; but `//` is not a command in any Stata, so the only
    /// difference is whether the user gets a comment or r(199). More
    /// importantly, reading it as code breaks 02 §5.4 property 2 outright —
    /// `src[r.span]` would begin at byte 0 of its own buffer, where the same
    /// scanner calls it a comment, so "run this block" would not run this block.
    /// Treating command start as column 0 makes the property hold universally.
    #[inline]
    fn comment_start_here(&self, k: usize) -> bool {
        k == 0 || k == self.start || self.b[k - 1].is_ascii_whitespace()
    }

    fn slash_run(&self, k: usize) -> usize {
        let mut n = 0;
        while self.at(k + n) == b'/' {
            n += 1;
        }
        n
    }

    fn skip_to_eol(&mut self) {
        while self.i < self.b.len() && self.b[self.i] != b'\n' {
            self.i += 1;
        }
    }

    fn run(&mut self) {
        let mut in_string = false;
        let mut compound = 0u32;
        let mut block = 0u32;

        while self.i < self.b.len() {
            if block > 0 {
                if self.at(self.i) == b'/' && self.at(self.i + 1) == b'*' {
                    block += 1;
                    self.i += 2;
                    continue;
                }
                if self.at(self.i) == b'*' && self.at(self.i + 1) == b'/' {
                    block -= 1;
                    self.i += 2;
                    continue;
                }
                self.bump();
                continue;
            }

            if in_string {
                let c = self.b[self.i];
                if c == b'"' {
                    in_string = false;
                    self.i += 1;
                    continue;
                }
                if c == b'\n' {
                    // [V] an unterminated `"` closes at end of physical line.
                    in_string = false;
                    self.nl += 1;
                    if self.delim == Delimiter::Cr {
                        self.close_run();
                        self.i += 1;
                        return;
                    }
                    // In `;` mode the newline is ordinary whitespace and stays.
                    self.i += 1;
                    continue;
                }
                self.i += 1;
                continue;
            }

            if compound > 0 {
                if self.at(self.i) == b'`' && self.at(self.i + 1) == b'"' {
                    compound += 1;
                    self.i += 2;
                    continue;
                }
                if self.at(self.i) == b'"' && self.at(self.i + 1) == b'\'' {
                    compound -= 1;
                    self.i += 2;
                    continue;
                }
                self.bump();
                continue;
            }

            if !self.at_command_start {
                // Fast path: skip the run of bytes that cannot change any state.
                self.open_run();
                let b = self.b;
                let mut k = self.i;
                while k < b.len() && !INTERESTING[b[k] as usize] {
                    k += 1;
                }
                self.i = k;
                if k >= b.len() {
                    break;
                }
            }

            let c = self.b[self.i];

            if self.at_command_start && c == b'*' {
                self.close_run();
                self.saw_comment = true;
                self.star_comment();
                return;
            }

            if c == b'/' && self.at(self.i + 1) == b'*' {
                self.close_run();
                self.saw_comment = true;
                block = 1;
                self.i += 2;
                continue;
            }

            if c == b'/' && self.at(self.i + 1) == b'/' && self.comment_start_here(self.i) {
                let n = self.slash_run(self.i);
                self.close_run();
                self.saw_comment = true;
                self.skip_to_eol();
                if n >= 3 {
                    // [V] continuation: consume the newline and splice with
                    // NOTHING inserted. The spaces on either side are the user's.
                    self.eat_eol();
                    continue;
                }
                if self.delim == Delimiter::Cr {
                    self.eat_eol();
                    return;
                }
                self.eat_eol();
                continue;
            }

            if c == b'"' {
                self.open_run();
                self.at_command_start = false;
                in_string = true;
                self.i += 1;
                continue;
            }

            if c == b'`' && self.at(self.i + 1) == b'"' {
                self.open_run();
                self.at_command_start = false;
                compound = 1;
                self.i += 2;
                continue;
            }

            if self.delim == Delimiter::Semi && c == b';' {
                self.close_run();
                self.i += 1;
                return;
            }
            if self.delim == Delimiter::Cr && c == b'\n' {
                self.nl += 1;
                if self.lenient_prologue && self.at_command_start {
                    self.i += 1;
                    continue;
                }
                self.close_run();
                self.i += 1;
                return;
            }

            if c == b'{' {
                self.brace_delta += 1;
            } else if c == b'}' {
                self.brace_delta -= 1;
                self.brace_min = self.brace_min.min(self.brace_delta);
            }
            if !c.is_ascii_whitespace() {
                self.at_command_start = false;
            } else if c == b'\n' {
                // Reachable only in `;` mode, where a newline is whitespace.
                self.nl += 1;
            }
            self.open_run();
            self.i += 1;
        }

        self.close_run();
        if block > 0 {
            self.open_at_end = Some(Unterminated::BlockComment);
        } else if compound > 0 {
            self.open_at_end = Some(Unterminated::CompoundQuote);
        }
    }

    /// `*` at the first non-blank position of a command: the whole logical line
    /// is a comment. It ends at the newline in `cr` mode and at the `;` in `;`
    /// mode, and `///` inside it swallows the next line [V].
    fn star_comment(&mut self) {
        self.i += 1;
        while self.i < self.b.len() {
            let c = self.b[self.i];
            if self.delim == Delimiter::Semi && c == b';' {
                self.i += 1;
                return;
            }
            if c == b'/'
                && self.at(self.i + 1) == b'/'
                && self.comment_start_here(self.i)
                && self.slash_run(self.i) >= 3
            {
                self.skip_to_eol();
                self.eat_eol();
                continue;
            }
            if c == b'\n' {
                self.nl += 1;
                self.i += 1;
                if self.delim == Delimiter::Semi {
                    continue;
                }
                return;
            }
            self.i += 1;
        }
    }

    /// True when the first non-whitespace emitted byte is `#`.
    fn first_code_is_hash(&self, b: &[u8]) -> bool {
        for &(s, e) in &self.runs {
            for k in s..e {
                let c = b[k as usize];
                if c.is_ascii_whitespace() {
                    continue;
                }
                return c == b'#';
            }
        }
        false
    }

    /// Trim outer whitespace off the emitted runs and materialise `code` + `map`.
    fn finish(&self, src: &str) -> (Option<Box<Derived>>, Span) {
        let b = src.as_bytes();
        let mut runs: SmallVec<[(u32, u32); 2]> = SmallVec::new();
        for &(mut s, e) in &self.runs {
            if runs.is_empty() {
                while s < e && b[s as usize].is_ascii_whitespace() {
                    s += 1;
                }
            }
            if s < e {
                runs.push((s, e));
            }
        }
        while !runs.is_empty() {
            let n = runs.len() - 1;
            while runs[n].1 > runs[n].0 && b[runs[n].1 as usize - 1].is_ascii_whitespace() {
                runs[n].1 -= 1;
            }
            if runs[n].0 < runs[n].1 {
                break;
            }
            runs.pop();
        }

        if runs.is_empty() {
            let at = self.i as u32;
            return (None, Span { start: at, end: at });
        }
        let code_span = Span {
            start: runs[0].0,
            end: runs[runs.len() - 1].1,
        };
        if runs.len() == 1 {
            return (None, code_span);
        }
        let mut out = String::with_capacity(runs.iter().map(|(s, e)| (e - s) as usize).sum());
        let mut map = SpanMap::new();
        for &(s, e) in &runs {
            // Relative to the line's start — see `Derived`.
            map.push(out.len() as u32, s - self.start as u32, e - s);
            out.push_str(&src[s as usize..e as usize]);
        }
        (
            Some(Box::new(Derived {
                text: out.into_boxed_str(),
                map,
            })),
            code_span,
        )
    }
}
