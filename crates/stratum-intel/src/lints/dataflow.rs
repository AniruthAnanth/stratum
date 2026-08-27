//! The one document model the lints and the reproducibility checks share.
//!
//! Design 03 §10 describes the repro audit as "a static abstract interpretation
//! over blocks in document order", and design 07 §6.3 describes the lints as
//! "deterministic, from the AST + dataflow". Both walk the same statements and
//! ask overlapping questions, so both walk them **once**: building this model
//! parses every logical line exactly one time, and thirty-eight checks then read
//! it instead of thirty-eight independent passes over the file.
//!
//! Everything here is speculative-mode parsing. Macro values are not known at
//! check time, so a `` `cmd' `` in the command position produces a
//! [`Stmt::head`] of `` `cmd' `` and a `Taint`-shaped downgrade in the checks
//! that care, rather than a guess.

use rustc_hash::FxHashSet;
use stratum_parse::ast::{
    BlockCommand, Command, CommandAst, Expr, ForeachSource, OptionArg, OptionItem, Prefix,
    StoredClass, SysVar, VarItemKind, VarList, VarPattern,
};
use stratum_parse::ParseMode;
use stratum_proto::diagnostic::Diagnostic;
use stratum_proto::Span;

use crate::ParseIndex;

/// One statement, parsed, with the facts every check reads off it.
#[derive(Clone, Debug)]
pub struct Stmt<'a> {
    /// Index into the segmentation's logical-line vector.
    pub ix: usize,
    /// Comment-stripped, continuation-spliced code.
    pub code: &'a str,
    /// Extent of `code` in the source buffer.
    pub span: Span,
    /// 0-based physical line of the first code byte.
    pub line: u32,
    /// The parse.
    pub ast: CommandAst,
    /// The command word as typed, prefixes stripped. `` `x' `` for a macro head.
    pub head: String,
    /// The canonical command name, when the word resolved against the table.
    pub canonical: Option<&'static str>,
    /// Prefix words, outermost first: `by`, `capture`, `quietly`, `noisily`,
    /// `version`, `frame`, or a generic prefix command's own name.
    pub prefixes: Vec<String>,
    /// Diagnostics the parser produced for this statement, spans already mapped
    /// back into source coordinates.
    pub parse_diags: Vec<Diagnostic>,
    /// Brace nesting depth at the start of this statement. `0` is file scope.
    pub depth: u32,
}

impl Stmt<'_> {
    /// True when this statement carries `prefix` anywhere in its chain.
    #[must_use]
    pub fn has_prefix(&self, prefix: &str) -> bool {
        self.prefixes.iter().any(|p| p == prefix)
    }

    /// The canonical name if it resolved, else the word as typed. What a lint
    /// message should print.
    #[must_use]
    pub fn name(&self) -> &str {
        self.canonical.unwrap_or(&self.head)
    }

    /// Whether an option was given, by canonical name.
    #[must_use]
    pub fn has_option(&self, canonical: &str) -> bool {
        self.options().any(|o| {
            o.canonical == Some(canonical)
                || (o.canonical.is_none() && canonical.starts_with(o.name.as_str()))
        })
    }

    /// The argument of an option, by canonical name.
    #[must_use]
    pub fn option_text(&self, canonical: &str) -> Option<String> {
        self.options()
            .find(|o| o.canonical == Some(canonical) || o.name == canonical)
            .and_then(|o| o.arg.as_ref().map(raw_option_text))
    }

    /// Every option on the command, in the order typed.
    pub fn options(&self) -> impl Iterator<Item = &OptionItem> {
        match &self.ast.cmd {
            Command::Known(k) => k.slots.options.items.iter(),
            _ => [].iter(),
        }
    }

    /// The `using` filename, verbatim.
    #[must_use]
    pub fn using(&self) -> Option<&str> {
        match &self.ast.cmd {
            Command::Known(k) => k.slots.using.as_ref().map(|f| f.raw.as_str()),
            _ => None,
        }
    }

    /// The command's varlist slot.
    #[must_use]
    pub fn varlist(&self) -> Option<&VarList> {
        match &self.ast.cmd {
            Command::Known(k) => k.slots.varlist.as_ref(),
            _ => None,
        }
    }

    /// The unclassified positional tail (`label define lbl 1 "a"`, `use file`).
    #[must_use]
    pub fn rest(&self) -> Option<&str> {
        match &self.ast.cmd {
            Command::Known(k) => k.slots.rest.as_ref().map(|r| r.text.as_str()),
            Command::Unknown { rest, .. } => Some(rest.text.as_str()),
            _ => None,
        }
    }

    /// Every expression this statement evaluates: `= exp`, `if exp`, weights,
    /// and a `while` condition.
    pub fn exprs(&self) -> Vec<&Expr> {
        let mut out = Vec::new();
        match &self.ast.cmd {
            Command::Known(k) => {
                out.extend(k.slots.assign.iter());
                out.extend(k.slots.if_.iter());
                out.extend(k.slots.weight.as_ref().map(|w| &w.expr));
            }
            Command::Block(b) => {
                if let BlockCommand::While { cond, .. } = b.as_ref() {
                    out.push(cond);
                }
                if let BlockCommand::IfElse { arms } = b.as_ref() {
                    out.extend(arms.iter().filter_map(|(c, _)| c.as_ref()));
                }
            }
            _ => {}
        }
        out
    }

    /// Map an offset inside `code` back to a source offset.
    #[must_use]
    pub fn to_source(&self, idx: &ParseIndex<'_>, s: Span) -> Span {
        idx.span_to_source(self.ix, s)
    }
}

fn raw_option_text(arg: &OptionArg) -> String {
    match arg {
        OptionArg::Raw(r) => r.text.clone(),
        OptionArg::Str(s) => s.clone(),
        OptionArg::Int(n) => n.to_string(),
        OptionArg::Real(n) => stratum_core::fmt::fmt_macro(*n),
        OptionArg::VarList(v) => varlist_names(v).join(" "),
        _ => String::new(),
    }
}

/// The whole file, parsed once.
#[derive(Clone, Debug)]
pub struct Doc<'a> {
    /// Statements in document order.
    pub stmts: Vec<Stmt<'a>>,
    /// `*! nolint(CODE)` markers: the code and the extent of the marker.
    pub suppressions: Vec<(String, Span)>,
}

impl<'a> Doc<'a> {
    /// Parse every logical line of `idx`.
    #[must_use]
    pub fn build(idx: &'a ParseIndex<'a>) -> Doc<'a> {
        let seg = idx.segmentation();
        let mut stmts = Vec::with_capacity(seg.lines.len());
        let mut depth: i64 = 0;
        for (i, line, code) in idx.statements() {
            let entry_depth = depth.max(0) as u32;
            depth += i64::from(line.brace_delta);
            let (ast, diags) = stratum_parse::parse_command(code, ParseMode::Speculative);
            let (head, canonical) = head_of(&ast, code);
            let prefixes = ast.prefixes.iter().map(prefix_word).collect();
            let parse_diags = diags
                .into_iter()
                .map(|mut d| {
                    d.span = d.span.map(|s| idx.span_to_source(i, s));
                    d
                })
                .collect();
            stmts.push(Stmt {
                ix: i,
                code,
                span: line.code_span,
                line: line.code_first_line,
                ast,
                head,
                canonical,
                prefixes,
                parse_diags,
                depth: entry_depth,
            });
        }
        Doc {
            stmts,
            suppressions: suppressions(idx),
        }
    }

    /// Codes suppressed at physical line `line`: a `*! nolint(...)` on that line
    /// or on the line immediately above it (design 03 §10).
    #[must_use]
    pub fn suppresses(&self, idx: &ParseIndex<'_>, line: u32, code: &str) -> bool {
        self.suppressions.iter().any(|(c, span)| {
            if c != code {
                return false;
            }
            let l = idx.segmentation().line_index.line_of(span.start);
            l == line || l + 1 == line
        })
    }
}

/// Scan comment extents for `*! nolint(CODE[, CODE]*)`.
///
/// Only comment extents, so a `*! nolint(R001)` inside a string literal — which
/// is data, not a suppression — cannot silence a finding.
fn suppressions(idx: &ParseIndex<'_>) -> Vec<(String, Span)> {
    const MARK: &str = "nolint(";
    let src = idx.source();
    let mut out = Vec::new();
    for span in idx.comment_spans() {
        let Some(text) = src.get(span.start as usize..span.end as usize) else {
            continue;
        };
        let mut from = 0usize;
        while let Some(rel) = text.get(from..).and_then(|t| t.find(MARK)) {
            let open = from + rel + MARK.len();
            let Some(close) = text.get(open..).and_then(|t| t.find(')')) else {
                break;
            };
            let body = text.get(open..open + close).unwrap_or("");
            for code in body.split(',') {
                let code = code.trim();
                if !code.is_empty() {
                    out.push((code.to_ascii_uppercase(), span));
                }
            }
            from = open + close;
        }
    }
    out
}

fn prefix_word(p: &Prefix) -> String {
    match p {
        Prefix::By(b) => if b.sort { "bysort" } else { "by" }.to_owned(),
        Prefix::Quietly { .. } => "quietly".to_owned(),
        Prefix::Noisily { .. } => "noisily".to_owned(),
        Prefix::Capture { .. } => "capture".to_owned(),
        Prefix::Version { .. } => "version".to_owned(),
        Prefix::Frame { .. } => "frame".to_owned(),
        Prefix::Generic { name, .. } => name.clone(),
    }
}

/// `(word as typed, canonical name)`.
fn head_of(ast: &CommandAst, code: &str) -> (String, Option<&'static str>) {
    match &ast.cmd {
        Command::Known(k) => {
            let sig = stratum_parse::table().get(k.id);
            let typed = code
                .get(k.name_span.start as usize..k.name_span.end as usize)
                .unwrap_or(sig.canonical);
            (typed.to_owned(), Some(sig.canonical))
        }
        Command::Unknown { name, .. } => (name.clone(), None),
        Command::Directive(_) => ("#delimit".to_owned(), Some("#delimit")),
        Command::Block(b) => {
            let w = match b.as_ref() {
                BlockCommand::Foreach { .. } => "foreach",
                BlockCommand::Forvalues { .. } => "forvalues",
                BlockCommand::While { .. } => "while",
                BlockCommand::IfElse { .. } => "if",
                BlockCommand::Program { .. } => "program",
                BlockCommand::Input { .. } => "input",
                BlockCommand::Mata { .. } => "mata",
                BlockCommand::Python { .. } => "python",
                BlockCommand::Capture { .. } => "capture",
                BlockCommand::Quietly { .. } => "quietly",
                BlockCommand::Noisily { .. } => "noisily",
                BlockCommand::Anonymous { .. } => "{",
            };
            (w.to_owned(), Some(w))
        }
    }
}

// ---------------------------------------------------------------------------
// Name extraction
// ---------------------------------------------------------------------------

/// The bare names a varlist mentions. Globs, ranges and `_all` contribute
/// nothing: they name a *set*, and a check that treated `pri*` as the variable
/// `pri*` would report a finding about a variable that does not exist.
#[must_use]
pub fn varlist_names(v: &VarList) -> Vec<String> {
    let mut out = Vec::new();
    for item in &v.items {
        match &item.kind {
            VarItemKind::Single(a) => push_pattern(&a.base, &mut out),
            VarItemKind::Interact { atoms, .. } => {
                for a in atoms {
                    push_pattern(&a.base, &mut out);
                }
            }
        }
    }
    out
}

fn push_pattern(p: &VarPattern, out: &mut Vec<String>) {
    match p {
        VarPattern::Name(n) => out.push(n.clone()),
        VarPattern::Labeled { name, .. } => out.push(name.clone()),
        VarPattern::Typed { inner, .. } => {
            for q in inner {
                push_pattern(q, out);
            }
        }
        _ => {}
    }
}

/// Every bare name an expression reads.
#[must_use]
pub fn expr_names(e: &Expr) -> Vec<String> {
    let mut out = Vec::new();
    e.walk(&mut |node| {
        if let Expr::Name(n, _) = node {
            out.push(n.clone());
        }
        if let Expr::Term(atom, _) = node {
            push_pattern(&atom.base, &mut out);
        }
    });
    out
}

/// Every function name an expression calls.
#[must_use]
pub fn expr_calls(e: &Expr) -> Vec<String> {
    let mut out = Vec::new();
    e.walk(&mut |node| {
        if let Expr::Call { name, .. } = node {
            out.push(name.clone());
        }
    });
    out
}

/// Every `c(...)` key an expression reads, lower-cased.
#[must_use]
pub fn expr_c_keys(e: &Expr) -> Vec<String> {
    let mut out = Vec::new();
    e.walk(&mut |node| {
        if let Expr::Stored {
            class: StoredClass::C,
            key,
            ..
        } = node
        {
            if let Expr::Name(n, _) = key.as_ref() {
                out.push(n.to_ascii_lowercase());
            }
            if let Expr::Str(s, _) = key.as_ref() {
                out.push(s.to_ascii_lowercase());
            }
        }
    });
    out
}

/// True when the expression reads `_rc`.
#[must_use]
pub fn reads_rc(e: &Expr) -> bool {
    let mut found = false;
    e.walk(&mut |node| {
        if matches!(node, Expr::Sys(SysVar::Rc, _)) {
            found = true;
        }
    });
    found
}

/// `(created, used)` for one logical line's code.
///
/// This is `ProgramIndex::creates_and_uses` at statement granularity. "Created"
/// is an AST fact — the target of a `generate`/`egen`, the new name of a
/// `rename`, the `gen()` target of `encode`/`decode`/`egen` — and never a guess,
/// which is what design 07 §0.1 means by "`Created by` is an AST fact".
pub fn creates_and_uses_line(code: &str, created: &mut Vec<String>, used: &mut Vec<String>) {
    let (ast, _) = stratum_parse::parse_command(code, ParseMode::Speculative);
    let Command::Known(k) = &ast.cmd else {
        return;
    };
    let sig = stratum_parse::table().get(k.id);
    let targets = matches!(
        sig.canonical,
        "generate" | "egen" | "gen" | "sysuse" | "use" | "decode" | "encode"
    );
    if let Some(v) = &k.slots.varlist {
        let names = varlist_names(v);
        if targets {
            created.extend(names);
        } else {
            used.extend(names);
        }
    }
    if let Some(t) = k
        .slots
        .options
        .items
        .iter()
        .find(|o| o.canonical == Some("generate") || o.name == "gen")
        .and_then(|o| o.arg.as_ref())
    {
        created.push(raw_option_text(t));
    }
    for e in k.slots.assign.iter().chain(k.slots.if_.iter()) {
        used.extend(expr_names(e));
    }
    if sig.canonical == "rename" {
        // `rename old new`: the first name is used, the second created.
        if let Some(rest) = &k.slots.rest {
            let mut words = rest.text.split_whitespace();
            if let Some(new) = words.next_back() {
                created.push(new.to_owned());
            }
        }
    }
}

/// Names a `foreach` source mentions, for the empty-loop lint.
#[must_use]
pub fn foreach_source_names(src: &ForeachSource) -> Vec<String> {
    match src {
        ForeachSource::OfLocal(n) | ForeachSource::OfGlobal(n) => vec![n.clone()],
        ForeachSource::OfVarlist(v) | ForeachSource::OfNewlist(v) => varlist_names(v),
        _ => Vec::new(),
    }
}

/// A set of names, ordered nowhere. Every ordering in this crate is an explicit
/// sort, so an `FxHashSet` here cannot reach output.
pub type NameSet = FxHashSet<String>;

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;

    #[test]
    fn the_model_parses_each_statement_once_and_tracks_depth() {
        let src = "\
sysuse auto, clear
foreach v of varlist price mpg {
    summarize `v'
}
";
        let idx = ParseIndex::new(src);
        let doc = Doc::build(&idx);
        assert_eq!(doc.stmts.len(), 4, "{:?}", doc.stmts.len());
        assert_eq!(doc.stmts[0].name(), "sysuse");
        assert_eq!(doc.stmts[1].name(), "foreach");
        assert_eq!(doc.stmts[1].depth, 0);
        assert_eq!(doc.stmts[2].depth, 1, "the loop body is nested");
    }

    #[test]
    fn prefixes_are_recorded_outermost_first() {
        let idx = ParseIndex::new("capture noisily regress price mpg\n");
        let doc = Doc::build(&idx);
        assert_eq!(doc.stmts[0].prefixes, vec!["capture", "noisily"]);
        assert_eq!(doc.stmts[0].name(), "regress");
    }

    #[test]
    fn a_nolint_marker_is_only_honoured_inside_a_comment() {
        let real = "*! nolint(R001)\nuse /abs/path.dta, clear\n";
        let idx = ParseIndex::new(real);
        let doc = Doc::build(&idx);
        assert!(doc.suppresses(&idx, 1, "R001"), "{:?}", doc.suppressions);

        let faked = "di \"*! nolint(R001)\"\nuse /abs/path.dta, clear\n";
        let idx = ParseIndex::new(faked);
        let doc = Doc::build(&idx);
        assert!(
            doc.suppressions.is_empty(),
            "a suppression inside a string is data: {:?}",
            doc.suppressions
        );
    }

    #[test]
    fn created_is_an_ast_fact() {
        let mut created = Vec::new();
        let mut used = Vec::new();
        creates_and_uses_line("generate ln_price = ln(price)", &mut created, &mut used);
        assert_eq!(created, vec!["ln_price"]);
        assert!(used.contains(&"price".to_owned()), "{used:?}");
    }
}
