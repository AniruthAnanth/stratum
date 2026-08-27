//! Completion — design 07 §7.1, spec §22.
//!
//! **Lane A: deterministic, synchronous, offline.** It works with no network, no
//! key and no loaded dataset (the varlist sources just return empty), and it is
//! a better completion experience than most statistical software ships. Lane B —
//! the AI ghost text — is off by default, lives in a different UI channel, and
//! `complete()` never waits on it: this function returns before any network
//! request could be created, because there is no code path here that can create
//! one.
//!
//! # Accuracy comes from the role, not from the ranker
//!
//! Sources are dispatched on the syntactic role the cursor is in ([`Role`]),
//! which is why the popup does not offer variable names where only an option can
//! go. Getting the role right is most of what makes this feel accurate; the
//! ranking is the last few percent.
//!
//! # The budget, and how it is proven
//!
//! §14 puts a hard 2 ms contract on `complete`, measured **at the A11 cap**:
//! 2 048 variables and 512 of everything else. Per ADR-017 the *gate* is a
//! counter, not a stopwatch: [`CompletionList::scanned`] is every candidate the
//! ranker looked at, and it is bounded by [`CANDIDATE_CEILING`] — a constant
//! derived from the wire caps plus the two static tables. The stronger property
//! is structural: **there is no observation input to this module at all.**
//! [`crate::Env`] carries names and never values, so no amount of data makes
//! this slower. `benches/complete.rs` records the wall clock alongside.

pub mod commands;
pub mod macros;
pub mod options;
pub mod paths;
pub mod rank;
pub mod varnames;

use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use stratum_parse::ast::StoredClass;
use stratum_proto::complete::{COMPLETION_ENV_MAX_OTHER, COMPLETION_ENV_MAX_VARS};

use crate::{Env, ParseIndex};

pub use rank::{Ranker, Tier};

/// Rows the popup is ever given.
///
/// It renders twelve; the rest are for the user who keeps typing without
/// re-triggering. Beyond a few hundred a list has stopped being a completion and
/// become a search result.
pub const MAX_ITEMS: usize = 256;

/// The most candidates any single `complete()` call can examine.
///
/// The A11 wire caps (2 048 variables, 512 for each of the ten other lists) plus
/// the two static tables. This is the ADR-017 counter's ceiling: a test asserts
/// `scanned <= CANDIDATE_CEILING` at the cap, which is what "the popup does one
/// bounded pass" means as a machine-checkable statement.
pub const CANDIDATE_CEILING: u32 = (COMPLETION_ENV_MAX_VARS + 10 * COMPLETION_ENV_MAX_OTHER) as u32
    + 4_096  // command table headroom
    + 4_096; // function table headroom

/// What a completion item is.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionKind {
    /// A Stata command in command position.
    Command,
    /// An option inside the current command's option list.
    Option,
    /// A variable in the active frame.
    Variable,
    /// A local macro, offered after `` ` ``.
    Local,
    /// A global macro, offered after `$`.
    Global,
    /// A scalar.
    Scalar,
    /// A matrix.
    Matrix,
    /// A frame name.
    Frame,
    /// A value-label name.
    ValueLabel,
    /// A stored estimate name.
    StoredEstimate,
    /// An `e()`, `r()` or `s()` member.
    StoredResult,
    /// A built-in function.
    Function,
    /// A path, inside a quoted filename or after `using`.
    Path,
    /// A language keyword (`if`, `in`, `using`, `by`).
    Keyword,
    /// A multi-line snippet (`foreach`, `program`, `preserve`/`restore`).
    Snippet,
}

/// One row of the popup.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct CompletionItem {
    /// The text shown in the popup.
    pub label: String,
    /// What it is.
    pub kind: CompletionKind,
    /// Right-aligned annotation: a signature, a frame name, an option's arity.
    /// **Never a variable label** — A11 fetches those for the visible rows only,
    /// off the keystroke path.
    pub detail: Option<String>,
    /// Text to insert when it differs from `label` (`"strpos("`).
    pub insert: Option<String>,
    /// Sort rank, ascending. The popup renders items as given; `rank` is here so
    /// a consumer that re-sorts cannot silently change the order.
    pub rank: i32,
}

/// The result of [`complete`].
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct CompletionList {
    /// Byte range the accepted item replaces — the token under the cursor, not
    /// the cursor position, so `reg pri` completes `pri` rather than appending.
    pub from: u32,
    /// End of the replaced range.
    pub to: u32,
    /// Ordered; the popup renders them as given.
    pub items: Vec<CompletionItem>,
    /// Candidates that survived [`MAX_ITEMS`].
    pub offered: u32,
    /// Candidates that matched. Equal to `offered` unless the cap bound.
    pub total: u32,
    /// **The ADR-017 counter.** Every candidate the ranker examined, matched or
    /// not. Bounded by [`CANDIDATE_CEILING`] and independent of the number of
    /// observations in the dataset.
    pub scanned: u32,
}

/// Which syntactic role the cursor sits in.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Role {
    /// The first word of a statement.
    Command,
    /// After the command's comma. Carries the canonical command name when it
    /// resolved.
    Option(Option<&'static str>),
    /// After a `` ` ``.
    Local,
    /// After a `$`.
    Global,
    /// Inside `e(`, `r(` or `s(`.
    Stored(StoredClass),
    /// Inside a quoted filename, or after `using`.
    Path,
    /// Where a frame name goes.
    Frame,
    /// Where a value-label name goes.
    ValueLabel,
    /// Anywhere else in a command: variables, functions, keywords.
    Expr,
}

/// Per-document statistics the ranker uses as a tie-break.
///
/// Built once per document version by the caller and reused across keystrokes.
/// Rebuilding it per keystroke would be one pass over the file per character —
/// affordable at the calibrated corpus size (StataCorp's own `ado` library has a
/// median program of 2.0 KiB and nothing above 512 KiB) but pointless, since the
/// answer only changes when the document does.
#[derive(Clone, Debug, Default)]
pub struct FileIndex {
    frequency: FxHashMap<String, u32>,
    programs: Vec<String>,
}

impl FileIndex {
    /// Count identifier occurrences and collect `program define` names.
    #[must_use]
    pub fn new(idx: &ParseIndex<'_>) -> Self {
        use stratum_proto::token::TokenKind;
        let mut frequency: FxHashMap<String, u32> = FxHashMap::default();
        for (i, _line, code) in idx.statements() {
            let _ = i;
            for tok in stratum_parse::lex(code) {
                if tok.kind != TokenKind::Ident {
                    continue;
                }
                if let Some(t) = code.get(tok.span.start as usize..tok.span.end as usize) {
                    *frequency.entry(t.to_owned()).or_default() += 1;
                }
            }
        }
        let doc = crate::lints::Doc::build(idx);
        FileIndex {
            frequency,
            programs: crate::diagnose::didyoumean::user_programs(&doc),
        }
    }

    /// How often a name appears in this file.
    #[must_use]
    pub fn frequency(&self, name: &str) -> u32 {
        self.frequency.get(name).copied().unwrap_or(0)
    }

    /// `program define` names, in document order.
    #[must_use]
    pub fn programs(&self) -> &[String] {
        &self.programs
    }
}

/// Everything one completion needs.
pub struct CompletionContext<'a> {
    /// The buffer.
    pub text: &'a str,
    /// Byte offset of the caret.
    pub cursor: usize,
    /// Names the caller knows about. `Env::default()` is a valid, useful
    /// argument: commands, functions, keywords and snippets still complete.
    pub env: &'a Env,
    /// Per-document statistics, when the caller cached them.
    pub file: Option<&'a FileIndex>,
}

impl<'a> CompletionContext<'a> {
    /// A context over a buffer with no cached statistics.
    #[must_use]
    pub fn new(text: &'a str, cursor: usize, env: &'a Env) -> Self {
        CompletionContext {
            text,
            cursor,
            env,
            file: None,
        }
    }

    fn frequency(&self, name: &str) -> u32 {
        self.file.map_or(0, |f| f.frequency(name))
    }

    /// Position of `name` in the session's recent list, or `u32::MAX`.
    fn recency(&self, name: &str) -> u32 {
        self.env
            .recent
            .iter()
            .position(|n| n == name)
            .map_or(u32::MAX, |i| i as u32)
    }
}

/// Complete at the caret. Synchronous, allocation-bounded, offline.
#[must_use]
pub fn complete(ctx: &CompletionContext<'_>) -> CompletionList {
    let cursor = ctx.cursor.min(ctx.text.len());
    let word = word_at(ctx.text, cursor);
    let typed = ctx.text.get(word.0..cursor).unwrap_or("");
    let role = role_at(ctx.text, word.0);

    let mut r = Ranker::new(typed);
    match &role {
        Role::Command => commands::offer(&mut r, ctx),
        Role::Option(cmd) => options::offer(&mut r, *cmd),
        Role::Local => macros::offer_local(&mut r, ctx),
        Role::Global => macros::offer_global(&mut r, ctx),
        Role::Stored(class) => varnames::offer_stored(&mut r, ctx, *class),
        Role::Path => paths::offer(&mut r, ctx),
        Role::Frame => varnames::offer_frames(&mut r, ctx),
        Role::ValueLabel => varnames::offer_value_labels(&mut r, ctx),
        Role::Expr => varnames::offer_expr(&mut r, ctx),
    }
    let scanned = r.scanned;
    let (items, offered, total) = r.finish();
    CompletionList {
        from: word.0 as u32,
        to: cursor as u32,
        items,
        offered,
        total,
        scanned,
    }
}

/// The extent of the identifier-ish token the caret is inside.
///
/// Returns `(start, end)`. `.` is included because `L.gnp` and `i.rep78` are one
/// token to the user even though the lexer sees three.
#[must_use]
pub fn word_at(text: &str, cursor: usize) -> (usize, usize) {
    let b = text.as_bytes();
    let is_word = |c: u8| c.is_ascii_alphanumeric() || c == b'_' || c == b'.';
    let mut start = cursor.min(b.len());
    while start > 0 && b.get(start - 1).copied().is_some_and(is_word) {
        start -= 1;
    }
    let mut end = cursor.min(b.len());
    while end < b.len() && b.get(end).copied().is_some_and(is_word) {
        end += 1;
    }
    (start, end)
}

/// Which role the token starting at `token_start` plays.
#[must_use]
pub fn role_at(text: &str, token_start: usize) -> Role {
    let prefix = statement_prefix(text, token_start);
    let b = prefix.as_bytes();

    // A macro sigil immediately before the token wins over everything: `` ` ``
    // and `$` are unambiguous.
    match b.last() {
        Some(b'`') => return Role::Local,
        Some(b'$') => return Role::Global,
        _ => {}
    }
    // `e(`, `r(`, `s(` with a partially typed key.
    if let Some(class) = stored_class_open(prefix) {
        return Role::Stored(class);
    }
    if unterminated_quote(prefix) {
        return Role::Path;
    }

    let head = strip_prefixes(prefix.trim_start());
    let mut words = head.split_whitespace();
    let Some(first) = words.next() else {
        return Role::Command;
    };
    // Still typing the first word: the token IS the command.
    if head.trim_end() == first && !head.ends_with(char::is_whitespace) {
        return Role::Command;
    }
    // Options live after the one comma. `top_level_comma` ignores commas inside
    // parentheses and strings, which is where option arguments put theirs.
    if top_level_comma(head) {
        return Role::Option(stratum_parse::table().canonical(first).map(|s| s.canonical));
    }
    let rest: Vec<&str> = words.collect();
    if rest.last() == Some(&"frame") {
        return Role::Frame;
    }
    // `using` opens the filename, and the filename runs to the end of the
    // statement — a top-level comma has already returned `Option` above. The
    // question is whether `using` is BEHIND the caret, not whether it is
    // immediately before it: by the time the user has typed part of the path,
    // `using proj/data/wave20` leaves `data/` as the previous word, and asking
    // only about the previous word dropped the popup back to variable names
    // exactly when it had the most to offer.
    if rest.contains(&"using") {
        return Role::Path;
    }
    if first == "frame" && rest.len() <= 1 {
        return Role::Frame;
    }
    if first.starts_with("lab") && rest.first().is_some_and(|w| "values".starts_with(w)) {
        // `label values <var> <lblname>`: the label name is the second argument.
        if rest.len() >= 2 {
            return Role::ValueLabel;
        }
    }
    if first.starts_with("use") || first == "do" || first == "include" || first == "run" {
        return Role::Path;
    }
    Role::Expr
}

/// The text from the start of the current statement to `upto`.
///
/// Walks back over `///` continuations so a command word on a previous physical
/// line is still found, and stops at a `;` so a `#delimit ;` body does not read
/// back to the top of the file.
fn statement_prefix(text: &str, upto: usize) -> &str {
    let head = text.get(..upto).unwrap_or("");
    let mut start = head.rfind('\n').map_or(0, |i| i + 1);
    loop {
        let before = head.get(..start.saturating_sub(1)).unwrap_or("");
        let prev_start = before.rfind('\n').map_or(0, |i| i + 1);
        let prev = before.get(prev_start..).unwrap_or("");
        if start > 0 && prev.trim_end().ends_with("///") {
            start = prev_start;
            continue;
        }
        break;
    }
    let line = head.get(start..).unwrap_or("");
    match line.rfind(';') {
        Some(i) => line.get(i + 1..).unwrap_or(""),
        None => line,
    }
}

/// Commands that can carry a `:` prefix clause (`by rep78: summarize`).
///
/// A NAME test, not "the first colon on the line". A colon is also ordinary
/// argument syntax — `merge 1:1`, `merge m:1`, `frlink 1:1` — and splitting
/// there left `1` sitting in the command position, which emptied the option list
/// for every one of those commands and hid the filename after their `using`.
const COLON_PREFIXES: &[&str] = &[
    "bootstrap",
    "by",
    "bys",
    "bysort",
    "cap",
    "capture",
    "fp",
    "jackknife",
    "mi",
    "nestreg",
    "noi",
    "noisily",
    "permute",
    "qui",
    "quietly",
    "rolling",
    "simulate",
    "statsby",
    "stepwise",
    "svy",
    "sw",
    "version",
    "xi",
];

/// Strip prefix commands so `capture noisily reg` still completes a command.
fn strip_prefixes(mut s: &str) -> &str {
    loop {
        let word = s.split_whitespace().next().unwrap_or("");
        // Bare-word prefixes come off FIRST. `capture merge 1:1 …` has to lose
        // its `capture` before the colon rule reads the line, or the match-type
        // colon in `1:1` looks like the end of a `by`-style prefix clause.
        if matches!(
            word,
            "capture" | "cap" | "quietly" | "qui" | "noisily" | "noi"
        ) && s.len() > word.len()
        {
            s = s.get(word.len()..).unwrap_or("").trim_start();
            continue;
        }
        if let Some(i) = s.find(':') {
            // `by foreign:` / `quietly:` — everything through the colon is a
            // prefix, and the command position is after it. `quietly:` arrives
            // with the colon glued on, hence the trim.
            if COLON_PREFIXES.contains(&word.trim_end_matches(':'))
                && !s.get(..i).is_some_and(|h| h.contains('"'))
            {
                s = s.get(i + 1..).unwrap_or("").trim_start();
                continue;
            }
        }
        return s;
    }
}

/// `e(`, `r(` or `s(` open at the end of the prefix.
fn stored_class_open(prefix: &str) -> Option<StoredClass> {
    let t = prefix.trim_end_matches(|c: char| c.is_alphanumeric() || c == '_');
    let t = t.strip_suffix('(')?;
    let cls = t.as_bytes().last()?;
    // A letter before the sigil means it is a longer identifier, not `e(`.
    let before = t
        .get(..t.len() - 1)
        .and_then(|s| s.as_bytes().last().copied());
    if before.is_some_and(|c| c.is_ascii_alphanumeric() || c == b'_') {
        return None;
    }
    match cls {
        b'e' => Some(StoredClass::E),
        b'r' => Some(StoredClass::R),
        b's' => Some(StoredClass::S),
        b'c' => Some(StoredClass::C),
        _ => None,
    }
}

fn unterminated_quote(prefix: &str) -> bool {
    let mut open = false;
    let mut prev = b' ';
    for c in prefix.bytes() {
        if c == b'"' && prev != b'\\' {
            open = !open;
        }
        prev = c;
    }
    open
}

fn top_level_comma(s: &str) -> bool {
    let mut depth = 0i32;
    let mut in_str = false;
    for c in s.bytes() {
        match c {
            b'"' => in_str = !in_str,
            b'(' | b'[' if !in_str => depth += 1,
            b')' | b']' if !in_str => depth -= 1,
            b',' if !in_str && depth <= 0 => return true,
            _ => {}
        }
    }
    false
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;

    fn role(src: &str) -> Role {
        // The caret is written as `|`.
        let cursor = src.find('|').unwrap_or(src.len());
        let text = src.replace('|', "");
        let (start, _) = word_at(&text, cursor);
        role_at(&text, start)
    }

    #[test]
    fn the_first_word_of_a_statement_is_a_command() {
        assert_eq!(role("reg|"), Role::Command);
        assert_eq!(role("|"), Role::Command);
        assert_eq!(role("di 1\nsum|"), Role::Command);
        assert_eq!(role("capture noisily reg|"), Role::Command);
        assert_eq!(role("by foreign: reg|"), Role::Command);
    }

    #[test]
    fn after_the_comma_only_options_are_offered() {
        assert!(matches!(
            role("summarize price, det|"),
            Role::Option(Some("summarize"))
        ));
        assert!(matches!(role("nosuchcmd price, det|"), Role::Option(None)));
        // A comma inside an option argument does not open a second option list.
        assert!(matches!(
            role("regress y x, vce(cluster id) rob|"),
            Role::Option(_)
        ));
    }

    #[test]
    fn a_macro_sigil_is_unambiguous() {
        assert_eq!(role("summarize `out|"), Role::Local);
        assert_eq!(role("summarize $glo|"), Role::Global);
    }

    #[test]
    fn stored_results_complete_inside_their_parentheses() {
        assert_eq!(role("display e(N|"), Role::Stored(StoredClass::E));
        assert_eq!(role("display r(me|"), Role::Stored(StoredClass::R));
        // `price(` is not `e(`.
        assert_eq!(role("gen x = strpos(a|"), Role::Expr);
    }

    #[test]
    fn a_filename_position_completes_paths() {
        assert_eq!(role("use data/w|"), Role::Path);
        assert_eq!(role("merge 1:1 pid using data/w|"), Role::Path);
        assert_eq!(role("import delimited \"raw/x|"), Role::Path);
    }

    /// A colon inside a command's own arguments is not a prefix clause. Without
    /// this, `merge`, `frlink` and every other match-type command lost both its
    /// option list and its `using` path completion.
    #[test]
    fn a_match_type_colon_is_not_a_prefix_clause() {
        assert!(matches!(
            role("merge 1:1 pid using x.dta, keep|"),
            Role::Option(Some("merge"))
        ));
        assert_eq!(role("capture merge m:1 pid using dat/w|"), Role::Path);
        // A real prefix clause still comes off.
        assert!(matches!(
            role("bysort rep78: summarize price, det|"),
            Role::Option(Some("summarize"))
        ));
        assert_eq!(role("quietly: reg|"), Role::Command);
    }

    #[test]
    fn a_continuation_keeps_the_command_context() {
        assert!(matches!(
            role("regress price ///\n    mpg, rob|"),
            Role::Option(Some("regress"))
        ));
        assert_eq!(role("regress price ///\n    mp|"), Role::Expr);
    }

    /// **ADR-017's gate.** The popup's cost is asserted with a COUNTER, never a
    /// stopwatch: at the A11 wire cap the ranker still makes one bounded pass,
    /// which is exactly what `scanned <= CANDIDATE_CEILING` says. An empty
    /// prefix is the worst case, because every candidate matches.
    #[test]
    fn the_candidate_count_is_bounded_at_the_a11_cap() {
        fn names(n: usize, prefix: &str) -> Vec<String> {
            (0..n).map(|i| format!("{prefix}{i:05}")).collect()
        }
        let other = COMPLETION_ENV_MAX_OTHER;
        let env = Env {
            varnames: Some(names(COMPLETION_ENV_MAX_VARS, "v")),
            locals: names(other, "l"),
            globals: names(other, "g"),
            scalars: names(other, "s"),
            matrices: names(other, "m"),
            e_names: names(other, "e"),
            r_names: names(other, "r"),
            s_names: names(other, "t"),
            frames: names(other, "f"),
            value_labels: names(other, "b"),
            stored_estimates: names(other, "q"),
            ..Env::default()
        };
        for src in ["", "summarize ", "summarize `", "summarize $", "frame "] {
            let list = complete(&CompletionContext::new(src, src.len(), &env));
            assert!(
                list.scanned <= CANDIDATE_CEILING,
                "{src:?} scanned {}, ceiling {CANDIDATE_CEILING}",
                list.scanned
            );
        }
    }

    #[test]
    fn the_replaced_range_is_the_token_not_the_caret() {
        let env = Env::default();
        let list = complete(&CompletionContext::new("reg pri", 7, &env));
        assert_eq!((list.from, list.to), (4, 7));
    }

    #[test]
    fn everything_works_with_an_empty_environment() {
        let env = Env::default();
        let list = complete(&CompletionContext::new("reg", 3, &env));
        assert!(
            list.items.iter().any(|i| i.label == "regress"),
            "commands complete with no dataset, no key and no network"
        );
    }
}
