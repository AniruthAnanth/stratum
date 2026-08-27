//! Varlists — design 02 §7 / [U] 11.4: reading one, and resolving it against a
//! live dataset.
//!
//! # Storage order is the answer to every question here
//!
//! `summarize price-rep78` on `auto.dta` gives `price mpg rep78` [V]
//! (`tests/golden/stata18/semantics.log`) — not alphabetical, not numeric.
//! `summarize m*` gives `make mpg` [V], in storage order rather than match
//! order. `ds` prints the twelve variables in storage order. A `-` range is
//! "everything between these two columns", and that is only meaningful because
//! a Stata dataset has an ORDER, which is also why `order` is a command.
//!
//! # A varlist is a word grammar, so it is read from text
//!
//! Design 02 §7's patterns — `pri~e`, `a-b`, `i.rep78`, `L(1/4).x`, `str8 name`
//! — are single whitespace-delimited words with internal structure. Reading them
//! off a token stream would mean reconstructing adjacency from spans on every
//! atom; reading them off the text is direct, and it is what keeps `pri ~ e`
//! (three tokens) from accidentally parsing as `pri~e` (one atom).

use stratum_proto::{Span, StorageType};

use crate::ast::varlist::VarPattern;
use crate::ast::varlist::{BaseLevel, FvOp, TsLag, TsOp, VarAtom, VarItem, VarItemKind, VarList};
use crate::lints::StataError;
use crate::parse::expr::parse_numlist;

/// The live column layout. CONTRACTS §13: implemented by `stratum-data`.
pub trait VarIndex {
    /// Number of variables.
    fn len(&self) -> usize;
    /// True when there are none.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// Name at a storage position.
    fn name(&self, pos: usize) -> &str;
    /// Storage position of an exact, case-sensitive name.
    fn position(&self, name: &str) -> Option<usize>;
    /// Storage type at a position.
    fn storage_type(&self, pos: usize) -> StorageType;
    /// True when the variable at a position holds strings.
    fn is_string(&self, pos: usize) -> bool {
        matches!(
            self.storage_type(pos),
            StorageType::Str { .. } | StorageType::StrL
        )
    }
}

/// Everything varlist resolution is allowed to see.
pub struct VarlistCtx<'a> {
    /// The dataset's columns.
    pub vars: &'a dyn VarIndex,
    /// `set varabbrev on|off`. When false, a bare name must match exactly:
    /// `di pri` is r(111) [V].
    pub varabbrev: bool,
}

/// Whether the names in a varlist must already exist.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum VarlistMode {
    /// Every name must resolve. `summarize`, `list`, `drop`.
    Existing,
    /// Every name must NOT exist. `generate`, `egen`.
    New,
    /// Some of each. `merge`'s key list against a using-file.
    Mixed,
}

/// A `VarIndex` over a flat name/type list.
///
/// The engine's real index lives in `stratum-data` over the column store. This
/// one exists because the EDITOR process has a `Vec<VariableInfo>` from the last
/// `EngineEvent` and nothing else, and completion (spec §22) has to resolve
/// `pri` against it without a dataset.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct SimpleVarIndex {
    names: Vec<String>,
    types: Vec<StorageType>,
}

impl SimpleVarIndex {
    /// Build from names in STORAGE order, all typed `double`.
    pub fn from_names<S: AsRef<str>>(names: &[S]) -> Self {
        SimpleVarIndex {
            names: names.iter().map(|n| n.as_ref().to_owned()).collect(),
            types: vec![StorageType::Double; names.len()],
        }
    }

    /// Build from names and types in STORAGE order.
    pub fn new(pairs: Vec<(String, StorageType)>) -> Self {
        let (names, types) = pairs.into_iter().unzip();
        SimpleVarIndex { names, types }
    }
}

impl VarIndex for SimpleVarIndex {
    fn len(&self) -> usize {
        self.names.len()
    }
    fn name(&self, pos: usize) -> &str {
        &self.names[pos]
    }
    fn position(&self, name: &str) -> Option<usize> {
        self.names.iter().position(|n| n == name)
    }
    fn storage_type(&self, pos: usize) -> StorageType {
        self.types[pos]
    }
}

/// Names that may never be a variable ([U] 11.3).
pub const RESERVED: &[&str] = &[
    "_all", "_b", "_coef", "_cons", "_n", "_N", "_pi", "_rc", "_se", "_skip", "byte", "int",
    "long", "float", "double", "strL", "if", "in", "using", "with", "alias",
];

/// True when `name` may not be used as a variable name.
///
/// The `str#` family and the `_r_*` family are patterns rather than literals, so
/// they are tested rather than listed.
pub fn is_reserved(name: &str) -> bool {
    if RESERVED.contains(&name) {
        return true;
    }
    if let Some(w) = name.strip_prefix("str") {
        if !w.is_empty() && w.bytes().all(|c| c.is_ascii_digit()) {
            return true;
        }
    }
    name.starts_with("_r_")
}

// ──────────────────────────── reading a varlist ─────────────────────────────

/// Read a varlist out of `src[span]`.
///
/// Never fails: an atom this grammar cannot make sense of becomes a
/// [`VarPattern::Name`] holding the raw word, and resolution reports r(111)
/// against it with the word the user typed. A parser that rejected here would
/// stop the editor from highlighting a line it merely does not understand,
/// which is decision D7.
pub fn parse_varlist(src: &str, span: Span) -> VarList {
    let text = &src[span.start as usize..span.end as usize];
    let base = span.start;
    let mut items = Vec::new();
    let b = text.as_bytes();
    let mut i = 0usize;
    while i < b.len() {
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= b.len() {
            break;
        }
        let start = i;
        // A word ends at whitespace. `(` is kept inside the word so that
        // `L(1/4).x`, `i(1 3).rep78` and `int(a b)` stay one atom.
        let mut depth = 0usize;
        while i < b.len() {
            match b[i] {
                b'(' => depth += 1,
                b')' => depth = depth.saturating_sub(1),
                c if c.is_ascii_whitespace() && depth == 0 => break,
                _ => {}
            }
            i += 1;
        }
        let word = &text[start..i];
        let wspan = Span {
            start: base + start as u32,
            end: base + i as u32,
        };
        items.push(parse_item(word, wspan));
    }
    VarList { items, span }
}

fn parse_item(word: &str, span: Span) -> VarItem {
    // `a#b` / `a##b` — [U] 11.4.3. Split before the atoms are read so that
    // `c.weight#c.weight` becomes two atoms rather than one unparseable word.
    if word.contains('#') {
        let full = word.contains("##");
        let atoms: Vec<VarAtom> = word
            .split('#')
            .filter(|p| !p.is_empty())
            .map(|p| parse_atom(p, span))
            .collect();
        if atoms.len() > 1 {
            return VarItem {
                span,
                kind: VarItemKind::Interact { atoms, full },
            };
        }
    }
    VarItem {
        span,
        kind: VarItemKind::Single(parse_atom(word, span)),
    }
}

fn parse_atom(word: &str, span: Span) -> VarAtom {
    let (fv, rest) = take_fv(word);
    let (ts, rest) = take_ts(rest);
    VarAtom {
        ts,
        fv,
        base: parse_pattern(rest),
        span,
    }
}

/// `i.`, `ib2.`, `ibn.`, `i(1 3).`, `c.`, `o.`.
fn take_fv(word: &str) -> (Option<FvOp>, &str) {
    let b = word.as_bytes();
    match b.first() {
        Some(b'c') if b.get(1) == Some(&b'.') => (Some(FvOp::C), &word[2..]),
        Some(b'o') if b.get(1) == Some(&b'.') => (Some(FvOp::O), &word[2..]),
        Some(b'i') => {
            let mut j = 1usize;
            let mut base = None;
            let mut levels = None;
            if b.get(j) == Some(&b'b') {
                j += 1;
                if b.get(j) == Some(&b'n') {
                    base = Some(BaseLevel::None);
                    j += 1;
                } else if b.get(j) == Some(&b'(') {
                    let close = word[j..].find(')').map_or(word.len(), |k| j + k);
                    base = Some(match &word[j + 1..close.min(word.len())] {
                        "first" => BaseLevel::First,
                        "last" => BaseLevel::Last,
                        "freq" => BaseLevel::Freq,
                        other => other
                            .parse()
                            .map(BaseLevel::Value)
                            .unwrap_or(BaseLevel::First),
                    });
                    j = (close + 1).min(word.len());
                } else {
                    let s = j;
                    while b.get(j).is_some_and(u8::is_ascii_digit) {
                        j += 1;
                    }
                    base = word[s..j].parse().ok().map(BaseLevel::Value);
                }
            } else if b.get(j) == Some(&b'(') {
                if let Some(k) = word[j..].find(')') {
                    let inner = &word[j + 1..j + k];
                    levels =
                        Some(parse_numlist(inner, Span { start: 0, end: 0 }).unwrap_or_default());
                    j += k + 1;
                }
            }
            if b.get(j) == Some(&b'.') {
                (Some(FvOp::I { base, levels }), &word[j + 1..])
            } else {
                (None, word)
            }
        }
        _ => (None, word),
    }
}

/// `L.`, `L2.`, `L(1/4).`, `F.`, `D.`, `S.`, and chains of them.
fn take_ts(word: &str) -> (Option<TsOp>, &str) {
    let mut rest = word;
    let mut ops: Vec<TsOp> = Vec::new();
    loop {
        let b = rest.as_bytes();
        let Some(&c0) = b.first() else { break };
        if !matches!(c0, b'L' | b'F' | b'D' | b'S' | b'l' | b'f' | b'd' | b's') {
            break;
        }
        let mut j = 1usize;
        let mut lag = TsLag::Fixed(1);
        if b.get(j) == Some(&b'(') {
            let Some(k) = rest[j..].find(')') else { break };
            let inner = &rest[j + 1..j + k];
            let Some(nl) = parse_numlist(inner, Span { start: 0, end: 0 }) else {
                break;
            };
            lag = TsLag::List(nl);
            j += k + 1;
        } else {
            let s = j;
            while b.get(j).is_some_and(u8::is_ascii_digit) {
                j += 1;
            }
            if j > s {
                lag = TsLag::Fixed(rest[s..j].parse().unwrap_or(1));
            }
        }
        if b.get(j) != Some(&b'.') {
            break;
        }
        // A single letter followed by `.` and then a name is a time-series
        // operator; `L.` at the end of a word is not.
        if j + 1 >= rest.len() {
            break;
        }
        ops.push(match c0.to_ascii_uppercase() {
            b'L' => TsOp::L(lag),
            b'F' => TsOp::F(lag),
            b'D' => TsOp::D(fixed(&lag)),
            _ => TsOp::S(fixed(&lag)),
        });
        rest = &rest[j + 1..];
    }
    match ops.len() {
        0 => (None, word),
        1 => (ops.pop(), rest),
        _ => (Some(TsOp::Chain(ops)), rest),
    }
}

fn fixed(l: &TsLag) -> u32 {
    match l {
        TsLag::Fixed(n) => (*n).max(0) as u32,
        TsLag::List(_) => 1,
    }
}

fn parse_pattern(word: &str) -> VarPattern {
    if word == "_all" || word == "*" {
        return VarPattern::All;
    }
    if let Some((name, label)) = word.split_once(':') {
        if !name.is_empty() && !label.is_empty() && !word.contains('-') {
            return VarPattern::Labeled {
                name: name.to_owned(),
                label: label.to_owned(),
            };
        }
    }
    if let Some((ty, inner)) = typed_prefix(word) {
        return VarPattern::Typed {
            ty,
            inner: inner
                .split(|c: char| c.is_ascii_whitespace())
                .filter(|s| !s.is_empty())
                .map(parse_pattern)
                .collect(),
        };
    }
    // `a-b` is a range only when the `-` separates two names; a leading `-` is
    // `gsort`'s descending marker and belongs to the word.
    if let Some(k) = word.find('-') {
        if k > 0 && k + 1 < word.len() {
            return VarPattern::Range {
                lo: word[..k].to_owned(),
                hi: word[k + 1..].to_owned(),
            };
        }
    }
    if word.contains('~') {
        return VarPattern::Tilde(word.to_owned());
    }
    if word.contains('*') || word.contains('?') {
        return VarPattern::Glob(word.to_owned());
    }
    VarPattern::Name(word.to_owned())
}

/// `str8 name`, `int(a b)`, `double(x)`.
fn typed_prefix(word: &str) -> Option<(StorageType, &str)> {
    let (head, rest) = match word.find('(') {
        Some(k) if word.ends_with(')') => (&word[..k], &word[k + 1..word.len() - 1]),
        _ => return None,
    };
    let ty = match head {
        "byte" => StorageType::Byte,
        "int" => StorageType::Int,
        "long" => StorageType::Long,
        "float" => StorageType::Float,
        "double" => StorageType::Double,
        "strL" => StorageType::StrL,
        other => {
            let w: u16 = other.strip_prefix("str")?.parse().ok()?;
            StorageType::Str { width: w }
        }
    };
    Some((ty, rest))
}

// ─────────────────────────── resolving a varlist ────────────────────────────

/// Resolve a varlist against the live dataset.
///
/// Returns storage positions. **Not deduplicated and not sorted**: repetition is
/// legal and meaningful in an existing-varlist ([U] 11.4.1), and every command
/// that takes a varlist reports in the order given. Within one glob the order is
/// storage order; across items it is source order.
pub fn expand_varlist(
    vl: &VarList,
    cx: &VarlistCtx<'_>,
    mode: VarlistMode,
) -> Result<Vec<u32>, StataError> {
    let mut out = Vec::with_capacity(vl.items.len());
    for item in &vl.items {
        match &item.kind {
            VarItemKind::Single(a) => resolve_atom(a, cx, mode, item.span, &mut out)?,
            VarItemKind::Interact { atoms, .. } => {
                for a in atoms {
                    resolve_atom(a, cx, mode, item.span, &mut out)?;
                }
            }
        }
    }
    Ok(out)
}

fn resolve_atom(
    atom: &VarAtom,
    cx: &VarlistCtx<'_>,
    mode: VarlistMode,
    span: Span,
    out: &mut Vec<u32>,
) -> Result<(), StataError> {
    resolve_pattern(&atom.base, cx, mode, span, out)
}

fn resolve_pattern(
    pat: &VarPattern,
    cx: &VarlistCtx<'_>,
    mode: VarlistMode,
    span: Span,
    out: &mut Vec<u32>,
) -> Result<(), StataError> {
    let vars = cx.vars;
    match pat {
        VarPattern::All => out.extend(0..vars.len() as u32),
        VarPattern::Glob(g) => {
            // Zero matches is NOT an error for a glob ([U] 11.4.1): `summarize
            // zz*` on a dataset with no `zz*` summarizes nothing.
            for i in 0..vars.len() {
                if glob_match(g, vars.name(i)) {
                    out.push(i as u32);
                }
            }
        }
        VarPattern::Tilde(t) => {
            // `~` has glob semantics but must land on exactly one variable.
            let g = t.replace('~', "*");
            let hits: Vec<u32> = (0..vars.len())
                .filter(|i| glob_match(&g, vars.name(*i)))
                .map(|i| i as u32)
                .collect();
            match hits.len() {
                1 => out.push(hits[0]),
                0 => return Err(not_found(t, span)),
                _ => {
                    return Err(StataError::new(111, "ambiguous abbreviation")
                        .at(span)
                        .token(t.clone()))
                }
            }
        }
        VarPattern::Range { lo, hi } => {
            let a = lookup(lo, cx, span)?;
            let b = lookup(hi, cx, span)?;
            // A storage-order range, in either direction: `ds mpg-make` is the
            // same three variables reversed, and Stata accepts both.
            if a <= b {
                out.extend(a..=b);
            } else {
                out.extend((b..=a).rev());
            }
        }
        VarPattern::Typed { ty, inner } => {
            let mut inner_hits = Vec::new();
            for p in inner {
                resolve_pattern(p, cx, mode, span, &mut inner_hits)?;
            }
            out.extend(
                inner_hits
                    .into_iter()
                    .filter(|i| vars.storage_type(*i as usize) == *ty),
            );
        }
        VarPattern::Labeled { name, .. } => out.push(lookup(name, cx, span)?),
        VarPattern::Name(n) => {
            if mode == VarlistMode::New {
                // A new variable is a NAME, not a lookup. Its legality — not
                // reserved, not already taken — is the command's business, not
                // the varlist's; `generate` reports r(110) itself.
                if let Some(p) = vars.position(n) {
                    out.push(p as u32);
                }
                return Ok(());
            }
            out.push(lookup(n, cx, span)?);
        }
        VarPattern::Hole { .. } => {}
    }
    Ok(())
}

/// A bare name: exact match first, then unique-prefix abbreviation.
///
/// [U] 11.2.3: *"typing `dvcr` is the same as typing `dvcr~`"*. With `set
/// varabbrev off` the fallback is gone and `di pri` is r(111) [V].
fn lookup(name: &str, cx: &VarlistCtx<'_>, span: Span) -> Result<u32, StataError> {
    if let Some(p) = cx.vars.position(name) {
        return Ok(p as u32);
    }
    if !cx.varabbrev {
        return Err(not_found(name, span));
    }
    let hits: Vec<usize> = (0..cx.vars.len())
        .filter(|i| cx.vars.name(*i).starts_with(name))
        .collect();
    match hits.len() {
        1 => Ok(hits[0] as u32),
        0 => Err(not_found(name, span)),
        _ => Err(StataError::new(111, "ambiguous abbreviation")
            .at(span)
            .token(name.to_owned())),
    }
}

fn not_found(name: &str, span: Span) -> StataError {
    // The exact wording of `tests/golden/stata18/errors.log`.
    StataError::new(111, format!("variable {name} not found"))
        .at(span)
        .token(name.to_owned())
}

/// `*` matches zero or more characters, `?` exactly one ([U] 11.4.1).
///
/// A hand-written matcher and not `regex`: this is the whole wildcard language,
/// it is forty lines, and `regex` would be well over a megabyte of binary in a
/// crate that has to ship to wasm.
pub fn glob_match(pat: &str, name: &str) -> bool {
    let (p, n) = (pat.as_bytes(), name.as_bytes());
    // Iterative backtracking: `star` remembers where the last `*` was so a
    // failed match can resume one byte later. Linear in practice, and it cannot
    // blow the stack on `i*f*95`.
    let (mut pi, mut ni) = (0usize, 0usize);
    let (mut star, mut resume) = (usize::MAX, 0usize);
    while ni < n.len() {
        if pi < p.len() && (p[pi] == b'?' || p[pi] == n[ni]) {
            pi += 1;
            ni += 1;
        } else if pi < p.len() && p[pi] == b'*' {
            star = pi;
            resume = ni;
            pi += 1;
        } else if star != usize::MAX {
            pi = star + 1;
            resume += 1;
            ni = resume;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == b'*' {
        pi += 1;
    }
    pi == p.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn auto() -> SimpleVarIndex {
        // auto.dta in STORAGE order, read off `ds` in
        // tests/golden/stata18/semantics.log (which prints column-major).
        SimpleVarIndex::from_names(&[
            "make",
            "price",
            "mpg",
            "rep78",
            "headroom",
            "trunk",
            "weight",
            "length",
            "turn",
            "displacement",
            "gear_ratio",
            "foreign",
        ])
    }

    fn names(spec: &str, abbrev: bool) -> Result<Vec<String>, StataError> {
        let idx = auto();
        let cx = VarlistCtx {
            vars: &idx,
            varabbrev: abbrev,
        };
        let vl = parse_varlist(
            spec,
            Span {
                start: 0,
                end: spec.len() as u32,
            },
        );
        Ok(expand_varlist(&vl, &cx, VarlistMode::Existing)?
            .into_iter()
            .map(|i| idx.name(i as usize).to_owned())
            .collect())
    }

    #[test]
    fn glob_is_storage_order() {
        assert_eq!(names("m*", true).unwrap(), ["make", "mpg"]);
    }

    #[test]
    fn range_is_storage_order_not_alphabetical() {
        assert_eq!(
            names("price-rep78", true).unwrap(),
            ["price", "mpg", "rep78"]
        );
        assert_eq!(names("make-mpg", true).unwrap(), ["make", "price", "mpg"]);
    }

    #[test]
    fn tilde_must_be_unique() {
        assert_eq!(names("pri~e", true).unwrap(), ["price"]);
        let e = names("r~p", true).unwrap_err();
        assert_eq!(e.rc, 111);
    }

    #[test]
    fn varabbrev_off_disables_only_bare_name_abbreviation() {
        assert_eq!(names("pri", true).unwrap(), ["price"]);
        assert_eq!(names("pri", false).unwrap_err().rc, 111);
        // 02 §7 OPEN QUESTION Q5: `~` is documented as a wildcard rather than an
        // abbreviation, so it keeps working with `varabbrev off`.
        assert_eq!(names("pri~e", false).unwrap(), ["price"]);
    }

    #[test]
    fn repetition_is_preserved() {
        assert_eq!(
            names("price price mpg", true).unwrap(),
            ["price", "price", "mpg"]
        );
    }

    #[test]
    fn all_expands_in_storage_order() {
        assert_eq!(names("_all", true).unwrap().len(), 12);
        assert_eq!(names("_all", true).unwrap()[0], "make");
    }

    #[test]
    fn globs_match_the_manual() {
        assert!(glob_match("i*f*95", "incomef1995"));
        assert!(glob_match("??*", "abc"));
        assert!(!glob_match("??*", "a"));
        assert!(glob_match("*", ""));
        assert!(!glob_match("r*p", "rep78"));
    }

    #[test]
    fn operators_are_parsed_but_not_evaluated() {
        let vl = parse_varlist("L2.gnp i.rep78", Span { start: 0, end: 14 });
        let VarItemKind::Single(a) = &vl.items[0].kind else {
            panic!("expected an atom")
        };
        assert_eq!(a.ts, Some(TsOp::L(TsLag::Fixed(2))));
        assert_eq!(a.base, VarPattern::Name("gnp".to_owned()));
        let VarItemKind::Single(b) = &vl.items[1].kind else {
            panic!("expected an atom")
        };
        assert!(matches!(b.fv, Some(FvOp::I { .. })));
        assert_eq!(b.base, VarPattern::Name("rep78".to_owned()));
    }

    #[test]
    fn reserved_names_are_recognised() {
        assert!(is_reserved("_all"));
        assert!(is_reserved("str8"));
        assert!(is_reserved("_r_b"));
        assert!(!is_reserved("price"));
    }
}
