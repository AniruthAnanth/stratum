//! `R001`–`R026`, the twenty-six reproducibility checks.
//!
//! Design 03 §10 supplies `R001`–`R024`; ARCHITECTURE C14 adds design 07's
//! `R025` (package dependencies declared) and `R026` (no `profile.do`
//! dependence). Every one of them is a static abstract interpretation over the
//! statements in document order, so **all twenty-six run headless, with no API
//! key, no network and no GUI** — which is what makes `stratum check` real.
//!
//! # The honesty rule, in code
//!
//! Design 03 §10 and `stratum_proto::repro`'s own header are emphatic: *"a green
//! mark that was inferred from static analysis is the single worst thing this
//! feature could ship."* Two consequences show up everywhere below.
//!
//! * A check that **cannot** decide emits nothing and lets the roll-up report
//!   `Tri::Unknown`. It never emits a "probably fine".
//! * A check whose evidence is incomplete emits at
//!   [`Confidence::Probable`] or [`Confidence::Speculative`] and says why in
//!   `detail`, rather than at `Exact` with a hedge in the prose.
//!
//! Three checks are structurally limited by what this crate is allowed to reach,
//! and each says so in its own section: `R004` cannot stat a file, `R022` cannot
//! read one, and `R024` needs an execution to have happened.

use camino::Utf8Path;
use rustc_hash::{FxHashMap, FxHashSet};
use stratum_effects::{EffectTable, FrameEffect, Name, StaticCtx};
use stratum_parse::ast::{BlockCommand, Command, Expr, StoredClass};
use stratum_proto::diagnostic::{Confidence, Related, Severity, Suggestion, SuggestionKind};
use stratum_proto::repro::{Finding, Tri};
use stratum_proto::{Edit, Span};

use super::{CheckMeta, Cx};
use crate::lints::dataflow::{expr_c_keys, expr_calls, expr_names, varlist_names, Stmt};
use crate::lints::facts;

/// One check: its metadata and the function that runs it.
pub struct Check {
    /// Id, severity, title, rule.
    pub meta: CheckMeta,
    /// The analysis.
    pub run: fn(&Cx<'_>, &mut Vec<Finding>),
}

/// All twenty-six, in id order.
pub static CHECKS: &[Check] = &[
    Check { meta: CheckMeta { id: "R001", severity: Severity::Warning, title: "Absolute file path", rule: "Any literal path in a file-reading or file-writing command that starts at the filesystem root, a drive letter, a UNC share or `~`. Such a path only works on the machine it was written on." }, run: r001 },
    Check { meta: CheckMeta { id: "R002", severity: Severity::Error, title: "Random seed defined", rule: "Any command or function that consumes the random-number stream, reached with no `set seed` dominating it in document order." }, run: r002 },
    Check { meta: CheckMeta { id: "R003", severity: Severity::Error, title: "Seed is itself deterministic", rule: "`set seed` whose argument is not an integer literal — a clock, a macro or a missing value gives a different stream on every run." }, run: r003 },
    Check { meta: CheckMeta { id: "R004", severity: Severity::Error, title: "Inputs resolved", rule: "Every literal input path resolves against the project listing the host supplied." }, run: r004 },
    Check { meta: CheckMeta { id: "R005", severity: Severity::Note, title: "Input path built dynamically", rule: "A read path containing a macro reference cannot be verified from the source, so the check says so rather than claiming a tick." }, run: r005 },
    Check { meta: CheckMeta { id: "R006", severity: Severity::Error, title: "Hidden interactive dependency", rule: "A macro, scalar, matrix, stored estimate or frame is read while nothing in this file defined it. This is the lint that catches \"it only works because I ran something in the command bar an hour ago\"." }, run: r006 },
    Check { meta: CheckMeta { id: "R007", severity: Severity::Warning, title: "Version pinned", rule: "No `version <n>` before the first executable command. Behaviour changes across Stata releases; a portable do-file pins it." }, run: r007 },
    Check { meta: CheckMeta { id: "R008", severity: Severity::Warning, title: "No order-dependent results", rule: "A `sort`/`gsort` without `, stable` on a key that is not provably unique, with an order-sensitive construct in its forward closure. Stata randomises the order of tied observations." }, run: r008 },
    Check { meta: CheckMeta { id: "R009", severity: Severity::Error, title: "No interactive-only commands", rule: "`browse`, `edit`, `pause`, `more`, `db`, `sleep` and `window` block or no-op in a headless run." }, run: r009 },
    Check { meta: CheckMeta { id: "R010", severity: Severity::Warning, title: "No output/input collisions", rule: "A path that is both read and written by this file: the second run reads different input than the first." }, run: r010 },
    Check { meta: CheckMeta { id: "R011", severity: Severity::Warning, title: "No environment dependence", rule: "Reads of `c(pwd)`, `c(current_date)`, `c(username)`, `c(os)`, `c(processors)` and friends. Their values differ between machines and between runs." }, run: r011 },
    Check { meta: CheckMeta { id: "R012", severity: Severity::Warning, title: "No unverifiable external execution", rule: "`shell`, `!`, `winexec`, `python`, `java`, `plugin call`, `ssc install`, `net install`. What they do is outside anything this project can verify." }, run: r012 },
    Check { meta: CheckMeta { id: "R013", severity: Severity::Warning, title: "`preserve` restored on all paths", rule: "A `preserve` with no matching `restore` in the same scope, or a `restore` with no `preserve`." }, run: r013 },
    Check { meta: CheckMeta { id: "R014", severity: Severity::Warning, title: "File establishes its own data", rule: "No `clear`, `use … , clear`, `import … , clear`, `sysuse … , clear` or `input` before the first command that reads a variable. The file assumes pre-loaded data." }, run: r014 },
    Check { meta: CheckMeta { id: "R015", severity: Severity::Warning, title: "Merges are validated", rule: "`merge` without `assert()` or `keep()`, where `_merge` is subsequently dropped without being inspected." }, run: r015 },
    Check { meta: CheckMeta { id: "R016", severity: Severity::Warning, title: "No `capture` over a data-modifying command", rule: "`capture` (not `capture noisily`) wrapping a command that writes data or files. The failure becomes invisible and the run silently diverges." }, run: r016 },
    Check { meta: CheckMeta { id: "R017", severity: Severity::Note, title: "No float equality comparison", rule: "`x == <non-integer literal>` on a stored value. Binary floating point rarely holds the literal exactly, and the answer differs between storage types." }, run: r017 },
    Check { meta: CheckMeta { id: "R018", severity: Severity::Note, title: "Storage type explicit", rule: "`generate` with no explicit type and a non-integer expression, while `set type` is never issued: the result then depends on a session setting." }, run: r018 },
    Check { meta: CheckMeta { id: "R019", severity: Severity::Note, title: "No path escapes the project root", rule: "A `../` sequence that resolves outside the project directory. Portable-ish, but not shareable." }, run: r019 },
    Check { meta: CheckMeta { id: "R020", severity: Severity::Note, title: "No reliance on processor count", rule: "`set processors`, or user code branching on `c(processors)`. Our own reductions are thread-count invariant; user code is not." }, run: r020 },
    Check { meta: CheckMeta { id: "R021", severity: Severity::Warning, title: "No temporary name in output", rule: "A `tempvar`/`tempname`/`tempfile` macro reaching `display`, `label`, `save`, `outsheet` or an export. Temporary names differ between runs." }, run: r021 },
    Check { meta: CheckMeta { id: "R022", severity: Severity::Warning, title: "Encoding pinned", rule: "`import delimited`/`import excel` with no `encoding()`. The default depends on the file's bytes and on the platform." }, run: r022 },
    Check { meta: CheckMeta { id: "R023", severity: Severity::Note, title: "Log is re-runnable", rule: "`log using` without `, replace`, or with a name built from a macro. The second run either fails or produces a different file." }, run: r023 },
    Check { meta: CheckMeta { id: "R024", severity: Severity::Error, title: "Declared effects hold", rule: "A `*! stratum:` effect annotation that the observed footprint contradicted. Emitted only after an execution; static analysis cannot decide it." }, run: r024 },
    Check { meta: CheckMeta { id: "R025", severity: Severity::Warning, title: "Package dependencies declared", rule: "A command that is neither built in, nor defined in this file, nor resolvable from the ado path the host reported. The file does not run on a clean install." }, run: r025 },
    Check { meta: CheckMeta { id: "R026", severity: Severity::Warning, title: "No `profile.do` dependence", rule: "Reliance on a `set matsize`/`maxvar`/`type`/`seed` value that the file never sets itself, and that therefore comes from the user's `profile.do`." }, run: r026 },
];

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn finding(id: &str, message: impl Into<String>, span: Span) -> Finding {
    let meta = CHECKS.iter().find(|c| c.meta.id == id).map(|c| &c.meta);
    Finding {
        lint: id.to_owned(),
        severity: meta.map_or(Severity::Warning, |m| m.severity),
        title: meta.map_or(String::new(), |m| m.title.to_owned()),
        message: message.into(),
        detail: None,
        evidence: Vec::new(),
        block: None,
        span: Some(span),
        fix: None,
        confidence: Confidence::Exact,
    }
}

fn with_detail(mut f: Finding, detail: impl Into<String>) -> Finding {
    f.detail = Some(detail.into());
    f
}

fn with_fix(mut f: Finding, label: &str, kind: SuggestionKind, edits: Vec<Edit>) -> Finding {
    f.fix = Some(Suggestion {
        label: label.to_owned(),
        kind,
        edits,
    });
    f
}

/// A file path written literally in a statement, with its extent.
struct FileRef {
    path: String,
    span: Span,
    write: bool,
}

/// Every literal file path a statement names.
///
/// Reads the `using` slot, the positional tail of the file commands, and the
/// `saving()` option. Quoting is stripped; a path containing a macro reference
/// is still returned, because `R005` and `R023` need to see one.
fn file_refs(st: &Stmt<'_>) -> Vec<FileRef> {
    let mut out = Vec::new();
    let name = st.name();
    let reads = facts::in_list(facts::READS_FILES, name);
    let writes = facts::in_list(facts::WRITES_FILES, name);

    if let Command::Known(k) = &st.ast.cmd {
        if let Some(f) = &k.slots.using {
            out.push(FileRef {
                path: unquote(&f.raw).to_owned(),
                span: f.span,
                // `merge … using x` reads; `outfile … using x` writes.
                write: writes && !reads,
            });
        }
        // A Stata command names at most one positional file, so a filled
        // `using` slot means the tail is a varlist or a subcommand, not a path.
        if let Some(rest) = &k.slots.rest {
            if (reads || writes) && k.slots.using.is_none() {
                if let Some((word, off)) = positional_path(name, &rest.text) {
                    out.push(FileRef {
                        path: word.to_owned(),
                        span: Span {
                            start: rest.span.start + off,
                            end: rest.span.start + off + word.len() as u32,
                        },
                        write: writes,
                    });
                }
            }
        }
        for opt in &k.slots.options.items {
            if opt.canonical == Some("saving") || opt.name == "saving" {
                if let Some(t) = st.option_text("saving") {
                    out.push(FileRef {
                        path: unquote(&t).to_owned(),
                        span: opt.span,
                        write: true,
                    });
                }
            }
        }
    }
    out
}

/// Commands whose positional tail *begins* with the filename, so a bare word
/// with no separator and no extension is still a path: `use auto`, `do setup`,
/// `erase scratch`.
const PATH_IS_FIRST_WORD: &[&str] = &[
    "copy", "do", "erase", "include", "mkdir", "rmdir", "run", "save", "saveold", "sysuse", "use",
    "webuse",
];

/// The positional word that names a file, with its offset in `text`.
///
/// Everything else in `READS_FILES`/`WRITES_FILES` puts a subcommand or a
/// varlist in front of the filename — `import delimited raw.csv`,
/// `graph export g.png`, `log using out.log` — so taking the first word
/// unconditionally produced `R004: "1:1" does not exist in the project` on a
/// perfectly ordinary `merge`. A word is a path when the command names its file
/// immediately, when it was quoted, or when it looks like one.
fn positional_path<'a>(name: &str, text: &'a str) -> Option<(&'a str, u32)> {
    let words = tail_words(text);
    if facts::in_list(PATH_IS_FIRST_WORD, name) {
        return words.first().map(|(w, off, _)| (*w, *off));
    }
    words
        .into_iter()
        .find(|(w, _, quoted)| *quoted || looks_like_path(w))
        .map(|(w, off, _)| (w, off))
}

/// A word is path-shaped when it carries a separator or an extension.
fn looks_like_path(w: &str) -> bool {
    if w.contains('/') || w.contains('\\') {
        return true;
    }
    w.rsplit_once('.').is_some_and(|(stem, ext)| {
        !stem.is_empty() && !ext.is_empty() && ext.chars().all(|c| c.is_ascii_alphanumeric())
    })
}

/// `(word, offset, was_quoted)` for the positional tail, stopping at the option
/// comma. Quotes are stripped, so an offset points at the first byte *inside*
/// the quotes and the span covers the path rather than the punctuation.
fn tail_words(text: &str) -> Vec<(&str, u32, bool)> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while let Some(&c) = b.get(i) {
        if c == b',' {
            break;
        }
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if c == b'"' {
            let start = i + 1;
            let Some(len) = text.get(start..).and_then(|t| t.find('"')) else {
                break;
            };
            if let Some(w) = text.get(start..start + len) {
                out.push((w, start as u32, true));
            }
            i = start + len + 1;
            continue;
        }
        let start = i;
        while b
            .get(i)
            .is_some_and(|c| !c.is_ascii_whitespace() && *c != b',')
        {
            i += 1;
        }
        if let Some(w) = text.get(start..i) {
            if !w.is_empty() {
                out.push((w, start as u32, false));
            }
        }
    }
    out
}

fn unquote(s: &str) -> &str {
    s.trim().trim_matches('"')
}

fn is_absolute(path: &str) -> bool {
    if path.starts_with('/') || path.starts_with('~') || path.starts_with("\\\\") {
        return true;
    }
    // A drive-letter root: `C:/proj`, `D:\\proj`. Three chars, not three bytes —
    // a path is user text and may start with anything.
    let mut c = path.chars();
    matches!(
        (c.next(), c.next(), c.next()),
        (Some(letter), Some(':'), Some('/' | '\\')) if letter.is_ascii_alphabetic()
    )
}

fn has_macro(s: &str) -> bool {
    s.contains('`') || s.contains('$')
}

/// Every macro reference in a statement's raw text, as names.
fn macro_refs(code: &str) -> Vec<String> {
    let b = code.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while let Some(&byte) = b.get(i) {
        match byte {
            b'`' => {
                let start = i + 1;
                let mut j = start;
                let mut depth = 1u32;
                while depth > 0 {
                    let Some(&c) = b.get(j) else { break };
                    match c {
                        b'`' => depth += 1,
                        b'\'' => depth -= 1,
                        _ => {}
                    }
                    j += 1;
                }
                if let Some(inner) = code.get(start..j.saturating_sub(1)) {
                    // `` `=exp' `` and `` `:xmf' `` are evaluations, not names.
                    if !inner.starts_with('=')
                        && !inner.starts_with(':')
                        && !inner.is_empty()
                        && inner.chars().all(|c| c.is_alphanumeric() || c == '_')
                    {
                        out.push(inner.to_owned());
                    }
                }
                i = j;
            }
            b'$' => {
                let start = i + 1;
                let braced = b.get(start) == Some(&b'{');
                let s = if braced { start + 1 } else { start };
                let mut j = s;
                while b
                    .get(j)
                    .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_')
                {
                    j += 1;
                }
                if let Some(inner) = code.get(s..j) {
                    if !inner.is_empty() {
                        out.push(inner.to_owned());
                    }
                }
                i = if braced { j + 1 } else { j };
            }
            _ => i += 1,
        }
    }
    out
}

/// Names a statement defines: `local x`, `global x`, `scalar x = …`,
/// `matrix X = …`, `tempvar a b`, `estimates store m`, `frame create f`.
fn defines(st: &Stmt<'_>) -> Vec<String> {
    let mut out = Vec::new();
    let name = st.name();
    let rest = st.rest().unwrap_or("").trim();
    let first = rest.split_whitespace().next().unwrap_or("");
    match name {
        "local" | "global" | "scalar" | "matrix" | "frame" => {
            let w = first.trim_start_matches('`').trim_end_matches('\'');
            // `scalar define x = 1`, `matrix define X = …`, `frame create f`.
            if matches!(w, "define" | "create" | "copy" | "rename" | "put") {
                if let Some(second) = rest.split_whitespace().nth(1) {
                    out.push(second.trim_matches('"').to_owned());
                }
            } else if !w.is_empty() {
                out.push(w.to_owned());
            }
        }
        "tempvar" | "tempname" | "tempfile" => {
            out.extend(rest.split_whitespace().map(str::to_owned));
        }
        "estimates" => {
            if first == "store" {
                if let Some(second) = rest.split_whitespace().nth(1) {
                    out.push(second.to_owned());
                }
            }
        }
        "foreach" | "forvalues" => {
            if let Command::Block(b) = &st.ast.cmd {
                match b.as_ref() {
                    BlockCommand::Foreach { loopvar, .. }
                    | BlockCommand::Forvalues { loopvar, .. } => out.push(loopvar.clone()),
                    _ => {}
                }
            }
        }
        _ => {}
    }
    // `syntax`/`args` inside a program define its locals.
    if name == "args" {
        out.extend(rest.split_whitespace().map(str::to_owned));
    }
    out
}

/// The set of every path a file reads and every path it writes.
type PathSites = FxHashMap<String, (Span, String)>;

fn read_write_sets(cx: &Cx<'_>) -> (PathSites, PathSites) {
    let mut reads: PathSites = FxHashMap::default();
    let mut writes: PathSites = FxHashMap::default();
    for st in &cx.doc.stmts {
        for fr in file_refs(st) {
            let key = normalize_path(&fr.path);
            let span = st.to_source(cx.idx, fr.span);
            // The normalised key is how two spellings of one file are matched;
            // the message quotes the path AS WRITTEN, because a user who reads
            // `/users/ana/raw.dta` in a finding goes looking for text that is
            // not in their file.
            let site = (span, fr.path);
            if fr.write {
                writes.entry(key).or_insert(site);
            } else {
                reads.entry(key).or_insert(site);
            }
        }
    }
    (reads, writes)
}

/// Lower-cased, separator-normalised, `.dta` implied. Stata appends `.dta` to a
/// `use`/`save` target with no suffix, so `use x` and `save x.dta` are the same
/// file and `R010` must see them as one.
fn normalize_path(p: &str) -> String {
    let p = p.replace('\\', "/").to_ascii_lowercase();
    if p.contains('.') {
        p
    } else {
        format!("{p}.dta")
    }
}

// ---------------------------------------------------------------------------
// R001 — absolute file path
// ---------------------------------------------------------------------------

fn r001(cx: &Cx<'_>, out: &mut Vec<Finding>) {
    for st in &cx.doc.stmts {
        for fr in file_refs(st) {
            if !is_absolute(&fr.path) {
                continue;
            }
            let span = st.to_source(cx.idx, fr.span);
            let f = finding(
                "R001",
                format!(
                    "`{}` is an absolute path — it only resolves on this machine",
                    fr.path
                ),
                span,
            );
            // A project-relative rewrite is offered only when the path is
            // actually under the project root. Otherwise there is nothing
            // honest to rewrite it to, and the finding stands on its own.
            let fix = cx.env.project_root.as_deref().and_then(|root| {
                Utf8Path::new(&fr.path)
                    .strip_prefix(root)
                    .ok()
                    .map(|rel| rel.to_string())
            });
            out.push(match fix {
                Some(rel) => with_fix(
                    f,
                    "Rewrite relative to the project root",
                    SuggestionKind::ChangePath,
                    vec![Edit { span, text: rel }],
                ),
                None => with_detail(
                    f,
                    "The path is outside the project root, so there is no relative form to \
                     rewrite it to. Copy the input into the project, or take it from a macro the \
                     project sets.",
                ),
            });
        }
    }
}

// ---------------------------------------------------------------------------
// R002 / R003 — the random-number stream
// ---------------------------------------------------------------------------

fn is_set_seed(st: &Stmt<'_>) -> bool {
    st.name() == "set"
        && st
            .rest()
            .is_some_and(|r| r.split_whitespace().next() == Some("seed"))
}

fn seed_argument(st: &Stmt<'_>) -> Option<String> {
    st.rest()
        .and_then(|r| r.split_whitespace().nth(1).map(str::to_owned))
}

fn consumes_rng(st: &Stmt<'_>) -> bool {
    if facts::in_list(facts::RNG_COMMANDS, st.name())
        || st
            .prefixes
            .iter()
            .any(|p| facts::in_list(facts::RNG_COMMANDS, p))
    {
        return true;
    }
    st.exprs()
        .iter()
        .flat_map(|e| expr_calls(e))
        .any(|f| facts::in_list(facts::RNG_FUNCTIONS, &f))
}

fn r002(cx: &Cx<'_>, out: &mut Vec<Finding>) {
    let mut seeded_at: Option<Span> = None;
    let mut first_consumer: Option<Span> = None;
    for st in &cx.doc.stmts {
        if is_set_seed(st) {
            if seeded_at.is_none() {
                seeded_at = Some(st.span);
            }
            continue;
        }
        if !consumes_rng(st) {
            continue;
        }
        if first_consumer.is_none() {
            first_consumer = Some(st.span);
        }
        if seeded_at.is_none() {
            out.push(with_fix(
                finding(
                    "R002",
                    format!(
                        "`{}` consumes the random-number stream and no `set seed` runs before it",
                        st.name()
                    ),
                    st.span,
                ),
                "Insert `set seed 20260821` at the top of the file",
                SuggestionKind::InsertLine,
                vec![Edit {
                    span: Span { start: 0, end: 0 },
                    text: "set seed 20260821\n".to_owned(),
                }],
            ));
        }
    }
    // A seed placed after the first consumer is the same failure wearing a
    // disguise: the first draw is unseeded, and only the rest reproduce.
    if let (Some(seed), Some(first)) = (seeded_at, first_consumer) {
        if seed.start > first.start {
            let mut f = finding(
                "R002",
                "`set seed` runs after the first command that draws random numbers",
                seed,
            );
            f.evidence.push(Related {
                span: first,
                file: None,
                message: "the first draw happens here, before the seed is set".to_owned(),
            });
            out.push(f);
        }
    }
}

fn r003(cx: &Cx<'_>, out: &mut Vec<Finding>) {
    for st in &cx.doc.stmts {
        if !is_set_seed(st) {
            continue;
        }
        let arg = seed_argument(st).unwrap_or_default();
        if arg.chars().all(|c| c.is_ascii_digit()) && !arg.is_empty() {
            continue;
        }
        out.push(with_detail(
            finding(
                "R003",
                format!("`set seed {arg}` is not an integer literal, so the stream differs between runs"),
                st.span,
            ),
            "A seed taken from the clock, from a macro, or left missing defeats the purpose of \
             setting one. Write the number out.",
        ));
    }
}

// ---------------------------------------------------------------------------
// R004 / R005 — inputs
// ---------------------------------------------------------------------------

fn r004(cx: &Cx<'_>, out: &mut Vec<Finding>) {
    // The honesty rule at its sharpest: with no complete project listing we do
    // not know whether a path resolves, so we say nothing and the roll-up
    // reports `Unknown`. This crate cannot stat a file — it builds for wasm and
    // runs in the editor — so "complete listing" is the host's assertion, not
    // ours.
    if !cx.env.file_listing_is_complete {
        return;
    }
    let known: FxHashSet<String> = cx
        .env
        .project_files
        .iter()
        .map(|p| normalize_path(p.as_str()))
        .collect();
    for st in &cx.doc.stmts {
        for fr in file_refs(st) {
            if fr.write || has_macro(&fr.path) || is_absolute(&fr.path) {
                continue;
            }
            if known.contains(&normalize_path(&fr.path)) {
                continue;
            }
            out.push(finding(
                "R004",
                format!("`{}` does not exist in the project", fr.path),
                st.to_source(cx.idx, fr.span),
            ));
        }
    }
}

fn r005(cx: &Cx<'_>, out: &mut Vec<Finding>) {
    for st in &cx.doc.stmts {
        for fr in file_refs(st) {
            if fr.write || !has_macro(&fr.path) {
                continue;
            }
            out.push(with_detail(
                {
                    let mut f = finding(
                        "R005",
                        format!(
                            "`{}` is built from a macro and cannot be verified here",
                            fr.path
                        ),
                        st.to_source(cx.idx, fr.span),
                    );
                    f.confidence = Confidence::Speculative;
                    f
                },
                "This is not necessarily wrong — a project-root macro is good practice. It is \
                 reported so the report does not claim a tick it did not earn.",
            ));
        }
    }
}

// ---------------------------------------------------------------------------
// R006 — hidden interactive dependency
// ---------------------------------------------------------------------------

fn r006(cx: &Cx<'_>, out: &mut Vec<Finding>) {
    let mut defined: FxHashSet<String> = FxHashSet::default();
    defined.extend(cx.env.locals.iter().cloned());
    defined.extend(cx.env.globals.iter().cloned());
    defined.extend(cx.env.scalars.iter().cloned());
    defined.extend(cx.env.matrices.iter().cloned());
    defined.extend(cx.env.stored_estimates.iter().cloned());
    // Stata's own built-in globals, which no file defines.
    defined.insert("S_level".to_owned());
    let mut reported: FxHashSet<String> = FxHashSet::default();
    // `e()` survives only until the next estimation, and on a clean run there
    // has been none — which is the "it only works because I ran `regress` in the
    // command bar an hour ago" case this check exists for.
    let mut estimated = !cx.env.e_names.is_empty();

    for st in &cx.doc.stmts {
        for name in macro_refs(st.code) {
            if defined.contains(&name) || !reported.insert(name.clone()) {
                continue;
            }
            out.push(with_detail(
                finding(
                    "R006",
                    format!("`{name}` is read but nothing in this file defines it"),
                    st.span,
                ),
                "The value comes from the session — something typed in the command bar, or a \
                 do-file run earlier. On a clean run there is nothing there, and the macro \
                 expands to nothing.",
            ));
        }
        if !estimated {
            for key in stored_reads(st) {
                if !reported.insert(format!("e({key})")) {
                    continue;
                }
                out.push(with_detail(
                    finding(
                        "R006",
                        format!("`e({key})` is read but no estimation in this file stores it"),
                        st.span,
                    ),
                    "The estimation results come from the session, so on a clean run `e()` is \
                     empty and the expression evaluates to missing.",
                ));
            }
        }
        estimated |= facts::in_list(facts::ESTIMATION, st.name());
        defined.extend(defines(st));
    }
}

/// The `e(...)` member names a statement reads.
///
/// `r()` and `s()` are deliberately **not** reported. Nearly every command sets
/// `r()`, so "nothing in this file stores it" is not decidable from the command
/// name, and the honesty rule says an undecidable check emits nothing rather
/// than a plausible guess. `e()` is different: design 03 §10's own list of
/// commands that store `e(b)` is `facts::ESTIMATION`, and it is finite.
///
/// Both halves are needed for the same reason `c_keys_of` has two: `display`,
/// `scalar` and `local` carry a `REST` slot, so their `e(N)` never reaches the
/// AST, while `summarize … if e(sample)` only reaches it.
fn stored_reads(st: &Stmt<'_>) -> Vec<String> {
    let mut out = Vec::new();
    for e in st.exprs() {
        e.walk(&mut |n| {
            let Expr::Stored {
                class: StoredClass::E,
                key,
                ..
            } = n
            else {
                return;
            };
            match key.as_ref() {
                Expr::Name(k, _) | Expr::Str(k, _) => out.push(k.clone()),
                _ => {}
            }
        });
    }
    out.extend(e_keys_in_text(st.code));
    out.sort();
    out.dedup();
    out
}

/// `e(<key>)` in raw statement text, outside string literals.
///
/// String literals are skipped because this feeds a `Severity::Error` finding
/// and `display "the e(nd)"` is prose, not a stored-result read. The preceding
/// byte must not be part of an identifier, so `replace(` and `mse(` do not read
/// as `e(`.
fn e_keys_in_text(code: &str) -> Vec<String> {
    let b = code.as_bytes();
    let mut out = Vec::new();
    let mut in_string = false;
    let mut i = 0usize;
    while let Some(&c) = b.get(i) {
        if c == b'"' {
            in_string = !in_string;
            i += 1;
            continue;
        }
        let boundary = i == 0
            || b.get(i - 1)
                .is_some_and(|p| !p.is_ascii_alphanumeric() && *p != b'_' && *p != b'.');
        if in_string || c != b'e' || b.get(i + 1) != Some(&b'(') || !boundary {
            i += 1;
            continue;
        }
        let start = i + 2;
        let Some(len) = code.get(start..).and_then(|t| t.find(')')) else {
            break;
        };
        if let Some(key) = code.get(start..start + len) {
            let key = key.trim();
            if !key.is_empty()
                && key
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
            {
                out.push(key.to_owned());
            }
        }
        i = start + len + 1;
    }
    out
}

// ---------------------------------------------------------------------------
// R007 — version pinned
// ---------------------------------------------------------------------------

fn r007(cx: &Cx<'_>, out: &mut Vec<Finding>) {
    for st in &cx.doc.stmts {
        if st.name() == "version" || st.has_prefix("version") {
            return;
        }
        // Anything executable that is not `version` settles it.
        if !matches!(st.name(), "#delimit" | "clear") {
            break;
        }
    }
    if cx.doc.stmts.is_empty() {
        return;
    }
    out.push(with_fix(
        finding(
            "R007",
            "no `version` statement — this file's behaviour can change with the Stata release \
             that runs it",
            Span { start: 0, end: 0 },
        ),
        "Insert `version 18` at the top of the file",
        SuggestionKind::InsertLine,
        vec![Edit {
            span: Span { start: 0, end: 0 },
            text: "version 18\n".to_owned(),
        }],
    ));
}

// ---------------------------------------------------------------------------
// R008 — order dependence
// ---------------------------------------------------------------------------

fn uses_observation_number(st: &Stmt<'_>) -> bool {
    use stratum_parse::ast::SysVar;
    st.exprs().iter().any(|e| {
        let mut found = false;
        e.walk(&mut |n| {
            if matches!(n, Expr::Sys(SysVar::NLower | SysVar::NUpper, _)) {
                found = true;
            }
            if let Expr::Index { .. } = n {
                found = true;
            }
        });
        found
    })
}

fn r008(cx: &Cx<'_>, out: &mut Vec<Finding>) {
    let mut pending: Option<(Span, String)> = None;
    let mut unique_keys: FxHashSet<String> = FxHashSet::default();

    for st in &cx.doc.stmts {
        let name = st.name();
        // `isid` proves uniqueness, which is exactly what makes the sort
        // deterministic. Design 03 §10 calls this out as the honest fix.
        if name == "isid" {
            if let Some(v) = st.varlist() {
                unique_keys.extend(varlist_names(v));
            }
            unique_keys.extend(
                st.rest()
                    .unwrap_or("")
                    .split_whitespace()
                    .map(str::to_owned),
            );
            continue;
        }
        if matches!(name, "sort" | "gsort") {
            if st.has_option("stable") {
                pending = None;
                continue;
            }
            let keys = st.varlist().map(varlist_names).unwrap_or_else(|| {
                st.rest()
                    .unwrap_or("")
                    .split_whitespace()
                    .map(str::to_owned)
                    .collect()
            });
            if keys.iter().any(|k| unique_keys.contains(k)) {
                pending = None;
                continue;
            }
            pending = Some((st.span, keys.join(" ")));
            continue;
        }
        let Some((sort_span, keys)) = pending.clone() else {
            continue;
        };
        let consumer = facts::in_list(facts::ORDER_SENSITIVE, name)
            || st.has_prefix("by")
            || st.has_prefix("bysort")
            || uses_observation_number(st);
        if !consumer {
            continue;
        }
        let mut f = finding(
            "R008",
            format!(
                "`sort {keys}` is not provably unique and `{}` below it depends on the order of \
                 tied observations",
                name
            ),
            sort_span,
        );
        f.confidence = Confidence::Probable;
        f.evidence.push(Related {
            span: st.span,
            file: None,
            message: "this command reads the order the sort produced".to_owned(),
        });
        out.push(with_fix(
            f,
            "Add `, stable` to the sort",
            SuggestionKind::InsertOption,
            vec![Edit {
                span: Span {
                    start: sort_span.end,
                    end: sort_span.end,
                },
                text: ", stable".to_owned(),
            }],
        ));
        pending = None;
    }
}

// ---------------------------------------------------------------------------
// R009 — interactive-only commands
// ---------------------------------------------------------------------------

fn r009(cx: &Cx<'_>, out: &mut Vec<Finding>) {
    for st in &cx.doc.stmts {
        if !facts::in_list(facts::INTERACTIVE_ONLY, st.name()) {
            continue;
        }
        out.push(with_detail(
            finding(
                "R009",
                format!("`{}` blocks or does nothing in a headless run", st.name()),
                st.span,
            ),
            "The command is fine interactively. It is a reproducibility problem because the same \
             file cannot then be run from the command line or in CI.",
        ));
    }
}

// ---------------------------------------------------------------------------
// R010 — output/input collisions
// ---------------------------------------------------------------------------

fn r010(cx: &Cx<'_>, out: &mut Vec<Finding>) {
    let (reads, writes) = read_write_sets(cx);
    let mut hits: Vec<(&String, Span, &str, Span)> = reads
        .iter()
        .filter_map(|(key, (rs, _))| {
            writes
                .get(key)
                .map(|(ws, written)| (key, *rs, written.as_str(), *ws))
        })
        .collect();
    hits.sort_by_key(|(key, _, _, _)| (*key).clone());
    for (_, read_span, path, write_span) in hits {
        let mut f = finding(
            "R010",
            format!("`{path}` is both read and written by this file"),
            write_span,
        );
        f.evidence.push(Related {
            span: read_span,
            file: None,
            message: "it is read here".to_owned(),
        });
        out.push(with_detail(
            f,
            "The second run reads what the first run wrote, so the file is not idempotent: \
             running it twice does not produce the same result as running it once.",
        ));
    }
}

// ---------------------------------------------------------------------------
// R011 / R020 / R026 — the environment and the session's settings
// ---------------------------------------------------------------------------

fn c_keys_of(st: &Stmt<'_>) -> Vec<String> {
    let mut keys: Vec<String> = st.exprs().iter().flat_map(|e| expr_c_keys(e)).collect();
    // `` `c(pwd)' `` in raw text — an extended macro function, not an
    // expression, so the AST does not carry it.
    let code = st.code;
    let mut from = 0usize;
    while let Some(rel) = code.get(from..).and_then(|t| t.find("c(")) {
        let at = from + rel + 2;
        if let Some(end) = code.get(at..).and_then(|t| t.find(')')) {
            if let Some(k) = code.get(at..at + end) {
                keys.push(k.trim().to_ascii_lowercase());
            }
            from = at + end;
        } else {
            break;
        }
    }
    keys.sort();
    keys.dedup();
    keys
}

fn r011(cx: &Cx<'_>, out: &mut Vec<Finding>) {
    for st in &cx.doc.stmts {
        for key in c_keys_of(st) {
            if !facts::in_list(facts::ENVIRONMENT_C_KEYS, &key) {
                continue;
            }
            out.push(with_detail(
                finding(
                    "R011",
                    format!("`c({key})` differs between machines or between runs"),
                    st.span,
                ),
                "Anything derived from it — a filename, a branch, a printed header — differs too, \
                 which is what makes two runs of this file disagree.",
            ));
        }
    }
}

fn r020(cx: &Cx<'_>, out: &mut Vec<Finding>) {
    for st in &cx.doc.stmts {
        let sets_processors = st.name() == "set"
            && st
                .rest()
                .is_some_and(|r| r.split_whitespace().next() == Some("processors"));
        let reads_processors = c_keys_of(st).iter().any(|k| k.starts_with("processors"));
        if !sets_processors && !reads_processors {
            continue;
        }
        out.push(with_detail(
            finding(
                "R020",
                "this file's result depends on how many processors the machine has",
                st.span,
            ),
            "Stratum's own reductions are thread-count invariant by construction (INV-3), so the \
             engine is not the risk here — code that branches on the count is.",
        ));
    }
}

/// Settings a `profile.do` can set and a do-file can silently inherit.
const PROFILE_SETTINGS: &[&str] = &["matsize", "maxvar", "seed", "type"];

fn r026(cx: &Cx<'_>, out: &mut Vec<Finding>) {
    let mut set_here: FxHashSet<&str> = FxHashSet::default();
    for st in &cx.doc.stmts {
        if st.name() != "set" {
            continue;
        }
        if let Some(w) = st.rest().and_then(|r| r.split_whitespace().next()) {
            if let Some(known) = PROFILE_SETTINGS.iter().find(|s| **s == w) {
                set_here.insert(known);
            }
        }
    }
    let mut reported: FxHashSet<String> = FxHashSet::default();
    for st in &cx.doc.stmts {
        for key in c_keys_of(st) {
            let Some(known) = PROFILE_SETTINGS.iter().find(|s| **s == key) else {
                continue;
            };
            if set_here.contains(known) || !reported.insert(key.clone()) {
                continue;
            }
            out.push(with_detail(
                finding(
                    "R026",
                    format!("`c({key})` is read but this file never issues `set {key}`"),
                    st.span,
                ),
                "The value comes from the user's `profile.do` or from whatever was set earlier in \
                 the session, so the same file gives different answers on two machines.",
            ));
        }
    }
}

// ---------------------------------------------------------------------------
// R012 — external execution
// ---------------------------------------------------------------------------

fn r012(cx: &Cx<'_>, out: &mut Vec<Finding>) {
    for st in &cx.doc.stmts {
        let name = st.name();
        let external =
            facts::in_list(facts::EXTERNAL, name) || st.code.trim_start().starts_with('!');
        if !external {
            continue;
        }
        out.push(with_detail(
            finding(
                "R012",
                format!("`{name}` runs something outside Stata, and nothing here can verify what"),
                st.span,
            ),
            "This also blocks a tick on \"runs from clean state\": a run whose result depends on \
             an external program is not something a static check or a clean-room re-run can \
             confirm.",
        ));
    }
}

// ---------------------------------------------------------------------------
// R013 — preserve / restore
// ---------------------------------------------------------------------------

fn r013(cx: &Cx<'_>, out: &mut Vec<Finding>) {
    let mut open: Vec<(Span, u32)> = Vec::new();
    for st in &cx.doc.stmts {
        match st.name() {
            "preserve" => open.push((st.span, st.depth)),
            "restore" => {
                // A `restore` consumes the innermost open `preserve`, whether or
                // not it is reported; popping unconditionally is what keeps a
                // later unmatched `preserve` from being blamed on this one.
                let matched = open.pop();
                if matched.is_none() {
                    out.push(with_detail(
                        finding("R013", "`restore` with no matching `preserve`", st.span),
                        "On a clean run there is no snapshot to restore and the command fails.",
                    ));
                }
            }
            _ => {}
        }
    }
    for (span, depth) in open {
        out.push(with_detail(
            finding("R013", "`preserve` with no matching `restore`", span),
            if depth > 0 {
                "The `preserve` is inside a conditional or a loop, so whether it is restored \
                 depends on which path runs."
            } else {
                "The snapshot is never restored, so every command after this point sees the \
                 modified data."
            },
        ));
    }
}

// ---------------------------------------------------------------------------
// R014 — the file establishes its own data
// ---------------------------------------------------------------------------

fn reads_a_variable(st: &Stmt<'_>) -> bool {
    st.varlist().is_some_and(|v| !v.items.is_empty())
        || st.exprs().iter().any(|e| !expr_names(e).is_empty())
}

fn r014(cx: &Cx<'_>, out: &mut Vec<Finding>) {
    for st in &cx.doc.stmts {
        let name = st.name();
        if facts::in_list(facts::ESTABLISHES_DATA, name) || st.has_option("clear") {
            return;
        }
        if reads_a_variable(st) {
            out.push(with_fix(
                with_detail(
                    finding(
                        "R014",
                        format!("`{name}` reads variables before this file loads any data"),
                        st.span,
                    ),
                    "The file works only when the right dataset happens to be in memory already. \
                     On a clean run there is nothing there.",
                ),
                "Load the data at the top of the file",
                SuggestionKind::InsertLine,
                vec![Edit {
                    span: Span { start: 0, end: 0 },
                    text: "use <dataset>, clear\n".to_owned(),
                }],
            ));
            return;
        }
    }
}

// ---------------------------------------------------------------------------
// R015 — unvalidated merge
// ---------------------------------------------------------------------------

fn r015(cx: &Cx<'_>, out: &mut Vec<Finding>) {
    for (i, st) in cx.doc.stmts.iter().enumerate() {
        if st.name() != "merge" {
            continue;
        }
        if st.has_option("assert") || st.has_option("keep") {
            continue;
        }
        let mut inspected = false;
        let mut dropped: Option<Span> = None;
        for later in cx.doc.stmts.iter().skip(i + 1) {
            let mentions = later.code.contains("_merge");
            if !mentions {
                continue;
            }
            let name = later.name();
            if matches!(name, "drop")
                && later
                    .varlist()
                    .is_some_and(|v| varlist_names(v).iter().any(|n| n == "_merge"))
            {
                dropped = Some(later.span);
                break;
            }
            if matches!(
                name,
                "tabulate" | "assert" | "count" | "list" | "summarize" | "keep"
            ) {
                inspected = true;
                break;
            }
            // A `merge` reuses `_merge`, so the window closes.
            if name == "merge" {
                break;
            }
        }
        if inspected {
            continue;
        }
        let mut f = finding(
            "R015",
            "`merge` with no `assert()` or `keep()`, and `_merge` is never inspected",
            st.span,
        );
        if let Some(span) = dropped {
            f.evidence.push(Related {
                span,
                file: None,
                message: "`_merge` is dropped here without being looked at".to_owned(),
            });
        }
        out.push(with_fix(
            with_detail(
                f,
                "Spec §21 names this exact case. A merge that silently drops non-matches, or \
                 silently keeps them, is the most common way a dataset quietly becomes the wrong \
                 dataset.",
            ),
            "Add `, assert(match)` to the merge",
            SuggestionKind::InsertOption,
            vec![Edit {
                span: Span {
                    start: st.span.end,
                    end: st.span.end,
                },
                text: ", assert(match)".to_owned(),
            }],
        ));
    }
}

// ---------------------------------------------------------------------------
// R016 — capture over a data-modifying command
// ---------------------------------------------------------------------------

/// Does the authoritative table say this statement writes data or files?
///
/// `EffectTable` is a MAY-set biased toward "yes" (`stratum-effects`' own header
/// makes that a soundness requirement), which is the same direction `R016` needs
/// — a swallowed failure of something that *might* write is exactly the silent
/// divergence the check is about. `row_order` counts because `sort`/`gsort` are
/// in the fallback list too, and the two answers must not disagree on a
/// statement merely because the caller happened to link a runtime.
fn table_writes(cx: &Cx<'_>, table: &dyn EffectTable, st: &Stmt<'_>) -> bool {
    // The audit has no macro environment and no live cwd of its own; both are
    // the host's, and `StaticCtx::bare` is the sound answer when they are absent.
    let no_macros: FxHashMap<Name, Name> = FxHashMap::default();
    let cwd = cx.env.cwd.as_deref().unwrap_or_else(|| Utf8Path::new("."));
    let e = table.effects(&st.ast, &StaticCtx::bare(cwd, &no_macros));
    !e.writes.is_empty()
        || !e.creates.is_empty()
        || !e.drops.is_empty()
        || !e.renames.is_empty()
        || !e.file_writes.is_empty()
        || e.row_membership != Tri::No
        || e.row_order != Tri::No
        || e.frame != FrameEffect::None
}

fn r016(cx: &Cx<'_>, out: &mut Vec<Finding>) {
    for st in &cx.doc.stmts {
        if !st.has_prefix("capture") || st.has_prefix("noisily") {
            continue;
        }
        let name = st.name();
        // Prefer the caller's `EffectTable` when it has a row: it is the
        // authority, and this crate's own list is the conservative fallback for
        // the wasm build where no runtime exists (see `lints::facts`).
        let writes = match cx.effects {
            Some(table) if table.is_known_command(name) => table_writes(cx, table, st),
            _ => {
                facts::in_list(facts::MODIFIES_DATA, name)
                    || facts::in_list(facts::WRITES_FILES, name)
            }
        };
        if !writes {
            continue;
        }
        out.push(with_detail(
            finding(
                "R016",
                format!("`capture` hides any failure of `{name}`, which changes data or files"),
                st.span,
            ),
            "`capture noisily` keeps the message; a following `if _rc` keeps the control flow. A \
             bare `capture` over a command that writes turns a failure into a silent divergence.",
        ));
    }
}

// ---------------------------------------------------------------------------
// R017 / R018 — numerics
// ---------------------------------------------------------------------------

fn r017(cx: &Cx<'_>, out: &mut Vec<Finding>) {
    use stratum_parse::ast::BinOp;
    for st in &cx.doc.stmts {
        for e in st.exprs() {
            let mut hit: Option<(String, f64)> = None;
            e.walk(&mut |n| {
                let Expr::Binary {
                    op: BinOp::Eq,
                    lhs,
                    rhs,
                    ..
                } = n
                else {
                    return;
                };
                let pair = match (lhs.as_ref(), rhs.as_ref()) {
                    (Expr::Name(v, _), Expr::Num(k, _)) | (Expr::Num(k, _), Expr::Name(v, _)) => {
                        Some((v.clone(), *k))
                    }
                    _ => None,
                };
                if let Some((v, k)) = pair {
                    if k.fract() != 0.0 {
                        hit = Some((v, k));
                    }
                }
            });
            let Some((v, k)) = hit else { continue };
            let mut f = finding(
                "R017",
                format!("`{v} == {k}` compares a stored value against a non-integer literal"),
                st.span,
            );
            f.confidence = Confidence::Probable;
            out.push(with_detail(
                f,
                "A `float` holds ~7 significant digits and a `double` ~16, so the stored value is \
                 rarely the literal exactly. `float(x) == float(k)` compares at float precision; a \
                 tolerance such as `abs(x - k) < 1e-7` states the intent.",
            ));
        }
    }
}

fn expression_is_non_integer(e: &Expr) -> bool {
    use stratum_parse::ast::BinOp;
    let mut yes = false;
    e.walk(&mut |n| match n {
        Expr::Num(v, _) if v.fract() != 0.0 => yes = true,
        Expr::Binary { op: BinOp::Div, .. } => yes = true,
        Expr::Call { name, .. }
            if matches!(
                name.as_str(),
                "ln" | "log"
                    | "log10"
                    | "exp"
                    | "sqrt"
                    | "sin"
                    | "cos"
                    | "tan"
                    | "normal"
                    | "invnormal"
                    | "runiform"
                    | "rnormal"
            ) =>
        {
            yes = true;
        }
        _ => {}
    });
    yes
}

fn r018(cx: &Cx<'_>, out: &mut Vec<Finding>) {
    let sets_type = cx.doc.stmts.iter().any(|st| {
        st.name() == "set"
            && st
                .rest()
                .is_some_and(|r| r.split_whitespace().next() == Some("type"))
    });
    if sets_type {
        return;
    }
    for st in &cx.doc.stmts {
        if !matches!(st.name(), "generate" | "egen") {
            continue;
        }
        // An explicit storage type appears as the first word of the varlist
        // slot; the parser folds it into the pattern, so the source text is the
        // reliable place to look.
        let after_cmd = st.code.split_whitespace().nth(1).unwrap_or("");
        if matches!(
            after_cmd,
            "byte" | "int" | "long" | "float" | "double" | "str" | "strL"
        ) || after_cmd.starts_with("str")
        {
            continue;
        }
        let Command::Known(k) = &st.ast.cmd else {
            continue;
        };
        let Some(rhs) = &k.slots.assign else { continue };
        if !expression_is_non_integer(rhs) {
            continue;
        }
        let mut f = finding(
            "R018",
            format!(
                "`{}` creates a non-integer variable with no storage type, so `set type` decides \
                 the precision",
                st.name()
            ),
            st.span,
        );
        f.confidence = Confidence::Probable;
        out.push(with_fix(
            with_detail(
                f,
                "The default is `float` unless the session changed it. A `float` holds about \
                 seven significant digits, which is enough for most data and not enough for a \
                 difference of two large numbers.",
            ),
            "Name the storage type explicitly",
            SuggestionKind::Rewrite,
            Vec::new(),
        ));
    }
}

// ---------------------------------------------------------------------------
// R019 — paths escaping the project root
// ---------------------------------------------------------------------------

fn r019(cx: &Cx<'_>, out: &mut Vec<Finding>) {
    for st in &cx.doc.stmts {
        for fr in file_refs(st) {
            if is_absolute(&fr.path) || !fr.path.contains("..") {
                continue;
            }
            // Count how far up the path climbs against how deep it descends.
            let mut depth = 0i32;
            let mut escapes = false;
            for part in fr.path.split(['/', '\\']) {
                match part {
                    ".." => {
                        depth -= 1;
                        if depth < 0 {
                            escapes = true;
                        }
                    }
                    "" | "." => {}
                    _ => depth += 1,
                }
            }
            if !escapes {
                continue;
            }
            out.push(with_detail(
                finding(
                    "R019",
                    format!("`{}` resolves outside the project directory", fr.path),
                    st.to_source(cx.idx, fr.span),
                ),
                "The file runs for you and not for anyone who checks the project out on its own.",
            ));
        }
    }
}

// ---------------------------------------------------------------------------
// R021 — temporary names in output
// ---------------------------------------------------------------------------

/// Commands whose output a reader sees, so a temporary name in one is visible.
const OUTPUT_COMMANDS: &[&str] = &[
    "display", "export", "label", "list", "outfile", "outsheet", "putdocx", "putexcel", "save",
    "saveold", "tabulate",
];

fn r021(cx: &Cx<'_>, out: &mut Vec<Finding>) {
    let mut temps: FxHashSet<String> = FxHashSet::default();
    for st in &cx.doc.stmts {
        if matches!(st.name(), "tempvar" | "tempname" | "tempfile") {
            temps.extend(
                st.rest()
                    .unwrap_or("")
                    .split_whitespace()
                    .map(str::to_owned),
            );
            continue;
        }
        if temps.is_empty() || !facts::in_list(OUTPUT_COMMANDS, st.name()) {
            continue;
        }
        let mut leaked: Vec<String> = macro_refs(st.code)
            .into_iter()
            .filter(|m| temps.contains(m))
            .collect();
        leaked.sort();
        leaked.dedup();
        for name in leaked {
            out.push(with_detail(
                finding(
                    "R021",
                    format!(
                        "the temporary name `{name}` reaches `{}`'s output",
                        st.name()
                    ),
                    st.span,
                ),
                "Stata generates a fresh, unpredictable name for each temporary on every run, so \
                 the output is different every time even when the numbers are identical.",
            ));
        }
    }
}

// ---------------------------------------------------------------------------
// R022 — encoding
// ---------------------------------------------------------------------------

fn r022(cx: &Cx<'_>, out: &mut Vec<Finding>) {
    for st in &cx.doc.stmts {
        if st.name() != "import" {
            continue;
        }
        let kind = st
            .rest()
            .and_then(|r| r.split_whitespace().next())
            .unwrap_or("");
        if !matches!(kind, "delimited" | "excel") {
            continue;
        }
        if st.has_option("encoding") {
            continue;
        }
        let mut f = finding(
            "R022",
            format!("`import {kind}` with no `encoding()`"),
            st.span,
        );
        // The definitive form of this check reads the file's bytes and asks
        // whether they are valid UTF-8. This crate cannot open a file, so the
        // finding is the weaker, structural one and says so.
        f.confidence = Confidence::Probable;
        out.push(with_detail(
            f,
            "Whether this matters depends on the file's bytes, which cannot be checked from here. \
             Naming the encoding costs nothing and removes the question.",
        ));
    }
}

// ---------------------------------------------------------------------------
// R023 — logs
// ---------------------------------------------------------------------------

fn r023(cx: &Cx<'_>, out: &mut Vec<Finding>) {
    for st in &cx.doc.stmts {
        if st.name() != "log" {
            continue;
        }
        let rest = st.rest().unwrap_or("");
        if rest.split_whitespace().next() != Some("using") {
            continue;
        }
        let name = rest
            .split_whitespace()
            .nth(1)
            .unwrap_or("")
            .trim_matches('"');
        if has_macro(name) {
            out.push(with_detail(
                finding(
                    "R023",
                    format!("the log name `{name}` is built from a macro"),
                    st.span,
                ),
                "If the macro carries a timestamp, every run leaves a new file behind and no two \
                 runs can be diffed.",
            ));
            continue;
        }
        if !st.has_option("replace") && !st.has_option("append") {
            out.push(with_fix(
                with_detail(
                    finding(
                        "R023",
                        "`log using` with neither `replace` nor `append`",
                        st.span,
                    ),
                    "The second run fails because the log already exists, which is the failure \
                     mode people work around by deleting the log by hand.",
                ),
                "Add `, replace`",
                SuggestionKind::InsertOption,
                vec![Edit {
                    span: Span {
                        start: st.span.end,
                        end: st.span.end,
                    },
                    text: ", replace".to_owned(),
                }],
            ));
        }
    }
}

// ---------------------------------------------------------------------------
// R024 — declared effects, contradicted at run time
// ---------------------------------------------------------------------------

fn r024(cx: &Cx<'_>, out: &mut Vec<Finding>) {
    // Design 03 §10 marks this one "emitted post-execution". It runs headless
    // like the other twenty-five — it is invoked on every audit — but with no
    // observed footprint there is nothing for it to contradict, and inventing a
    // verdict would be the inference the honesty rule forbids.
    let Some(observed) = cx.observed else {
        return;
    };
    for contradiction in observed.contradictions() {
        out.push(with_detail(
            finding(
                "R024",
                format!(
                    "`{}` declared it would not touch {}, and the run shows it did",
                    contradiction.command, contradiction.footprint
                ),
                contradiction.span,
            ),
            "The block is permanently downgraded to unknown-effects, so the staleness sweep can \
             no longer prove anything about what depends on it.",
        ));
    }
}

// ---------------------------------------------------------------------------
// R025 — package dependencies declared
// ---------------------------------------------------------------------------

fn r025(cx: &Cx<'_>, out: &mut Vec<Finding>) {
    let installed: FxHashSet<&str> = cx.env.installed_ado.iter().map(String::as_str).collect();
    let programs: FxHashSet<String> = crate::diagnose::didyoumean::user_programs(cx.doc)
        .into_iter()
        .collect();
    let mut unresolved: Vec<(String, Span)> = Vec::new();
    let mut seen: FxHashSet<String> = FxHashSet::default();

    for st in &cx.doc.stmts {
        if st.canonical.is_some() {
            continue;
        }
        let name = st.head.clone();
        if name.is_empty()
            || has_macro(&name)
            || programs.contains(&name)
            || installed.contains(name.as_str())
            || !seen.insert(name.clone())
        {
            continue;
        }
        // `shell`, `python`, `browse` and friends are built in, and this crate
        // says so in `lints::facts` even where `stratum-parse`'s command table
        // does not model them. `R012` and `R009` already report them; a second
        // code for the same statement is exactly what ARCHITECTURE C14 exists
        // to prevent.
        if facts::in_list(facts::EXTERNAL, &name) || facts::in_list(facts::INTERACTIVE_ONLY, &name)
        {
            continue;
        }
        unresolved.push((name, st.span));
    }
    unresolved.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, span) in &unresolved {
        out.push(with_detail(
            finding(
                "R025",
                format!("`{name}` is not built in and this file does not say where it comes from"),
                *span,
            ),
            "On a clean install the command does not exist and the run stops here.",
        ));
    }
    if unresolved.is_empty() {
        return;
    }
    // The copyable block, once, attached to the first occurrence.
    let block: String = unresolved
        .iter()
        .map(|(n, _)| format!("ssc install {n}, replace\n"))
        .collect();
    if let Some(first) = out.iter_mut().rev().nth(unresolved.len() - 1) {
        first.fix = Some(Suggestion {
            label: "Insert the `ssc install` block at the top of the file".to_owned(),
            kind: SuggestionKind::InsertLine,
            edits: vec![Edit {
                span: Span { start: 0, end: 0 },
                text: block,
            }],
        });
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;

    #[test]
    fn the_registry_is_contiguous_and_unique() {
        assert_eq!(CHECKS.len(), 26);
        for (i, c) in CHECKS.iter().enumerate() {
            assert_eq!(c.meta.id, format!("R{:03}", i + 1));
            assert!(!c.meta.title.is_empty());
            assert!(c.meta.rule.len() > 40, "{} has no rule text", c.meta.id);
        }
    }

    #[test]
    fn path_helpers_agree_with_the_rule_text() {
        for p in [
            "/abs/x.dta",
            "~/x.dta",
            "C:/proj/x.dta",
            "D:\\proj\\x.dta",
            "\\\\srv\\s\\x",
        ] {
            assert!(is_absolute(p), "{p}");
        }
        for p in ["data/x.dta", "../raw/x.dta", "x.dta"] {
            assert!(!is_absolute(p), "{p}");
        }
        assert_eq!(normalize_path("Data/X"), "data/x.dta");
        assert_eq!(normalize_path("data\\x.dta"), "data/x.dta");
    }

    #[test]
    fn stored_estimate_reads_ignore_prose_and_identifier_tails() {
        assert_eq!(e_keys_in_text("display e(N)"), vec!["N"]);
        assert_eq!(e_keys_in_text("scalar b = e(b)[1,1]"), vec!["b"]);
        assert_eq!(
            e_keys_in_text("summarize price if e(sample)"),
            vec!["sample"]
        );
        // `replace(` and `mse(` end in `e` but are not `e(`.
        assert!(e_keys_in_text("save out.dta, replace(1)").is_empty());
        assert!(e_keys_in_text("estat ic, mse(x)").is_empty());
        // Prose is prose, and this feeds an Error-severity finding.
        assert!(e_keys_in_text("display \"the e(nd)\"").is_empty());
    }

    #[test]
    fn macro_references_are_extracted_and_evaluations_are_not() {
        assert_eq!(macro_refs("summarize `outcome'"), vec!["outcome"]);
        assert_eq!(macro_refs("use $ROOT/x.dta"), vec!["ROOT"]);
        assert_eq!(macro_refs("use ${ROOT}/x.dta"), vec!["ROOT"]);
        assert!(macro_refs("di `=2+2'").is_empty());
        assert!(macro_refs("di `: word 1 of a b'").is_empty());
    }
}
