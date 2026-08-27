//! The `syntax` mini-language — design 02 §9.1.
//!
//! ```stata
//! syntax varlist(min=1) [if] [in] [, Detail Level(integer 95)]
//! ```
//!
//! [V] with `pp price mpg if foreign==1, d level(90)` this sets
//! `` `varlist' `` = `price mpg`, `` `if' `` = `if foreign==1`,
//! `` `detail' `` = `detail`, `` `level' `` = `90`.
//!
//! Two rules that are easy to miss and break every real ado-file if wrong:
//!
//! * **Capitalised letters in an option name are its minimum abbreviation**, and
//!   the local the runtime creates is the LOWERCASED full name. `Detail` means
//!   "abbreviates to `d`, creates `` `detail' ``".
//! * **`[...]` means optional, bare means required.** `syntax varlist` fails
//!   with r(100) when no varlist is given; `syntax [varlist]` does not.
//!
//! This module produces the SPEC. Applying it to `` `0' `` is the runtime's job
//! — it needs the dataset to resolve a varlist and the expression evaluator to
//! check an `integer` — which is why the two are separate.

/// What kind of leading token list a program accepts.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum VarKind {
    /// `varlist` — one or more existing variables.
    Varlist,
    /// `varname` — exactly one existing variable.
    Varname,
    /// `newvarlist` — one or more names that must not exist.
    Newvarlist,
    /// `newvarname` — exactly one name that must not exist.
    Newvarname,
}

/// The `varlist(...)` clause.
#[derive(Clone, PartialEq, Debug)]
pub struct VarSpec {
    /// Which of the four spellings.
    pub kind: VarKind,
    /// Present without brackets.
    pub required: bool,
    /// `min=`.
    pub min: u32,
    /// `max=`.
    pub max: u32,
    /// `numeric`.
    pub numeric: bool,
    /// `string`.
    pub string: bool,
    /// `ts` — time-series operators allowed.
    pub ts: bool,
    /// `fv` — factor variables allowed.
    pub fv: bool,
    /// `default=`.
    pub default: Option<String>,
}

/// The type of an option's argument in a `syntax` statement.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum SyntaxTy {
    /// No argument — the local is set to the option name when present.
    Flag,
    /// `integer`.
    Integer,
    /// `real`.
    Real,
    /// `string`.
    String,
    /// `numlist`.
    Numlist,
    /// `varlist`.
    Varlist,
    /// `varname`.
    Varname,
    /// `newvarlist`.
    Newvarlist,
    /// `newvarname`.
    Newvarname,
    /// `name`.
    Name,
    /// `passthru` — the whole `opt(arg)` text, for forwarding.
    Passthru,
    /// `asis` — the argument verbatim, quoting included.
    Asis,
    /// `anything`.
    Anything,
}

/// One option a `syntax` statement declares.
#[derive(Clone, PartialEq, Debug)]
pub struct SyntaxOpt {
    /// The LOWERCASED full name — the local the runtime creates.
    pub name: String,
    /// Minimum abbreviation, from the count of capitalised leading letters.
    /// `0` means the whole name is required.
    pub min_abbrev: u8,
    /// Argument type.
    pub ty: SyntaxTy,
    /// Declared outside `[ ]`.
    pub required: bool,
    /// The `default(...)` text, when given.
    pub default: Option<String>,
}

/// A parsed `syntax` statement.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct SyntaxSpec {
    /// The leading varlist clause.
    pub varlist: Option<VarSpec>,
    /// `[if]` accepted; `if` (no brackets) means required.
    pub if_: Option<bool>,
    /// `[in]`.
    pub in_: Option<bool>,
    /// `[using]`.
    pub using: Option<bool>,
    /// `[fweight aweight …]` — the weight kinds accepted, as typed.
    pub weights: Vec<String>,
    /// `[anything]`.
    pub anything: Option<bool>,
    /// `*` — accept and pass through every unrecognised option.
    pub star: bool,
    /// The declared options, in the order written.
    pub options: Vec<SyntaxOpt>,
}

/// Parse the body of a `syntax` statement — everything after the word `syntax`.
///
/// Never fails. An unrecognised clause is skipped: a `syntax` line this build
/// does not fully understand must still let the ado-file's other clauses work,
/// because the alternative is an editor that refuses to highlight a file Stata
/// runs fine.
pub fn parse_syntax(body: &str) -> SyntaxSpec {
    let mut spec = SyntaxSpec::default();
    let (head, opts, opts_required) = split_option_list(body);
    for (clause, required) in clauses(head) {
        let (word, arg) = split_call(&clause);
        match word {
            "varlist" | "varname" | "newvarlist" | "newvarname" => {
                spec.varlist = Some(var_spec(word, arg.as_deref(), required));
            }
            "if" => spec.if_ = Some(required),
            "in" => spec.in_ = Some(required),
            "using" => spec.using = Some(required),
            "anything" => spec.anything = Some(required),
            w if w.ends_with("weight") => spec.weights.push(w.to_owned()),
            _ => {}
        }
    }
    for (clause, bare) in clauses(opts) {
        // `syntax [varlist] [, Detail]` — every option optional.
        // `syntax [varlist] , Level(integer)` — the whole list is REQUIRED,
        // and an individually bracketed clause inside it is not.
        let required = opts_required && bare;
        if clause.trim() == "*" {
            spec.star = true;
            continue;
        }
        let (word, arg) = split_call(&clause);
        if word.is_empty() {
            continue;
        }
        let (name, min_abbrev) = abbrev_from_case(word);
        let (ty, default) = opt_type(arg.as_deref());
        spec.options.push(SyntaxOpt {
            name,
            min_abbrev,
            ty,
            required,
            default,
        });
    }
    spec
}

/// Split into clauses, recording whether each was bracketed (optional).
fn clauses(s: &str) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    let b = s.as_bytes();
    let mut i = 0usize;
    while i < b.len() {
        while i < b.len() && (b[i].is_ascii_whitespace() || b[i] == b',') {
            i += 1;
        }
        if i >= b.len() {
            break;
        }
        if b[i] == b'[' {
            let mut depth = 0i32;
            let start = i;
            while i < b.len() {
                match b[i] {
                    b'[' => depth += 1,
                    b']' => {
                        depth -= 1;
                        if depth == 0 {
                            i += 1;
                            break;
                        }
                    }
                    _ => {}
                }
                i += 1;
            }
            // A bracketed group may hold several clauses: `[if] [in]` and
            // `[fweight aweight]` are both legal and mean the same thing.
            for (c, _) in clauses(&s[start + 1..i.saturating_sub(1).max(start + 1)]) {
                out.push((c, false));
            }
            continue;
        }
        let start = i;
        let mut depth = 0i32;
        while i < b.len() {
            match b[i] {
                b'(' => depth += 1,
                b')' => depth -= 1,
                c if (c.is_ascii_whitespace() || c == b',') && depth == 0 => break,
                _ => {}
            }
            i += 1;
        }
        out.push((s[start..i].to_owned(), true));
    }
    out
}

fn split_call(clause: &str) -> (&str, Option<String>) {
    let c = clause.trim();
    match (c.find('('), c.rfind(')')) {
        (Some(a), Some(b)) if b > a => (&c[..a], Some(c[a + 1..b].to_owned())),
        _ => (c, None),
    }
}

fn var_spec(word: &str, arg: Option<&str>, required: bool) -> VarSpec {
    let kind = match word {
        "varname" => VarKind::Varname,
        "newvarlist" => VarKind::Newvarlist,
        "newvarname" => VarKind::Newvarname,
        _ => VarKind::Varlist,
    };
    let single = matches!(kind, VarKind::Varname | VarKind::Newvarname);
    let mut s = VarSpec {
        kind,
        required,
        min: u32::from(single || required),
        max: if single { 1 } else { u32::MAX },
        numeric: false,
        string: false,
        ts: false,
        fv: false,
        default: None,
    };
    for part in arg.unwrap_or("").split_whitespace() {
        match part.split_once('=') {
            Some(("min", v)) => s.min = v.parse().unwrap_or(s.min),
            Some(("max", v)) => s.max = v.parse().unwrap_or(s.max),
            Some(("default", v)) => s.default = Some(v.to_owned()),
            _ => match part {
                "numeric" => s.numeric = true,
                "string" => s.string = true,
                "ts" => s.ts = true,
                "fv" => s.fv = true,
                _ => {}
            },
        }
    }
    s
}

fn opt_type(arg: Option<&str>) -> (SyntaxTy, Option<String>) {
    let Some(arg) = arg else {
        return (SyntaxTy::Flag, None);
    };
    let mut ty = SyntaxTy::String;
    let mut default = None;
    let mut words = arg.split_whitespace().peekable();
    if let Some(first) = words.next() {
        ty = match first {
            "integer" => SyntaxTy::Integer,
            "real" => SyntaxTy::Real,
            "string" => SyntaxTy::String,
            "numlist" => SyntaxTy::Numlist,
            "varlist" => SyntaxTy::Varlist,
            "varname" => SyntaxTy::Varname,
            "newvarlist" => SyntaxTy::Newvarlist,
            "newvarname" => SyntaxTy::Newvarname,
            "name" | "namelist" => SyntaxTy::Name,
            "passthru" => SyntaxTy::Passthru,
            "asis" => SyntaxTy::Asis,
            "anything" => SyntaxTy::Anything,
            other => {
                default = Some(other.to_owned());
                SyntaxTy::String
            }
        };
    }
    // `Level(integer 95)` — the trailing word is the default.
    if let Some(last) = words.last() {
        default = Some(last.to_owned());
    }
    (ty, default)
}

/// `Detail` → (`detail`, 1). `LEVel` → (`level`, 3). `robust` → (`robust`, 0).
///
/// 0 means "no abbreviation": Stata's rule is that the capitalised PREFIX is the
/// minimum, so a name with no capitals must be typed in full.
fn abbrev_from_case(word: &str) -> (String, u8) {
    let caps = word
        .chars()
        .take_while(|c| c.is_ascii_uppercase())
        .count()
        .min(u8::MAX as usize) as u8;
    (word.to_ascii_lowercase(), caps)
}

/// Split `body` into the leading clauses and the option list.
///
/// The option list is introduced by a comma, and the comma is almost always
/// INSIDE the optional-brackets: `syntax varlist [if] [in] [, Detail]`. A plain
/// "first comma at depth zero" split therefore finds nothing at all on the
/// manual's own example. The third return value is whether the list was
/// introduced by a BARE comma, which is Stata's way of saying "these options are
/// required".
fn split_option_list(body: &str) -> (&str, &str, bool) {
    let b = body.as_bytes();
    let mut paren = 0i32;
    let mut bracket = 0i32;
    let mut open_at: Option<usize> = None;
    for (i, &c) in b.iter().enumerate() {
        match c {
            b'(' => paren += 1,
            b')' => paren -= 1,
            b'[' => {
                if bracket == 0 && paren == 0 {
                    open_at = Some(i);
                }
                bracket += 1;
            }
            b']' => bracket -= 1,
            b',' if paren == 0 && bracket == 0 => {
                return (&body[..i], &body[i + 1..], true);
            }
            // `[, …]` — the bracket opened immediately before this comma, with
            // only whitespace between.
            b',' if paren == 0
                && bracket == 1
                && open_at.is_some_and(|k| body[k + 1..i].trim().is_empty()) =>
            {
                let k = open_at.expect("checked");
                let close = matching_bracket(b, k).unwrap_or(body.len());
                return (&body[..k], &body[i + 1..close], false);
            }
            _ => {}
        }
    }
    (body, "", false)
}

fn matching_bracket(b: &[u8], from: usize) -> Option<usize> {
    let mut depth = 0i32;
    for (i, &c) in b.iter().enumerate().skip(from) {
        match c {
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_manual_example_parses() {
        let s = parse_syntax("varlist(min=1) [if] [in] [, Detail Level(integer 95)]");
        let v = s.varlist.expect("varlist");
        assert_eq!(v.kind, VarKind::Varlist);
        assert!(v.required);
        assert_eq!(v.min, 1);
        assert_eq!(s.if_, Some(false));
        assert_eq!(s.in_, Some(false));
        assert_eq!(s.options.len(), 2);
        assert_eq!(s.options[0].name, "detail");
        assert_eq!(s.options[0].min_abbrev, 1);
        assert_eq!(s.options[0].ty, SyntaxTy::Flag);
        assert_eq!(s.options[1].name, "level");
        assert_eq!(s.options[1].ty, SyntaxTy::Integer);
        assert_eq!(s.options[1].default.as_deref(), Some("95"));
    }

    #[test]
    fn brackets_mean_optional_and_bare_means_required() {
        let s = parse_syntax("[varlist] [if]");
        assert!(!s.varlist.expect("varlist").required);
        let s = parse_syntax("varname using");
        assert!(s.varlist.expect("varname").required);
        assert_eq!(s.using, Some(true));
    }

    #[test]
    fn star_is_the_catch_all() {
        let s = parse_syntax("[varlist] [, Robust *]");
        assert!(s.star);
        assert_eq!(s.options.len(), 1);
        assert_eq!(s.options[0].min_abbrev, 1);
    }

    #[test]
    fn weights_are_collected() {
        let s = parse_syntax("[varlist] [fweight aweight] [if] [in]");
        assert_eq!(s.weights, ["fweight", "aweight"]);
    }
}
