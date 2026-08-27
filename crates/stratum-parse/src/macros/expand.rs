//! The expansion algorithm — design 02 §4.2, transcribed and verified.
//!
//! # Expansion is pure text substitution, and it happens BEFORE parsing
//!
//! [U] 18.3.7: *"macros are expanded before the line is interpreted."* Design 02
//! §1.1 proves that this ordering is observable, not an implementation detail,
//! with two measured cases:
//!
//! * `local q = `"embedded "quote""'` then `di "B13: `q'"` **errors** [V]. The
//!   substituted text contains a bare `"`, so `di "B13: embedded "quote""`
//!   re-tokenizes into different tokens. The expander must not be quote-aware.
//! * `di `="ab"+"cd"'` gives `abcd not found  r(111)` [V]. The `=exp` result is
//!   inserted as a **bare literal with no quoting**, so `di abcd` reads `abcd`
//!   as a variable name.
//!
//! Therefore [`expand`] returns a `String`. Any design that expands macros
//! during lexing is wrong, and would get both of those cases backwards.
//!
//! # The five verified properties this file exists to reproduce
//!
//! 1. **Innermost first.** The macro NAME is expanded before it is looked up:
//!    `local A B` / `local B C` / `` di "``A''" `` → `C` [V].
//! 2. **Undefined is empty, silently.** `` `undefined' `` → `""` [V];
//!    `$notdefined|end` → `|end` [V].
//! 3. **Adjacency.** `'` is a hard delimiter: `` `L1'x `` is the macro `L1`
//!    then `x`, `` `L1x' `` is the macro `L1x` [V].
//! 4. **Globals are maximal-munch.** `$G1x` reads the name `G1x`, not `$G1`
//!    then `x`; `${G1}x` is how the boundary is forced [V].
//! 5. **Substituted text is rescanned**, which is exactly what `macval()` exists
//!    to suppress ([U] 18.3.8).
//!
//! # `` `" `` is not a macro reference
//!
//! The one place the "a backtick opens a macro reference" rule does not hold.
//! `` `"…"' `` is a compound double quote: the delimiters are passed through
//! verbatim so the LEXER can see them, while the contents are expanded normally.
//! Getting this wrong turns `` di `"nested "quoted" text"' `` — which prints
//! `nested "quoted" text` [V], `tests/golden/stata18/semantics.log` — into a
//! lookup of a macro named `"nested "quoted" text"`, i.e. into empty output.

use stratum_proto::Span;

use crate::macros::env::MacroEnv;
use crate::macros::{xmf, ExpandHost, Expansion, StataError};
use crate::spanmap::SpanMap;

/// What an expansion actually did.
///
/// ADR-017: performance is asserted with counters, never durations. These are
/// the counters for the expansion path — `substitutions` is the number of macro
/// references resolved and `host_calls` the number of round trips into the
/// runtime, which is the expensive one because it re-enters the expression
/// evaluator. A macro-free line must score zero on both.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub struct ExpandStats {
    /// Macro references resolved, at every depth.
    pub substitutions: u32,
    /// Calls into [`ExpandHost`] — `=exp` and the extended macro functions
    /// `xmf` could not answer itself.
    pub host_calls: u32,
    /// Deepest recursion reached. `0` for a line with no macros at all.
    pub max_depth: u32,
    /// Bytes appended to the output. Equals the input length exactly when
    /// nothing was substituted.
    pub bytes_out: u32,
}

/// Expand every macro reference in `input`.
///
/// `input` is ONE logical line's code — comments already stripped and `///`
/// continuations already spliced by [`crate::scan::logical`]. The returned
/// [`Expansion::map`] takes an offset in the expanded text back to an offset in
/// `input`; compose it with [`crate::scan::LogicalLine::map`] to reach the
/// original source (spec §21's underline).
pub fn expand(
    input: &str,
    env: &mut MacroEnv,
    host: &mut dyn ExpandHost,
) -> Result<Expansion, StataError> {
    // The overwhelmingly common line has no macro in it at all. Finding that out
    // costs one pass of `memchr`-shaped byte scanning and saves building a piece
    // table, so the keystroke path never allocates a `SpanMap` for `summarize
    // price`.
    if !input.as_bytes().iter().any(|&c| c == b'`' || c == b'$') {
        return Ok(Expansion {
            text: input.to_owned(),
            map: SpanMap::identity(0, input.len() as u32),
            stats: ExpandStats {
                bytes_out: input.len() as u32,
                ..ExpandStats::default()
            },
        });
    }
    let mut ex = Expander {
        out: String::with_capacity(input.len() + input.len() / 2),
        map: SpanMap::new(),
        env,
        host,
        stats: ExpandStats::default(),
    };
    ex.level(input, true, 0)?;
    let stats = ExpandStats {
        bytes_out: ex.out.len() as u32,
        ..ex.stats
    };
    Ok(Expansion {
        text: ex.out,
        map: ex.map,
        stats,
    })
}

struct Expander<'a> {
    out: String,
    map: SpanMap,
    env: &'a mut MacroEnv,
    host: &'a mut dyn ExpandHost,
    stats: ExpandStats,
}

impl Expander<'_> {
    /// One level of the core loop.
    ///
    /// `record` is true only for the top-level input: pieces of the piece table
    /// describe where output bytes came from IN THAT TEXT, and the contents of a
    /// macro have no position in it. Text that came from a macro is therefore a
    /// GAP, which [`SpanMap::to_source`] resolves to the end of the preceding
    /// run — the underline lands on the macro reference that produced the bad
    /// text, which is the useful answer.
    fn level(&mut self, src: &str, record: bool, depth: u32) -> Result<(), StataError> {
        let limits = self.env.limits;
        if depth > limits.max_depth {
            return Err(StataError::new(920, "macro substitution too deep"));
        }
        self.stats.max_depth = self.stats.max_depth.max(depth);
        let b = src.as_bytes();
        let mut i = 0usize;
        let mut lit = 0usize;
        while i < b.len() {
            match b[i] {
                // A compound double quote, not a macro reference. Emit the
                // delimiter verbatim and keep going: the contents expand
                // normally and the LEXER strips the quoting later.
                b'`' if b.get(i + 1) == Some(&b'"') => i += 2,
                b'`' => {
                    let Some(j) = match_backtick(b, i) else {
                        // An unmatched backtick is a literal byte, not an error.
                        // Stata prints the line with the backtick in it.
                        i += 1;
                        continue;
                    };
                    self.flush(src, lit, i, record)?;
                    let mark = self.out.len();
                    let inner_raw = &src[i + 1..j];
                    let inner = self.expanded_string(inner_raw, depth + 1)?;
                    let (text, rescan) = self.classify(&inner, depth + 1)?;
                    self.stats.substitutions += 1;
                    if rescan {
                        self.level(&text, false, depth + 1)?;
                    } else {
                        self.push(&text)?;
                    }
                    self.anchor(mark, i, record);
                    i = j + 1;
                    lit = i;
                }
                b'$' => {
                    let (name, next) = match b.get(i + 1) {
                        Some(&b'{') => {
                            let Some(k) = match_brace(b, i + 1) else {
                                i += 1;
                                continue;
                            };
                            let raw = &src[i + 2..k];
                            (self.expanded_string(raw, depth + 1)?, k + 1)
                        }
                        _ => {
                            let n = greedy_name(src, i + 1);
                            if n.is_empty() {
                                // `$5`, `$ `, `$$` — a literal dollar sign.
                                i += 1;
                                continue;
                            }
                            (n.to_owned(), i + 1 + n.len())
                        }
                    };
                    self.flush(src, lit, i, record)?;
                    let mark = self.out.len();
                    let value = self.env.global(&name).unwrap_or("").to_owned();
                    self.stats.substitutions += 1;
                    self.level(&value, false, depth + 1)?;
                    self.anchor(mark, i, record);
                    i = next;
                    lit = i;
                }
                c => i += crate::lex::utf8_len(c),
            }
        }
        self.flush(src, lit, b.len(), record)?;
        Ok(())
    }

    fn flush(&mut self, src: &str, from: usize, to: usize, record: bool) -> Result<(), StataError> {
        if to <= from {
            return Ok(());
        }
        if record {
            self.map
                .push(self.out.len() as u32, from as u32, (to - from) as u32);
        }
        self.out.push_str(&src[from..to]);
        self.check_len()
    }

    /// Anchor substituted output to the macro reference that produced it.
    ///
    /// Without this a substitution is a pure GAP, and `SpanMap::to_source` of an
    /// offset inside it falls back to the SOURCE START OF THE NEXT RUN — the
    /// byte after the reference, or worse, byte 0 of the following literal when
    /// the reference began the line. A one-byte piece at the reference makes
    /// every offset in the substituted text resolve INTO the reference, which is
    /// what spec §21's underline needs: the squiggle lands on `` `v' ``, the
    /// thing the user can edit, not on whatever happens to follow it.
    ///
    /// One byte and not the whole run because a piece maps equal-length runs and
    /// the substituted text is almost never the length of the reference.
    fn anchor(&mut self, mark: usize, ref_start: usize, record: bool) {
        if record && self.out.len() > mark {
            self.map.push(mark as u32, ref_start as u32, 1);
        }
    }

    fn push(&mut self, s: &str) -> Result<(), StataError> {
        self.out.push_str(s);
        self.check_len()
    }

    /// Every append goes through here.
    ///
    /// The cap has to be checked on the LITERAL path as well as the substituted
    /// one: a self-referential `local a x`a''` grows by copying literal bytes
    /// each time round, so a check that only fired on `macval` would let it run
    /// to the depth limit having already built a gigabyte.
    fn check_len(&mut self) -> Result<(), StataError> {
        if self.out.len() > self.env.limits.max_expanded_len as usize {
            return Err(StataError::new(920, "macro substitution too long"));
        }
        Ok(())
    }

    /// Expand `src` into a fresh `String` rather than into `out`. This is the
    /// INNERMOST-FIRST step: the name of a macro reference is itself expanded
    /// before the lookup happens.
    fn expanded_string(&mut self, src: &str, depth: u32) -> Result<String, StataError> {
        if !src.as_bytes().iter().any(|&c| c == b'`' || c == b'$') {
            return Ok(src.to_owned());
        }
        let saved = std::mem::take(&mut self.out);
        self.level(src, false, depth)?;
        Ok(std::mem::replace(&mut self.out, saved))
    }

    /// Design 02 §4.2's `classify`, in its exact order. Returns the substituted
    /// text and whether it is rescanned.
    fn classify(&mut self, inner: &str, depth: u32) -> Result<(String, bool), StataError> {
        // `macval(NAME)` — the ONE non-rescanned path ([U] 18.3.8, "confines the
        // macro expansion to the first level").
        if let Some(rest) = inner.strip_prefix("macval(") {
            if let Some(name) = rest.strip_suffix(')') {
                return Ok((self.env.local(name.trim()).unwrap_or("").to_owned(), false));
            }
        }
        if let Some(exp) = inner.strip_prefix('=') {
            self.stats.host_calls += 1;
            return Ok((self.host.eval_expr_to_macro_text(exp)?, true));
        }
        if let Some(body) = inner.strip_prefix(':') {
            // 02 §4.3 puts the text-only extended macro functions in `xmf.rs`;
            // everything that needs the dataset, the stored results or the file
            // system goes to the runtime. Trying `xmf` first is what keeps
            // `` `: word count a b c' `` — a pure string operation — from
            // needing a live engine in a parser test.
            return match xmf::eval(body, self.env) {
                Some(v) => Ok((v?, true)),
                None => {
                    self.stats.host_calls += 1;
                    Ok((self.host.eval_xmf(body)?, true))
                }
            };
        }
        if let Some(name) = inner.strip_prefix("++") {
            return Ok((self.env.pre_step(name.trim(), 1.0), true));
        }
        if let Some(name) = inner.strip_prefix("--") {
            return Ok((self.env.pre_step(name.trim(), -1.0), true));
        }
        if let Some(name) = inner.strip_suffix("++") {
            return Ok((self.env.post_step(name.trim(), 1.0), true));
        }
        if let Some(name) = inner.strip_suffix("--") {
            return Ok((self.env.post_step(name.trim(), -1.0), true));
        }
        let _ = depth;
        // All-digit names are a program's positional arguments; they live in the
        // same local map, so this needs no separate branch beyond the note that
        // `` `1' `` outside a program is empty rather than an error [V].
        Ok((self.env.local(inner).unwrap_or("").to_owned(), true))
    }
}

/// Find the `'` that closes the `` ` `` at `i`, counting nested pairs.
///
/// This is what makes property 3 (adjacency) work: `` `L1'x `` closes at the
/// first `'` because nothing re-opened, while `` ``A'' `` closes at the second
/// because the inner `` ` `` pushed the depth to 2.
pub fn match_backtick(b: &[u8], i: usize) -> Option<usize> {
    debug_assert_eq!(b[i], b'`');
    let mut depth = 0usize;
    let mut k = i;
    while k < b.len() {
        match b[k] {
            b'`' => depth += 1,
            b'\'' => {
                depth -= 1;
                if depth == 0 {
                    return Some(k);
                }
            }
            _ => {}
        }
        k += 1;
    }
    None
}

/// Find the `}` closing the `{` at `i`, nesting-aware.
fn match_brace(b: &[u8], i: usize) -> Option<usize> {
    debug_assert_eq!(b[i], b'{');
    let mut depth = 0usize;
    let mut k = i;
    while k < b.len() {
        match b[k] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(k);
                }
            }
            _ => {}
        }
        k += 1;
    }
    None
}

/// Maximal munch of a global name at `i`: `[A-Za-z_][A-Za-z0-9_]{0,31}`.
///
/// Property 4. `$G1x` yields `G1x`, and the manual's own `$drivemyfile.dta`
/// example agrees — the `.` stops it, the `x` does not. Globals may not begin
/// with a digit ([U] 11.3, `global 1x` is r(198) [V]), so `$5` yields nothing
/// and the `$` stays a literal byte.
pub fn greedy_name(src: &str, i: usize) -> &str {
    if i >= src.len() || !crate::lex::is_ident_start(src, i) {
        return "";
    }
    let mut k = i;
    let mut chars = 0usize;
    while k < src.len() && crate::lex::is_ident_continue(src, k) && chars < 32 {
        k += crate::lex::utf8_len(src.as_bytes()[k]);
        chars += 1;
    }
    &src[i..k]
}

/// A span in the expanded text, back to a span in the ORIGINAL source.
///
/// The composition spec §21 needs: `expanded ──Expansion::map──▶ code
/// ──LogicalLine::map──▶ source`.
pub fn to_source(exp: &Expansion, line: &SpanMap, s: Span) -> Span {
    let composed = exp.map.compose(line);
    let parts = composed.span_to_source(s);
    Span {
        start: parts.first().map_or(0, |p| p.start),
        end: parts.last().map_or(0, |p| p.end),
    }
}
