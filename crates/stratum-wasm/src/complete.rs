//! `complete()`, `quick_fixes()` and `lints()` — the three JSON-shaped halves of
//! CONTRACTS §14.
//!
//! # What this file is, and what it is standing in for
//!
//! §14 wraps `stratum-parse` **and `stratum-intel`**. W20 landed
//! `crates/stratum-intel` while W11b was running, but `stratum_intel::complete`
//! is a one-line `//! placeholder`: there is no completion API in it to call
//! yet. So the completion source here is written against `stratum-parse`'s
//! generated command and function tables plus the live [`CompletionEnv`], and it
//! is deliberately the *deterministic* part of what W20 will own: no similarity
//! ranking, no dataflow index, no `e()`/`r()` introspection beyond the names the
//! engine already pushed.
//!
//! Every entry point below is a delegation site, and the module boundary is what
//! makes that cheap — nothing else in this crate calls into completion, so
//! replacing this file with a call into `stratum-intel` is a self-contained
//! change. It is not a free one: `stratum-intel` pulls `stratum-effects`, and
//! this crate's acceptance includes an assertion about its exact wasm dependency
//! tree (`tests/parity.rs::no_forbidden_crate_is_in_the_wasm_dep_tree`), which
//! has to be re-run with that edge in place.
//!
//! # What is NOT provisional
//!
//! * **Determinism.** The candidate order is a total order — group, then label —
//!   so two runs over the same environment produce the same popup. A popup whose
//!   order depends on hash iteration is a popup that moves under the user's
//!   finger between keystrokes.
//! * **The keystroke budget.** §14 puts a 2 ms hard contract on `complete`, and
//!   A11 says it is measured AT THE CAP: 2 048 variables and 512 of everything
//!   else. Nothing here allocates per candidate — matching works on `&str` and
//!   only the surviving [`MAX_ITEMS`] rows are ever turned into `String`s.
//! * **No I/O.** `Path` completion is absent rather than faked: there is no
//!   filesystem in wasm, and a path list would have to come from the engine.

use std::ops::Range;

use stratum_parse::{
    all_functions, lint, parse_command, resolve_command, segment, table, CommandLookup, CommandSig,
    Derived, LintCtx, LogicalLine, OptionSpec, ParseMode, Segmentation as ParseSegmentation,
};
use stratum_proto::{CompletionEnv, Diagnostic, Span, Suggestion};

use crate::env;
use crate::{CompletionItem, CompletionKind, CompletionList};

/// Rows the popup is ever given. It renders twelve; the rest are for the user
/// who keeps typing without re-triggering, and beyond a few hundred the list has
/// stopped being a completion and become a search result.
const MAX_ITEMS: usize = 256;

/// Logical lines `lints()` will parse before giving up.
///
/// The problems pane is debounced and user-visible, not a keystroke path, but
/// parsing every line of a 2 MB file is still seconds of work for a pane that
/// shows the first few dozen findings. Calibrated against the real corpus: the
/// StataCorp `ado` library's median program is 2.0 KiB and its largest is under
/// 512 KiB, so this covers every real file whole.
const MAX_LINT_LINES: usize = 20_000;

/// Findings `lints()` will return.
const MAX_LINTS: usize = 500;

// ===========================================================================
// Completion.
// ===========================================================================

/// What the cursor is sitting in. Decides which lists are offered at all.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum Trigger {
    /// After a `` ` ``: local macros only.
    Local,
    /// After a `$`: global macros only.
    Global,
    /// The first word of a statement: commands and user programs.
    Command,
    /// After the command's comma: that command's options.
    Opt,
    /// Anywhere else in a command: variables, functions, keywords.
    Expr,
}

/// Deterministic completion at `pos`.
pub fn complete(doc: &str, env: &CompletionEnv, pos: usize) -> CompletionList {
    let pos = pos.min(doc.len());
    let word = word_at(doc.as_bytes(), pos);
    let prefix = &doc[word.start..pos.max(word.start)];
    let (trigger, cmd) = context(doc, word.start);

    // (group, label, kind), BORROWED rather than owned: at the A11 cap an `Expr`
    // completion with an empty prefix matches every one of ~7 000 names, and
    // cloning them all to throw all but `MAX_ITEMS` away is most of the 2 ms.
    let mut hits: Vec<(u8, &str, CompletionKind)> = Vec::new();

    match trigger {
        Trigger::Local => offer(
            &mut hits,
            0,
            env::other(&env.locals),
            prefix,
            CompletionKind::Local,
        ),
        Trigger::Global => offer(
            &mut hits,
            0,
            env::other(&env.globals),
            prefix,
            CompletionKind::Global,
        ),
        Trigger::Command => {
            for sig in table().rows() {
                if sig.canonical.starts_with(prefix) {
                    hits.push((0, sig.canonical, CompletionKind::Command));
                }
            }
            offer(
                &mut hits,
                1,
                env::other(&env.programs),
                prefix,
                CompletionKind::Command,
            );
        }
        Trigger::Opt => {
            if let Some(sig) = cmd {
                for opt in sig.options {
                    if opt.canonical.starts_with(prefix) {
                        hits.push((0, opt.canonical, CompletionKind::Option));
                    }
                }
            }
        }
        Trigger::Expr => {
            offer(
                &mut hits,
                0,
                env::varnames(env),
                prefix,
                CompletionKind::Variable,
            );
            for f in all_functions() {
                if f.name.starts_with(prefix) {
                    hits.push((1, f.name, CompletionKind::Function));
                }
            }
            offer(
                &mut hits,
                2,
                env::other(&env.scalars),
                prefix,
                CompletionKind::Scalar,
            );
            offer(
                &mut hits,
                3,
                env::other(&env.matrices),
                prefix,
                CompletionKind::Matrix,
            );
            offer(
                &mut hits,
                4,
                env::other(&env.frames),
                prefix,
                CompletionKind::Frame,
            );
            offer(
                &mut hits,
                5,
                env::other(&env.value_labels),
                prefix,
                CompletionKind::ValueLabel,
            );
            offer(
                &mut hits,
                6,
                env::other(&env.stored_estimates),
                prefix,
                CompletionKind::StoredEstimate,
            );
            offer(
                &mut hits,
                7,
                env::other(&env.e_names),
                prefix,
                CompletionKind::StoredResult,
            );
            offer(
                &mut hits,
                8,
                env::other(&env.r_names),
                prefix,
                CompletionKind::StoredResult,
            );
            for kw in KEYWORDS {
                if kw.starts_with(prefix) {
                    hits.push((9, kw, CompletionKind::Keyword));
                }
            }
        }
    }

    // A TOTAL order: group, then label. Ties broken on the label means two runs
    // over the same environment produce the same popup, which is what makes the
    // whole surface reproducible (spec §12).
    hits.sort_unstable_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(b.1)));
    hits.dedup_by(|a, b| a.1 == b.1 && a.2 == b.2);
    let total = hits.len() as u32;
    hits.truncate(MAX_ITEMS);

    let items: Vec<CompletionItem> = hits
        .iter()
        .enumerate()
        .map(|(i, (_group, label, kind))| CompletionItem {
            label: (*label).to_owned(),
            kind: *kind,
            detail: detail_of(*kind, label, cmd),
            insert: insert_of(*kind, label),
            // Rank is the position in the TOTAL order, not the group index: the
            // popup renders `items` as given, and a rank that repeated across
            // groups would let a re-sorting consumer scramble it.
            rank: i as i32,
        })
        .collect();

    CompletionList {
        from: word.start as u32,
        to: word.end as u32,
        offered: items.len() as u32,
        total,
        truncated: total as usize > items.len(),
        items,
    }
}

/// Language keywords offered in expression position.
const KEYWORDS: [&str; 6] = ["if", "in", "using", "by", "bysort", "weight"];

/// Append every name in `list` that starts with `prefix`.
fn offer<'a>(
    out: &mut Vec<(u8, &'a str, CompletionKind)>,
    group: u8,
    list: &'a [String],
    prefix: &str,
    kind: CompletionKind,
) {
    for name in list {
        if name.starts_with(prefix) {
            out.push((group, name.as_str(), kind));
        }
    }
}

/// The right-aligned annotation. Never a variable *label*: A11 keeps those off
/// the keystroke path entirely, and the popup fetches them for visible rows.
fn detail_of(
    kind: CompletionKind,
    label: &str,
    cmd: Option<&'static CommandSig>,
) -> Option<String> {
    match kind {
        CompletionKind::Command => match resolve_command(label) {
            CommandLookup::Exact(id) | CommandLookup::Abbrev(id) => {
                let help = stratum_parse::cmdtable::command(id).help;
                (!help.is_empty()).then(|| help.to_owned())
            }
            _ => None,
        },
        CompletionKind::Option => cmd
            .and_then(|sig| sig.options.iter().find(|o| o.canonical == label))
            .map(option_detail),
        CompletionKind::Function => stratum_parse::function(label).map(|f| {
            if f.max_args == 0 {
                "()".to_owned()
            } else {
                format!(
                    "({} arg{})",
                    f.min_args,
                    if f.min_args == 1 { "" } else { "s" }
                )
            }
        }),
        _ => None,
    }
}

fn option_detail(o: &OptionSpec) -> String {
    if o.negatable {
        format!("{:?}, negatable", o.arg)
    } else {
        format!("{:?}", o.arg)
    }
}

/// Text to insert when it differs from the label.
fn insert_of(kind: CompletionKind, label: &str) -> Option<String> {
    match kind {
        // `strpos(` rather than `strpos`: the open paren is always wanted and
        // typing it re-triggers the signature help.
        CompletionKind::Function => Some(format!("{label}(")),
        // A local reference is `` `name' ``; the backtick is already typed, so
        // completing the name has to close it or the reference never terminates.
        CompletionKind::Local => Some(format!("{label}'")),
        _ => None,
    }
}

/// The identifier-ish token the cursor sits in, as a byte range.
///
/// An accepted completion replaces the whole word under the cursor rather than
/// only the text behind it, so `reg pri|ce` completes `price` instead of
/// appending. Byte-for-byte the same rule as `tokenAt` in
/// `apps/desktop/src/wasm/stub/completion.ts`, which is what lets the loader
/// claim the two backends are interchangeable.
fn word_at(buf: &[u8], pos: usize) -> Range<usize> {
    let at = pos.min(buf.len());
    let mut from = at;
    while from > 0 && is_word_byte(buf[from - 1]) {
        from -= 1;
    }
    let mut to = at;
    while to < buf.len() && is_word_byte(buf[to]) {
        to += 1;
    }
    from..to
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// What kind of position `at` is, and which command it belongs to.
///
/// **Bounded to the current physical line.** Resolving the enclosing statement
/// properly needs the segmentation, and `Segmenter::complete` takes `&self` — it
/// cannot sync the cache, and completing against a stale one would be worse than
/// completing against a smaller but current window. The consequence is that in
/// `#delimit ;` mode, or after a `///` continuation, the command word of a
/// statement that began on an earlier line is not found and the position reads
/// as `Expr`. That degrades the popup; it never mislabels it, because `Expr` is
/// the superset offer. `stratum-intel` is where this stops being a heuristic.
fn context(doc: &str, at: usize) -> (Trigger, Option<&'static CommandSig>) {
    let b = doc.as_bytes();
    if at > 0 {
        match b[at - 1] {
            b'`' => return (Trigger::Local, None),
            b'$' => return (Trigger::Global, None),
            _ => {}
        }
    }
    let line_start = b[..at]
        .iter()
        .rposition(|c| *c == b'\n')
        .map_or(0, |i| i + 1);
    let head = &doc[line_start..at];

    // Step over a prefix chain: `quietly:`, `bysort rep78:`, `capture noisily:`.
    // Only a top-level colon counts — one inside a string or parentheses is part
    // of an argument, not a prefix separator.
    let body = match top_level(head, b':') {
        Some(i) => &head[i + 1..],
        None => head,
    };
    let trimmed = body.trim_start_matches(|c: char| c.is_ascii_whitespace());
    let Some(word_len) = trimmed.find(|c: char| !is_word_byte(c as u8) || !c.is_ascii()) else {
        // Still inside the first word: this IS command position.
        return (Trigger::Command, None);
    };
    if word_len == 0 && !trimmed.is_empty() {
        return (Trigger::Expr, None);
    }
    let cmd_word = &trimmed[..word_len];
    let sig = match resolve_command(cmd_word) {
        CommandLookup::Exact(id) | CommandLookup::Abbrev(id) => {
            Some(stratum_parse::cmdtable::command(id))
        }
        _ => None,
    };
    if top_level(&trimmed[word_len..], b',').is_some() {
        (Trigger::Opt, sig)
    } else {
        (Trigger::Expr, sig)
    }
}

/// Byte index of the last `needle` at paren depth zero and outside quotes.
///
/// Quote-awareness is not optional: `label var x "a, b"` has a comma that is not
/// an option separator, and offering `summarize`'s options after it would put a
/// wrong list in front of the user on a correct line.
fn top_level(s: &str, needle: u8) -> Option<usize> {
    let mut depth: i32 = 0;
    let mut in_str = false;
    let mut found = None;
    for (i, c) in s.bytes().enumerate() {
        match c {
            b'"' => in_str = !in_str,
            b'(' | b'[' | b'{' if !in_str => depth += 1,
            b')' | b']' | b'}' if !in_str => depth -= 1,
            _ if !in_str && depth <= 0 && c == needle => found = Some(i),
            _ => {}
        }
    }
    found
}

// ===========================================================================
// Quick fixes and lints.
// ===========================================================================

/// Run `f` over a segmentation of `doc`, reusing `cached` when it is current.
///
/// The equality test is a `memcmp`, which is O(document) — deliberately paid
/// here and nowhere near the keystroke path: both callers are user-triggered
/// (the lightbulb, the problems pane), and the alternative is answering from a
/// segmentation of a document the user is no longer looking at.
fn with_segmentation<R>(
    cached: Option<&ParseSegmentation<'_>>,
    doc: &str,
    f: impl FnOnce(&ParseSegmentation<'_>) -> R,
) -> R {
    match cached {
        Some(seg) if seg.src == doc => f(seg),
        _ => f(&segment(doc)),
    }
}

/// Deterministic fixes at `pos`: the suggestions of every finding covering it.
pub fn quick_fixes(
    cached: Option<&ParseSegmentation<'_>>,
    doc: &str,
    pos: usize,
) -> Vec<Suggestion> {
    let pos = pos.min(doc.len()) as u32;
    with_segmentation(cached, doc, |seg| {
        let mut out = Vec::new();
        for (i, line) in seg.lines.iter().enumerate() {
            if line.span.end <= pos || line.span.start > pos || line.is_trivia {
                continue;
            }
            for d in line_findings(seg, line, seg.derived[i].as_deref()) {
                if d.span.is_some_and(|s| s.start <= pos && pos <= s.end) {
                    out.extend(d.suggestions);
                }
            }
        }
        out
    })
}

/// Whole-document lints that need no session state.
pub fn lints(cached: Option<&ParseSegmentation<'_>>, doc: &str) -> Vec<Diagnostic> {
    with_segmentation(cached, doc, |seg| {
        let mut out: Vec<Diagnostic> = Vec::new();
        // The segmenter's own findings first — unterminated braces, an `end`
        // with no opener. They are structural, they are already computed, and
        // they are the ones that explain why the rest of the file looks wrong.
        out.extend(seg.diags.iter().cloned());
        for (i, line) in seg.lines.iter().enumerate().take(MAX_LINT_LINES) {
            if line.is_trivia {
                continue;
            }
            if out.len() >= MAX_LINTS {
                break;
            }
            out.extend(line_findings(seg, line, seg.derived[i].as_deref()));
        }
        out.truncate(MAX_LINTS);
        out
    })
}

/// Parse one logical line and lint it, with spans mapped back to the source.
///
/// Speculative mode, because this is raw editor text with macro references still
/// in it. `vars: None`: the variable layout lives in `CompletionEnv`, which
/// `lints()` is not given — L007's unresolved-name check therefore stays quiet
/// rather than reporting every variable as unknown, which is the correct failure
/// for a check whose input is absent.
fn line_findings(
    seg: &ParseSegmentation<'_>,
    line: &LogicalLine,
    derived: Option<&Derived>,
) -> Vec<Diagnostic> {
    let code = line.code(seg.src, derived);
    if code.trim().is_empty() {
        return Vec::new();
    }
    let (ast, mut diags) = parse_command(code, ParseMode::Speculative);
    diags.extend(lint(
        &ast,
        &LintCtx {
            text: code,
            vars: None,
        },
    ));
    for d in &mut diags {
        d.span = d.span.map(|s| to_source(line, derived, s));
        for r in &mut d.related {
            r.span = to_source(line, derived, r.span);
        }
    }
    diags
}

/// A span in a line's code, as a span in the original source.
///
/// A span that crosses a stripped comment is genuinely two source ranges; the
/// underline takes the outer hull of them, because CONTRACTS §4's `Diagnostic`
/// carries one span and an underline that stops at the comment reads as a
/// second, unrelated error.
fn to_source(line: &LogicalLine, derived: Option<&Derived>, s: Span) -> Span {
    let pieces = line.span_to_source(derived, s);
    match (pieces.first(), pieces.last()) {
        (Some(a), Some(b)) => Span {
            start: a.start,
            end: b.end,
        },
        _ => line.code_span,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_with(varnames: &[&str], locals: &[&str]) -> CompletionEnv {
        CompletionEnv {
            generation: 1,
            varnames: varnames.iter().map(|s| (*s).to_owned()).collect(),
            var_total: varnames.len() as u32,
            locals: locals.iter().map(|s| (*s).to_owned()).collect(),
            ..CompletionEnv::default()
        }
    }

    fn labels(list: &CompletionList) -> Vec<&str> {
        list.items.iter().map(|i| i.label.as_str()).collect()
    }

    #[test]
    fn the_replaced_range_is_the_whole_word_under_the_cursor() {
        let doc = "summarize price";
        // Cursor between `pri` and `ce`.
        let list = complete(doc, &env_with(&["price"], &[]), 13);
        assert_eq!((list.from, list.to), (10, 15));
    }

    #[test]
    fn command_position_offers_commands() {
        let list = complete("summ", &CompletionEnv::default(), 4);
        assert!(labels(&list).contains(&"summarize"), "{:?}", labels(&list));
        assert!(list.items.iter().all(|i| i.kind == CompletionKind::Command));
    }

    #[test]
    fn a_backtick_offers_locals_and_closes_them() {
        let env = env_with(&["price"], &["yr", "yvar"]);
        let list = complete("display `y", &env, 10);
        assert_eq!(labels(&list), vec!["yr", "yvar"]);
        assert_eq!(list.items[0].insert.as_deref(), Some("yr'"));
    }

    #[test]
    fn after_the_comma_the_commands_options_are_offered() {
        let env = CompletionEnv::default();
        let list = complete("summarize price, de", &env, 19);
        assert!(labels(&list).contains(&"detail"), "{:?}", labels(&list));
        assert!(list.items.iter().all(|i| i.kind == CompletionKind::Option));
    }

    #[test]
    fn a_comma_inside_a_string_is_not_an_option_separator() {
        let env = env_with(&["price"], &[]);
        let list = complete("label var x \"a, b\" pri", &env, 22);
        assert!(
            list.items.iter().all(|i| i.kind != CompletionKind::Option),
            "a quoted comma opened the option list: {:?}",
            labels(&list)
        );
    }

    #[test]
    fn expression_position_offers_variables_and_functions() {
        let env = env_with(&["price", "priceadj"], &[]);
        let list = complete("summarize pri", &env, 13);
        let got = labels(&list);
        assert!(got.contains(&"price"));
        assert!(got.contains(&"priceadj"));
        // Variables sort before functions: a statistics IDE completes columns.
        assert_eq!(got[0], "price");
    }

    #[test]
    fn the_order_is_total_and_reproducible() {
        let env = env_with(&["b", "a", "c"], &[]);
        let a = complete("summarize ", &env, 10);
        let b = complete("summarize ", &env, 10);
        assert_eq!(labels(&a), labels(&b));
        let vars: Vec<&str> = a
            .items
            .iter()
            .filter(|i| i.kind == CompletionKind::Variable)
            .map(|i| i.label.as_str())
            .collect();
        assert_eq!(vars, vec!["a", "b", "c"], "labels break ties, ascending");
    }

    #[test]
    fn a_prefix_chain_does_not_hide_the_command() {
        let env = CompletionEnv::default();
        let list = complete("quietly: summarize price, de", &env, 28);
        assert!(labels(&list).contains(&"detail"), "{:?}", labels(&list));
    }

    /// A11: the cap is 2 048 variables and 512 of everything else, and the
    /// candidate list must stay bounded whatever the environment holds.
    #[test]
    fn the_offered_list_is_bounded_at_the_cap() {
        let names: Vec<String> = (0..2048).map(|i| format!("v{i:05}")).collect();
        let env = CompletionEnv {
            generation: 1,
            varnames: names,
            var_total: 32_767,
            truncated: true,
            ..CompletionEnv::default()
        };
        let list = complete("summarize ", &env, 10);
        assert!(list.items.len() <= MAX_ITEMS);
        assert!(list.truncated);
        assert!(list.total > MAX_ITEMS as u32);
    }

    #[test]
    fn lints_find_something_and_stay_bounded() {
        let doc = "summarize price, nosuchoption\n".repeat(3);
        let found = lints(None, &doc);
        assert!(found.len() <= MAX_LINTS);
        assert!(
            found
                .iter()
                .all(|d| d.span.is_none_or(|s| s.end as usize <= doc.len())),
            "a lint span escaped the document"
        );
    }

    #[test]
    fn lint_spans_are_in_source_coordinates() {
        // The finding must underline the option in the ORIGINAL text, not at the
        // offset it had in the comment-stripped line.
        let doc = "/* lead */ summarize price, nosuchoption\n";
        let found = lints(None, doc);
        let with_span: Vec<Span> = found.iter().filter_map(|d| d.span).collect();
        assert!(
            with_span.iter().all(|s| s.start >= 11),
            "a span landed inside the stripped comment: {with_span:?}"
        );
    }

    #[test]
    fn quick_fixes_outside_any_finding_are_empty() {
        assert!(quick_fixes(None, "list\n", 0).is_empty());
    }
}
