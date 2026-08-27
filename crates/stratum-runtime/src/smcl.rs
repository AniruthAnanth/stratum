//! The SMCL **parser** — ARCHITECTURE C22, CONTRACTS §5.2 (A12).
//!
//! This is the single place SMCL is turned into `Vec<StyledRun>`, and it exists
//! for exactly one class of input: **SMCL that user code emits.**
//! `display as result`, `.sthlp` files, `log using …, smcl` replay, an ado-file
//! drawing its own table. It does **not** wrap `stratum-stats`' output:
//! `classic_text(linesize)` returns `Vec<StyledRun>` directly, because given a
//! byte-exact 78-column `regress` table as a flat string nothing can recover
//! which spans were `{res}` and which were `{txt}`, and the Classic pane would
//! print every statistics table in one ink.
//!
//! The frontend never parses SMCL (`06` §16.2). The log-file writer, the CLI's
//! text mode, `log_copy` and the byte-exactness goldens all flatten runs with
//! `stratum_proto::styled::to_plain`, which is the only flattening function in
//! the workspace — so a change to styling can never move a golden byte.
//!
//! # Column tracking, and why `linesize` is a parameter
//!
//! `{col N}`, `{hline}`, `{center:…}` and `{right:…}` need to know where on the
//! line we are and how wide the line is. `set linesize` is rejected with `rc 10`
//! in v1 (ADR-016/A16) and `c(linesize)` is always [`LINESIZE`], so [`parse`]
//! hard-codes 80; [`parse_with`] takes the width anyway, because the alternative
//! is a constant baked through four layout branches that someone has to find
//! again when the setting is implemented.
//!
//! # Unknown directives are printed, not swallowed
//!
//! `{foo}` comes out as the literal seven characters. Stata errors on an unknown
//! directive; dropping it silently would make a user's own output quietly
//! disappear, which is the worse failure for a product whose thesis is fidelity.

use stratum_proto::{StyleId, StyledRun};

/// `c(linesize)`. Always 80 in v1 (ADR-016).
pub const LINESIZE: usize = 80;

/// What a `{help …}`-family directive points at.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LinkTarget {
    /// Which directive produced it.
    pub kind: LinkKind,
    /// The argument, verbatim: a help topic, a command line, a URL, a filename.
    pub arg: String,
}

/// The link directives `[P] smcl` defines that can appear in user output.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
#[allow(missing_docs)] // one word each, and the SMCL spelling is the doc.
pub enum LinkKind {
    Help,
    Stata,
    Browse,
    View,
    Net,
    Search,
    Var,
    NewVar,
    Manhelp,
}

/// A parsed SMCL fragment.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Smcl {
    /// The styled text. `StyleId::Link { target_index }` indexes `targets`.
    pub runs: Vec<StyledRun>,
    /// Link destinations, in first-appearance order.
    pub targets: Vec<LinkTarget>,
}

impl Smcl {
    /// The bytes, via the workspace's single flattening function.
    #[must_use]
    pub fn to_plain(&self) -> String {
        stratum_proto::styled::to_plain(&self.runs)
    }
}

/// Parse SMCL at the v1 line width.
#[must_use]
pub fn parse(src: &str) -> Smcl {
    parse_with(src, LINESIZE)
}

/// Parse SMCL at an explicit line width.
#[must_use]
pub fn parse_with(src: &str, linesize: usize) -> Smcl {
    let mut p = Parser {
        out: Vec::new(),
        targets: Vec::new(),
        pending: String::new(),
        pending_style: StyleId::Text,
        col: 0,
        linesize,
        depth: 0,
    };
    p.run(src, StyleId::Text);
    p.flush();
    Smcl {
        runs: p.out,
        targets: p.targets,
    }
}

/// Directives may nest (`{help x:{bf:go}}`); a hand-written `.sthlp` should not
/// be able to blow the stack.
const MAX_DEPTH: u32 = 32;

struct Parser {
    out: Vec<StyledRun>,
    targets: Vec<LinkTarget>,
    pending: String,
    pending_style: StyleId,
    col: usize,
    linesize: usize,
    depth: u32,
}

impl Parser {
    fn flush(&mut self) {
        if !self.pending.is_empty() {
            self.out.push(StyledRun {
                text: std::mem::take(&mut self.pending),
                style: self.pending_style,
            });
        }
    }

    fn push(&mut self, text: &str, style: StyleId) {
        if text.is_empty() {
            return;
        }
        if style != self.pending_style {
            self.flush();
            self.pending_style = style;
        }
        for ch in text.chars() {
            if ch == '\n' {
                self.col = 0;
            } else {
                self.col += 1;
            }
        }
        self.pending.push_str(text);
    }

    fn run(&mut self, src: &str, style: StyleId) {
        if self.depth > MAX_DEPTH {
            self.push(src, style);
            return;
        }
        let mut style = style;
        let bytes = src.as_bytes();
        let mut i = 0;
        let mut text_start = 0;
        while i < bytes.len() {
            if bytes[i] != b'{' {
                i += 1;
                continue;
            }
            let Some(end) = match_brace(src, i) else {
                // An unmatched `{` is literal text, not a parse error: user code
                // that printed a brace must see its brace.
                i += 1;
                continue;
            };
            self.push(&src[text_start..i], style);
            let inner = &src[i + 1..end];
            if let Some(next) = self.directive(inner, style) {
                style = next;
            }
            i = end + 1;
            text_start = i;
        }
        self.push(&src[text_start..], style);
    }

    /// Handle one `{…}`. Returns the style in force afterwards, when the
    /// directive is a bare style switch.
    fn directive(&mut self, inner: &str, style: StyleId) -> Option<StyleId> {
        if inner.starts_with('*') {
            return None; // `{* comment}` produces nothing.
        }
        let (head, body) = split_body(inner);
        let (verb, args) = match head.find(' ') {
            Some(k) => (&head[..k], head[k + 1..].trim()),
            None => (head, ""),
        };

        // Bare style switches: `{txt}`, `{res}`, … with no body.
        if body.is_none() && args.is_empty() {
            if let Some(s) = channel(verb) {
                return Some(under(style, s));
            }
        }
        // Body forms of the same directives: `{res:74}`.
        if let (Some(b), Some(s)) = (body, channel(verb)) {
            self.nested(b, under(style, s));
            return None;
        }

        match verb {
            "title" => {
                if let Some(b) = body {
                    self.nested(b, under(style, StyleId::Heading));
                }
            }
            "hline" => {
                let n = args
                    .parse::<usize>()
                    .unwrap_or_else(|_| self.linesize.saturating_sub(self.col));
                self.push(&"-".repeat(n), under(style, StyleId::Rule));
            }
            "dup" => {
                if let (Ok(n), Some(b)) = (args.parse::<usize>(), body) {
                    // Cheap ceiling: `{dup}` is a convenience for rules and
                    // padding, and a five-digit count is a typo, not a table.
                    for _ in 0..n.min(4096) {
                        self.nested(b, style);
                    }
                }
            }
            "col" => {
                if let Ok(n) = args.parse::<usize>() {
                    let target = n.saturating_sub(1);
                    if target > self.col {
                        self.push(&" ".repeat(target - self.col), style);
                    }
                }
            }
            "center" | "centre" | "right" => {
                let b = body?;
                let inner = parse_with(b, self.linesize);
                let text = inner.to_plain();
                let width = text.chars().count();
                let pad = if verb == "right" {
                    self.linesize.saturating_sub(width)
                } else {
                    self.linesize.saturating_sub(width) / 2
                };
                self.push(&" ".repeat(pad), style);
                self.nested(b, style);
            }
            // `{opt d:etail}`, `{cmdab:reg:ress}`, `{opth save:filename}`. The
            // colon marks the minimum abbreviation; it does not delimit an
            // argument, and EVERY segment is displayed. Treating it as a
            // `verb:body` split drops the `d` from `detail` — a character of
            // the user's own help text, silently gone.
            "opt" | "opth" | "opdt" | "cmdab" => {
                let s = under(style, StyleId::Input);
                if !args.is_empty() {
                    self.push(args, s);
                }
                if let Some(b) = body {
                    for seg in top_level_segments(b) {
                        self.nested(seg, s);
                    }
                }
            }
            // A `.sthlp` line continuation. Produces nothing, and must not
            // print itself the way an unknown directive does.
            "..." => {}
            "c" => self.push(char_directive(args), style),
            "break" => self.push("\n", style),
            "help" | "stata" | "browse" | "view" | "net" | "search" | "newvar" | "var"
            | "manhelp" => {
                let kind = match verb {
                    "help" => LinkKind::Help,
                    "stata" => LinkKind::Stata,
                    "browse" => LinkKind::Browse,
                    "view" => LinkKind::View,
                    "net" => LinkKind::Net,
                    "search" => LinkKind::Search,
                    "newvar" => LinkKind::NewVar,
                    "manhelp" => LinkKind::Manhelp,
                    _ => LinkKind::Var,
                };
                let arg = args.trim_matches('"').to_owned();
                let target_index = self.intern(kind, arg.clone());
                let label = StyleId::Link { target_index };
                match body {
                    Some(b) => self.nested(b, label),
                    // `{help summarize}` prints the topic itself.
                    None => self.push(&arg, label),
                }
            }
            // Layout directives with no effect on a flat run stream. They matter
            // to the Viewer's paragraph engine, which is `06`'s, and emitting a
            // newline for them here would corrupt every `.sthlp` line width.
            "p" | "p_end" | "pstd" | "phang" | "pmore" | "psee" | "phang2" | "p2colset"
            | "p2col" | "p2line" | "synoptset" | "synopt" | "syntab" | "marker" | "smcl" | "ul"
            | "s6hlp" | "asis" | "sf" | "space" => {
                if verb == "space" {
                    if let Ok(n) = args.parse::<usize>() {
                        self.push(&" ".repeat(n), style);
                    }
                } else if let Some(b) = body {
                    self.nested(b, style);
                }
            }
            _ => {
                // Unknown: print it back, braces and all.
                self.push(&format!("{{{inner}}}"), style);
            }
        }
        None
    }

    fn nested(&mut self, body: &str, style: StyleId) {
        self.depth += 1;
        self.run(body, style);
        self.depth -= 1;
    }

    fn intern(&mut self, kind: LinkKind, arg: String) -> u32 {
        if let Some(i) = self
            .targets
            .iter()
            .position(|t| t.kind == kind && t.arg == arg)
        {
            return i as u32;
        }
        self.targets.push(LinkTarget { kind, arg });
        (self.targets.len() - 1) as u32
    }
}

/// The style a nested directive actually takes.
///
/// A run carries exactly one `StyleId`, and `StyleId::Link { target_index }` is
/// the only one that carries a destination. `{help summarize:{bf:summarize}}` —
/// which is how every `.sthlp` in Stata's own library writes a bold link — would
/// otherwise emit a `Hilite` run and leave `Smcl::targets` holding a destination
/// no run points at, so the Viewer would render a `{help}` it cannot make
/// clickable. Inside a link body a style switch decorates; it does not replace.
#[inline]
fn under(current: StyleId, want: StyleId) -> StyleId {
    match current {
        StyleId::Link { .. } => current,
        _ => want,
    }
}

/// The bare channel directives, and the body forms that share their spelling.
fn channel(verb: &str) -> Option<StyleId> {
    Some(match verb {
        "txt" | "text" | "reset" | "sf" => StyleId::Text,
        "res" | "result" => StyleId::Result,
        "err" | "error" => StyleId::Error,
        "inp" | "input" | "cmd" | "com" | "bind" => StyleId::Input,
        "hi" | "hilite" | "bf" | "it" | "ualt" => StyleId::Hilite,
        "hline_char" => StyleId::Rule,
        _ => return None,
    })
}

/// `{c …}` — `[P] smcl`'s character directive.
///
/// The box-drawing names render as line-drawing glyphs in the Results window and
/// as ASCII in a text log. Every table in `tests/golden/stata18/*.log` is pure
/// ASCII (`-`, `|`, `+`), so the text mapping is what we produce; our own tables
/// never come through here anyway, because `stratum-stats` emits runs directly.
fn char_directive(arg: &str) -> &'static str {
    match arg {
        "-(" => "{",
        ")-" => "}",
        "|" => "|",
        "-" => "-",
        "+" => "+",
        "TLC" | "TRC" | "BLC" | "BRC" | "TT" | "BT" | "LT" | "RT" | "PLUS" => "+",
        "VS" | "vs" => "|",
        "S" => "\u{a7}",
        "247" | "S|" => "\u{a7}",
        "92" | "bs" => "\\",
        "174" | "rgt" => "\u{ae}",
        "169" | "cpr" => "\u{a9}",
        "0" => "\0",
        _ => "",
    }
}

/// Split on every top-level `:`, so `{cmdab:reg:ress}` displays `regress`
/// rather than `reg:ress`.
fn top_level_segments(body: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0u32;
    let mut start = 0usize;
    for (i, ch) in body.char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            ':' if depth == 0 => {
                out.push(&body[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&body[start..]);
    out
}

/// Split `verb args:body` at the first top-level `:`.
fn split_body(inner: &str) -> (&str, Option<&str>) {
    let mut depth = 0u32;
    for (i, ch) in inner.char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            ':' if depth == 0 => return (&inner[..i], Some(&inner[i + 1..])),
            _ => {}
        }
    }
    (inner, None)
}

/// The index of the `}` closing the `{` at `open`, counting nesting.
fn match_brace(src: &str, open: usize) -> Option<usize> {
    let mut depth = 0u32;
    for (i, ch) in src[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(open + i);
                }
            }
            _ => {}
        }
    }
    None
}

/// The channel styles that have a lossless SMCL spelling.
#[must_use]
pub fn channel_directive(style: StyleId) -> Option<&'static str> {
    Some(match style {
        StyleId::Text => "txt",
        StyleId::Result => "res",
        StyleId::Error | StyleId::ErrorToken => "err",
        StyleId::Input => "inp",
        StyleId::Hilite => "hi",
        _ => return None,
    })
}

/// Render runs back to SMCL.
///
/// Lossless for the five channel styles [`channel_directive`] names, which is
/// what the log writer needs (`log using …, smcl`). `Heading`, `Rule`, `Comment`
/// and `Link` have no bare directive — they come from body forms — so they are
/// written on their nearest channel and are **not** claimed to round-trip.
/// `tests/smcl.rs` states the property over the lossless set exactly.
#[must_use]
pub fn to_smcl(runs: &[StyledRun]) -> String {
    let mut out = String::new();
    // The directive last WRITTEN, not the style it came from: a `Rule` run is
    // written on `{txt}`, and comparing styles would emit a second, redundant
    // `{txt}` for the plain run that follows it.
    let mut cur: Option<&'static str> = None;
    for r in runs {
        if r.text.is_empty() {
            continue;
        }
        let want = channel_directive(r.style).unwrap_or("txt");
        if cur != Some(want) {
            out.push('{');
            out.push_str(want);
            out.push('}');
            cur = Some(want);
        }
        for ch in r.text.chars() {
            match ch {
                '{' => out.push_str("{c -(}"),
                '}' => out.push_str("{c )-}"),
                _ => out.push(ch),
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(s: &str) -> String {
        parse(s).to_plain()
    }

    #[test]
    fn a_brace_that_opens_nothing_is_literal_text() {
        assert_eq!(plain("a { b"), "a { b");
        assert_eq!(plain("100% {of it"), "100% {of it");
    }

    #[test]
    fn an_unknown_directive_is_printed_rather_than_swallowed() {
        assert_eq!(plain("x{nosuchthing}y"), "x{nosuchthing}y");
    }

    #[test]
    fn braces_survive_the_character_directive() {
        assert_eq!(plain("{c -(}foreach{c )-}"), "{foreach}");
    }

    #[test]
    fn hline_fills_to_the_line_width_and_a_count_overrides_it() {
        assert_eq!(plain("{hline 5}"), "-----");
        assert_eq!(plain("{hline}").len(), LINESIZE);
        assert_eq!(plain("ab{hline}").len(), LINESIZE);
    }

    #[test]
    fn col_pads_to_a_one_based_column() {
        assert_eq!(plain("a{col 5}b"), "a   b");
        assert_eq!(plain("abcdef{col 3}b"), "abcdefb", "never moves backwards");
    }

    #[test]
    fn nested_directives_keep_the_outer_link() {
        let s = parse("{help summarize:{bf:summarize}}");
        assert_eq!(s.to_plain(), "summarize");
        assert_eq!(s.targets.len(), 1);
        assert_eq!(s.targets[0].kind, LinkKind::Help);
        assert_eq!(s.targets[0].arg, "summarize");
        // Every target must be reachable from a run, or the Viewer has a
        // destination it cannot attach to anything.
        assert_eq!(
            s.runs,
            vec![StyledRun {
                text: "summarize".to_owned(),
                style: StyleId::Link { target_index: 0 },
            }]
        );
    }

    #[test]
    fn depth_is_bounded() {
        let deep = format!("{}x{}", "{bf:".repeat(200), "}".repeat(200));
        assert!(plain(&deep).contains('x'));
    }
}
