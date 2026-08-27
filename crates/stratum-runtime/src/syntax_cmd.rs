//! The `syntax` command — design 02 §9.1, the runtime half.
//!
//! `stratum-parse` parses the mini-language into a [`SyntaxSpec`]. Applying that
//! spec to `` `0' `` is here, because it needs the dataset (to resolve a
//! varlist), the expression evaluator (to check an `integer`) and the macro
//! environment (to create the locals). Those are exactly the three things the
//! parser crate cannot have and still build for wasm.
//!
//! # What "applying" means
//!
//! ```stata
//! program define pp
//!     syntax varlist(min=1) [if] [in] [, Detail Level(integer 95)]
//! end
//! . pp price mpg if foreign==1, d level(90)
//! ```
//!
//! creates `` `varlist' `` = `price mpg`, `` `if' `` = `if foreign==1`,
//! `` `detail' `` = `detail`, `` `level' `` = `90`, and `` `in' ``,
//! `` `using' `` and every unmatched option as the empty string.
//!
//! Three rules from 02 §9.1 that every real ado-file depends on:
//!
//! * **A flag's local is the option's full lowercased name**, not `1`. Ado-code
//!   writes `` `detail' `` straight into the command it forwards to, so a `1`
//!   there would forward `summarize price, 1`.
//! * **An absent option's local is the EMPTY STRING, and it is still created.**
//!   `` if "`detail'" != "" `` is the universal idiom; an undefined local would
//!   expand to nothing and still work, but `` `level' `` with a declared
//!   default must be the default rather than empty.
//! * **A missing required clause is `r(100)`**, and the message names what was
//!   missing — `varlist required` — because "invalid syntax" on someone else's
//!   ado-file is unactionable.

use stratum_core::missing::is_missing;
use stratum_parse::lints::StataError;
use stratum_parse::parse::syntax::{SyntaxOpt, SyntaxSpec, SyntaxTy, VarKind};
use stratum_parse::{ParseMode, VarlistMode};

use crate::ctx::{ExecCtx, Ns};

/// What one `syntax` statement extracted from `` `0' ``.
///
/// Returned rather than written straight into the environment so that the
/// caller can report every problem before any local exists — a half-applied
/// `syntax` is worse than none, because the program body then runs against a
/// mixture of this call's arguments and the last one's.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SyntaxBindings {
    /// `(name, value)` in creation order.
    pub locals: Vec<(String, String)>,
}

impl SyntaxBindings {
    fn set(&mut self, name: &str, value: impl Into<String>) {
        let value = value.into();
        match self.locals.iter_mut().find(|(k, _)| k == name) {
            Some(slot) => slot.1 = value,
            None => self.locals.push((name.to_owned(), value)),
        }
    }

    /// Look one up — for tests and for the caller that wants to inspect before
    /// committing.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&str> {
        self.locals
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

/// The pieces of `` `0' `` the universal syntax splits it into.
#[derive(Debug, Default)]
struct Split<'a> {
    head: &'a str,
    if_: Option<&'a str>,
    in_: Option<&'a str>,
    using: Option<&'a str>,
    weight: Option<&'a str>,
    options: &'a str,
}

/// Split an argument line the way [U] 11.1 does.
///
/// Quote-aware, because `label var x "if you must"` puts the word `if` inside a
/// string and a naive split would take it for a qualifier.
fn split_args_line(s: &str) -> Split<'_> {
    let b = s.as_bytes();
    let mut cuts: Vec<(usize, usize, u8)> = Vec::new(); // (start, word_end, kind)
    let mut i = 0usize;
    let mut in_str = false;
    let mut depth = 0i32;
    let mut opts_at = s.len();
    while i < b.len() {
        match b[i] {
            b'"' => in_str = !in_str,
            b'(' | b'[' if !in_str => depth += 1,
            b')' | b']' if !in_str => depth -= 1,
            b',' if !in_str && depth == 0 => {
                opts_at = i;
                break;
            }
            c if !in_str && depth == 0 && (c.is_ascii_alphabetic()) => {
                let at_word_start = i == 0 || b[i - 1].is_ascii_whitespace();
                if at_word_start {
                    let end = word_end(b, i);
                    let kind = match &s[i..end] {
                        "if" => 1u8,
                        "in" => 2,
                        "using" => 3,
                        _ => 0,
                    };
                    if kind != 0 {
                        cuts.push((i, end, kind));
                    }
                    i = end;
                    continue;
                }
            }
            _ => {}
        }
        i += 1;
    }

    // A `[weight]` clause is the only bracketed thing at top level.
    let weight = find_bracketed(s, opts_at);

    let mut out = Split {
        options: if opts_at < s.len() {
            &s[opts_at + 1..]
        } else {
            ""
        },
        weight,
        ..Split::default()
    };
    let first_cut = cuts.first().map_or(opts_at, |c| c.0);
    out.head = s[..first_cut.min(opts_at)].trim();
    for (n, (start, _, kind)) in cuts.iter().enumerate() {
        let end = cuts.get(n + 1).map_or(opts_at, |c| c.0);
        let text = s[*start..end.max(*start)].trim();
        match kind {
            1 => out.if_ = Some(text),
            2 => out.in_ = Some(text),
            _ => out.using = Some(text),
        }
    }
    out
}

fn word_end(b: &[u8], from: usize) -> usize {
    let mut i = from;
    while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
        i += 1;
    }
    i
}

fn find_bracketed(s: &str, limit: usize) -> Option<&str> {
    let lo = s[..limit].find('[')?;
    let hi = s[lo..limit].find(']')? + lo;
    Some(s[lo..=hi].trim())
}

/// One option as typed: `level(90)` → `("level", Some("90"))`.
fn split_typed_options(s: &str) -> Vec<(String, Option<String>)> {
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
        let name_end = word_end(b, i);
        if name_end == i {
            i += 1;
            continue;
        }
        let name = s[i..name_end].to_owned();
        i = name_end;
        let mut arg = None;
        if i < b.len() && b[i] == b'(' {
            let mut depth = 0i32;
            let start = i + 1;
            let mut in_str = false;
            while i < b.len() {
                match b[i] {
                    b'"' => in_str = !in_str,
                    b'(' if !in_str => depth += 1,
                    b')' if !in_str => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
                i += 1;
            }
            arg = Some(s[start..i.min(s.len())].to_owned());
            i += 1;
        }
        out.push((name, arg));
    }
    out
}

/// Does `typed` abbreviate `opt`?
fn matches_option(opt: &SyntaxOpt, typed: &str) -> bool {
    let typed = typed.to_ascii_lowercase();
    if typed == opt.name {
        return true;
    }
    // A `min_abbrev` of 0 means "no abbreviation": the whole name is required.
    opt.min_abbrev > 0 && typed.len() >= opt.min_abbrev as usize && opt.name.starts_with(&typed)
}

impl ExecCtx<'_> {
    /// Apply a `syntax` statement to an argument line, producing the locals it
    /// declares.
    ///
    /// # Errors
    ///
    /// `r(100)` for a missing required clause or option, `r(198)` for an option
    /// the statement does not declare, `r(7)` for an `integer` option whose
    /// argument is not one, and whatever varlist resolution raised.
    pub fn apply_syntax(
        &mut self,
        spec: &SyntaxSpec,
        args: &str,
    ) -> Result<SyntaxBindings, StataError> {
        let mut out = SyntaxBindings::default();
        let split = split_args_line(args);

        // ---- the leading varlist ------------------------------------------
        if let Some(vs) = &spec.varlist {
            let head = split.head.trim();
            if head.is_empty() {
                if vs.required {
                    return Err(missing("varlist"));
                }
                out.set("varlist", "");
            } else {
                let mode = match vs.kind {
                    VarKind::Varlist | VarKind::Varname => VarlistMode::Existing,
                    VarKind::Newvarlist | VarKind::Newvarname => VarlistMode::New,
                };
                let names = self.resolve_names(head, mode)?;
                let one = matches!(vs.kind, VarKind::Varname | VarKind::Newvarname);
                if one && names.len() > 1 {
                    return Err(StataError::new(103, "too many variables specified"));
                }
                if (names.len() as u32) < vs.min {
                    return Err(StataError::new(102, "too few variables specified"));
                }
                if vs.max > 0 && names.len() as u32 > vs.max {
                    return Err(StataError::new(103, "too many variables specified"));
                }
                let joined = names.join(" ");
                out.set(if one { "varname" } else { "varlist" }, joined);
            }
        }

        // ---- the qualifiers ------------------------------------------------
        for (name, present, required) in [
            ("if", split.if_, spec.if_),
            ("in", split.in_, spec.in_),
            ("using", split.using, spec.using),
        ] {
            let Some(required) = required else {
                // The statement did not declare it, so the caller may not use
                // it: `syntax varlist` followed by `pp price if x` is r(101).
                if present.is_some() {
                    return Err(
                        StataError::new(101, format!("{name} not allowed")).token(name.to_owned())
                    );
                }
                continue;
            };
            match present {
                Some(text) => out.set(name, text),
                None if required => return Err(missing(name)),
                None => out.set(name, ""),
            }
        }
        if !spec.weights.is_empty() {
            out.set("weight", split.weight.unwrap_or(""));
        }

        // ---- the options ---------------------------------------------------
        let typed = split_typed_options(split.options);
        let mut seen = vec![false; spec.options.len()];
        for (name, arg) in &typed {
            // `nodetail` is the negation of `detail`; the local is left empty,
            // which is what `if "`detail'" != ""` then sees.
            let (lookup, negated) = match name.strip_prefix("no") {
                Some(base) if spec.options.iter().any(|o| matches_option(o, base)) => (base, true),
                _ => (name.as_str(), false),
            };
            let Some(pos) = spec.options.iter().position(|o| matches_option(o, lookup)) else {
                if spec.star {
                    // `syntax …, *` forwards what it does not know, verbatim.
                    let text = match arg {
                        Some(a) => format!("{name}({a})"),
                        None => name.clone(),
                    };
                    let prev = out.get("options").unwrap_or("").to_owned();
                    out.set(
                        "options",
                        if prev.is_empty() {
                            text
                        } else {
                            format!("{prev} {text}")
                        },
                    );
                    continue;
                }
                return Err(
                    StataError::new(198, format!("option {name} not allowed")).token(name.clone())
                );
            };
            seen[pos] = true;
            let opt = &spec.options[pos];
            if negated {
                out.set(&opt.name, "");
                continue;
            }
            out.set(&opt.name, self.option_value(opt, arg.as_deref())?);
        }

        // Every declared option gets a local, present or not.
        for (i, opt) in spec.options.iter().enumerate() {
            if seen[i] {
                continue;
            }
            if opt.required {
                return Err(
                    StataError::new(100, format!("option {}() required", opt.name))
                        .token(opt.name.clone()),
                );
            }
            out.set(&opt.name, opt.default.clone().unwrap_or_default());
        }
        if spec.star && out.get("options").is_none() {
            out.set("options", "");
        }
        if spec.anything.is_some() {
            out.set("anything", split.head.trim());
        }

        for (k, _) in &out.locals {
            self.access.note_named_write(Ns::Macro, k);
        }
        Ok(out)
    }

    /// Apply a `syntax` statement and install the locals.
    ///
    /// # Errors
    ///
    /// As [`ExecCtx::apply_syntax`]. Nothing is installed when it fails, so a
    /// program body never runs against a half-applied argument line.
    pub fn run_syntax(&mut self, body: &str, args: &str) -> Result<(), StataError> {
        let spec = stratum_parse::parse::syntax::parse_syntax(body);
        let bindings = self.apply_syntax(&spec, args)?;
        for (k, v) in bindings.locals {
            self.macros.set_local(&k, v);
        }
        Ok(())
    }

    fn option_value(&mut self, opt: &SyntaxOpt, arg: Option<&str>) -> Result<String, StataError> {
        Ok(match opt.ty {
            // A flag's local is its own name — see the module header.
            SyntaxTy::Flag => opt.name.clone(),
            SyntaxTy::Integer | SyntaxTy::Real => {
                let text = arg.unwrap_or("").trim();
                let (e, diags) = crate::eval::parse_expr_text(text, ParseMode::Execute);
                if diags
                    .iter()
                    .any(|d| d.severity == stratum_proto::Severity::Error)
                {
                    return Err(bad_arg(&opt.name, text, opt.ty));
                }
                let v = self
                    .eval_scalar(&e)?
                    .as_real()
                    .ok_or_else(|| bad_arg(&opt.name, text, opt.ty))?;
                if is_missing(v) {
                    return Err(bad_arg(&opt.name, text, opt.ty));
                }
                if opt.ty == SyntaxTy::Integer && v.fract() != 0.0 {
                    return Err(bad_arg(&opt.name, text, opt.ty));
                }
                stratum_parse::macros::stringify_number(v)
            }
            SyntaxTy::Varlist | SyntaxTy::Varname => self
                .resolve_names(arg.unwrap_or("").trim(), VarlistMode::Existing)?
                .join(" "),
            SyntaxTy::Newvarlist | SyntaxTy::Newvarname => self
                .resolve_names(arg.unwrap_or("").trim(), VarlistMode::New)?
                .join(" "),
            SyntaxTy::Passthru => match arg {
                Some(a) => format!("{}({a})", opt.name),
                None => opt.name.clone(),
            },
            // `asis` keeps the quoting; every other string type strips one layer.
            SyntaxTy::Asis => arg.unwrap_or("").to_owned(),
            _ => arg.unwrap_or("").trim().trim_matches('"').to_owned(),
        })
    }

    /// Resolve a varlist written as TEXT — which is what `syntax` is handed —
    /// into concrete names.
    fn resolve_names(&mut self, text: &str, mode: VarlistMode) -> Result<Vec<String>, StataError> {
        if text.is_empty() {
            return Ok(Vec::new());
        }
        let vl = stratum_parse::varlist::parse_varlist(
            text,
            stratum_proto::Span {
                start: 0,
                end: text.len() as u32,
            },
        );
        if mode == VarlistMode::New {
            // A new-variable list is not resolved against the dataset: the whole
            // point is that the names do not exist yet.
            return Ok(vl
                .items
                .iter()
                .filter_map(|i| match &i.kind {
                    stratum_parse::ast::varlist::VarItemKind::Single(a) => {
                        Some(a.base.as_text().to_owned())
                    }
                    stratum_parse::ast::varlist::VarItemKind::Interact { .. } => None,
                })
                .collect());
        }
        let frame = self.frames.current();
        let index = crate::dispatch::frame_names(frame);
        let cx = stratum_parse::VarlistCtx {
            vars: &index,
            varabbrev: self.settings.varabbrev,
        };
        let positions = stratum_parse::expand_varlist(&vl, &cx, mode)?;
        let names = positions
            .into_iter()
            .map(|p| frame.vars()[p as usize].name.to_string())
            .collect();
        self.access.read_var_layout = true;
        Ok(names)
    }
}

fn missing(what: &str) -> StataError {
    StataError::new(100, format!("{what} required")).token(what.to_owned())
}

fn bad_arg(opt: &str, text: &str, ty: SyntaxTy) -> StataError {
    let expected = match ty {
        SyntaxTy::Integer => "an integer",
        SyntaxTy::Real => "a number",
        _ => "a value",
    };
    StataError::new(
        7,
        format!("'{text}' found where {expected} expected in {opt}()"),
    )
    .token(text.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ctx::{NoHost, Transcript};
    use stratum_data::StorageType;
    use stratum_parse::parse::syntax::parse_syntax;

    struct Fixture {
        out: Transcript,
        host: NoHost,
    }

    fn fixture() -> Fixture {
        Fixture {
            out: Transcript::new(),
            host: NoHost,
        }
    }

    fn ctx_with_vars<'a>(f: &'a mut Fixture, names: &[&str]) -> ExecCtx<'a> {
        let mut ctx = ExecCtx::new(&mut f.out, &mut f.host);
        let frame = ctx.frames.current_mut();
        frame.set_n_obs(3);
        for n in names {
            frame.add_var(n, StorageType::Double).unwrap();
        }
        ctx
    }

    #[test]
    fn the_worked_example_from_design_02_section_9_1() {
        // `pp price mpg if foreign==1, d level(90)` — every local verified
        // against StataMP 18.5 in the design note this transcribes.
        let mut f = fixture();
        let mut ctx = ctx_with_vars(&mut f, &["price", "mpg", "foreign"]);
        let spec = parse_syntax("varlist(min=1) [if] [in] [, Detail Level(integer 95)]");
        let b = ctx
            .apply_syntax(&spec, "price mpg if foreign==1, d level(90)")
            .unwrap();
        assert_eq!(b.get("varlist"), Some("price mpg"));
        assert_eq!(b.get("if"), Some("if foreign==1"));
        assert_eq!(b.get("in"), Some(""));
        assert_eq!(
            b.get("detail"),
            Some("detail"),
            "a flag's local is its NAME"
        );
        assert_eq!(b.get("level"), Some("90"));
    }

    #[test]
    fn an_absent_option_with_a_default_gets_the_default() {
        let mut f = fixture();
        let mut ctx = ctx_with_vars(&mut f, &["price"]);
        let spec = parse_syntax("varlist [, Level(integer 95)]");
        let b = ctx.apply_syntax(&spec, "price").unwrap();
        assert_eq!(b.get("level"), Some("95"));
    }

    #[test]
    fn a_missing_required_clause_is_r100_and_names_it() {
        let mut f = fixture();
        let mut ctx = ctx_with_vars(&mut f, &["price"]);
        let spec = parse_syntax("varlist(min=1) [if]");
        let e = ctx.apply_syntax(&spec, "").unwrap_err();
        assert_eq!(e.rc, 100);
        assert_eq!(e.offending_token.as_deref(), Some("varlist"));
    }

    #[test]
    fn an_undeclared_option_is_r198_reported_as_typed() {
        // Stata says `option detial not allowed`, with the user's spelling —
        // that is what makes the "did you mean" fix possible.
        let mut f = fixture();
        let mut ctx = ctx_with_vars(&mut f, &["price"]);
        let spec = parse_syntax("varlist [, Detail]");
        let e = ctx.apply_syntax(&spec, "price, detial").unwrap_err();
        assert_eq!(e.rc, 198);
        assert_eq!(e.offending_token.as_deref(), Some("detial"));
    }

    #[test]
    fn an_integer_option_rejects_a_non_integer() {
        let mut f = fixture();
        let mut ctx = ctx_with_vars(&mut f, &["price"]);
        let spec = parse_syntax("varlist [, Level(integer 95)]");
        let e = ctx.apply_syntax(&spec, "price, level(90.5)").unwrap_err();
        assert_eq!(e.rc, 7);
        assert_eq!(e.offending_token.as_deref(), Some("90.5"));
    }

    #[test]
    fn a_qualifier_the_statement_did_not_declare_is_r101() {
        let mut f = fixture();
        let mut ctx = ctx_with_vars(&mut f, &["price", "foreign"]);
        let spec = parse_syntax("varlist");
        let e = ctx.apply_syntax(&spec, "price if foreign==1").unwrap_err();
        assert_eq!(e.rc, 101);
    }

    #[test]
    fn star_forwards_what_it_does_not_know() {
        let mut f = fixture();
        let mut ctx = ctx_with_vars(&mut f, &["price"]);
        let spec = parse_syntax("varlist [, Detail *]");
        let b = ctx
            .apply_syntax(&spec, "price, detail robust cluster(id)")
            .unwrap();
        assert_eq!(b.get("detail"), Some("detail"));
        assert_eq!(b.get("options"), Some("robust cluster(id)"));
    }

    #[test]
    fn nodetail_negates_detail() {
        let mut f = fixture();
        let mut ctx = ctx_with_vars(&mut f, &["price"]);
        let spec = parse_syntax("varlist [, Detail]");
        let b = ctx.apply_syntax(&spec, "price, nodetail").unwrap();
        assert_eq!(b.get("detail"), Some(""));
    }

    #[test]
    fn run_syntax_installs_nothing_when_it_fails() {
        // A half-applied `syntax` leaves the body running against a mixture of
        // this call's arguments and the last one's, which is worse than an error.
        let mut f = fixture();
        let mut ctx = ctx_with_vars(&mut f, &["price"]);
        ctx.macros.set_local("detail", "stale");
        let e = ctx
            .run_syntax("varlist [, Detail]", "price, nosuchopt")
            .unwrap_err();
        assert_eq!(e.rc, 198);
        assert_eq!(ctx.macros.local("detail"), Some("stale"));
    }
}
