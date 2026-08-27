//! The bridge from a parsed statistical command to `stratum-stats`, and back.
//!
//! # What "glue" means, precisely
//!
//! `stratum-stats` owns the numbers and the byte-exact tables. It does not own
//! Stata's argument grammar, and it must not: varlist abbreviation, `if`/`in`
//! resolution, option validation and the `r()`/`e()` lifecycle are interpreter
//! concerns that every command shares. So this module does exactly five things
//! and then gets out of the way:
//!
//! 1. resolve the varlist against the live frame (r(111) with the offending
//!    token when a name does not exist);
//! 2. turn `if`/`in` into a [`Sample`](stratum_data::Sample);
//! 3. reject options this build does not implement — r(198) for an option the
//!    command has never had, **rc 10** for one it has but we have not written;
//! 4. hand [`CmdHost::run_stat`] a [`StatRequest`] with no parsing left in it;
//! 5. print `return list` / `ereturn list`.
//!
//! # Why option rejection is here and not in the statistics crate
//!
//! `summarize price, detial` must say `option detial not allowed` — the option
//! **as the user typed it**, not the canonical spelling it nearly matched
//! (verified, `errors.log`). Only the parse layer still knows what was typed,
//! so the check belongs on this side of the seam.

use stratum_parse::ast::expr::StoredClass;
use stratum_parse::ast::CommandAst;
use stratum_parse::{StataError, VarlistMode};
use stratum_proto::ScalarValue;

use super::{
    build_sample, err, resolve_varlist, rest, slots, take_options, CmdHost, CmdOutcome, CmdResult,
    Out, StatRequest,
};

/// `summarize [varlist] [if] [in] [, detail meanonly separator(#)]`.
pub fn summarize(host: &mut dyn CmdHost, ast: &CommandAst) -> CmdResult {
    stat(
        host,
        ast,
        "summarize",
        &["detail", "meanonly", "separator", "format"],
        true,
    )
}

/// `tabulate var1 [var2] [if] [in] [, options]`.
pub fn tabulate(host: &mut dyn CmdHost, ast: &CommandAst) -> CmdResult {
    stat(
        host,
        ast,
        "tabulate",
        &[
            "chi2", "column", "row", "cell", "missing", "nofreq", "nolabel", "exact", "all",
        ],
        false,
    )
}

/// `correlate [varlist] [if] [in] [, covariance means]`.
pub fn correlate(host: &mut dyn CmdHost, ast: &CommandAst) -> CmdResult {
    stat(
        host,
        ast,
        "correlate",
        &["covariance", "means", "wrap"],
        true,
    )
}

/// `pwcorr [varlist] [if] [in] [, sig obs print(#) star(#)]`.
pub fn pwcorr(host: &mut dyn CmdHost, ast: &CommandAst) -> CmdResult {
    stat(
        host,
        ast,
        "pwcorr",
        &["sig", "obs", "print", "star", "bonferroni"],
        true,
    )
}

/// `ttest var [== exp] [if] [in] [, by(group) unequal welch level(#)]`.
pub fn ttest(host: &mut dyn CmdHost, ast: &CommandAst) -> CmdResult {
    let s = slots(ast).ok_or_else(|| err::invalid("ttest"))?;
    // `ttest price` with no `by()` and no `== exp` is r(100) `by() option
    // required` — verified, `errors.log`. The check is here because only the
    // grammar layer can see that both forms are absent.
    if s.assign.is_none() && !super::has_option(s, "by") {
        return Err(err::required("by() option").at(ast.span));
    }
    stat(
        host,
        ast,
        "ttest",
        &["by", "unequal", "welch", "level", "unpaired"],
        false,
    )
}

/// `regress depvar [indepvars] [if] [in] [weight] [, options]`.
pub fn regress(host: &mut dyn CmdHost, ast: &CommandAst) -> CmdResult {
    stat(
        host,
        ast,
        "regress",
        &[
            "noconstant",
            "level",
            "vce",
            "robust",
            "cluster",
            "beta",
            "noheader",
            "notable",
        ],
        false,
    )
}

/// `predict newvar [if] [in] [, xb residuals stdp]`.
pub fn predict(host: &mut dyn CmdHost, ast: &CommandAst) -> CmdResult {
    let s = slots(ast).ok_or_else(|| err::invalid("predict"))?;
    let options = take_options(s, &["xb", "residuals", "stdp", "score", "rstandard"])?;
    let sample = build_sample(host, s.if_.as_ref(), s.in_.as_ref())?;
    // The new variable is a NAME, not a resolved position: it does not exist
    // yet, so it travels in `cmdline` and the stats crate creates it.
    //
    // Stata announces the default statistic only when the user named NONE of
    // them, so the test is "no options at all" — not "no `xb`". `predict yhat,
    // residuals` is silent, and so is `predict yhat, xb`: naming the statistic
    // is what suppresses the note.
    if options.is_empty() {
        // `predict yhat` prints `(option xb assumed; fitted values)` and
        // succeeds — verified, both `errors.log` and `core_surface.log`.
        let mut out = Out::new();
        out.txt("(option xb assumed; fitted values)");
        out.nl();
        host.emit(out.runs());
    }
    let req = StatRequest {
        cmd: "predict",
        cmdline: cmdline(ast),
        vars: Vec::new(),
        sample,
        options,
    };
    host.run_stat(&req)
}

/// The shared shape of every r-class / e-class statistical command.
fn stat(
    host: &mut dyn CmdHost,
    ast: &CommandAst,
    cmd: &'static str,
    allowed: &[&str],
    all_vars_if_empty: bool,
) -> CmdResult {
    let s = slots(ast).ok_or_else(|| err::invalid(cmd))?;
    let options = take_options(s, allowed)?;
    // An option the command really has, that this build has not implemented,
    // is exit 10 and not r(198): "we are incomplete" must stay distinct from
    // "you are wrong" (plan §W09).
    for (name, _) in &options {
        if !host.implements_option(cmd, name) {
            return Err(err::unsupported(&format!("{cmd}, option {name}")).token(name));
        }
    }
    let vars = resolve_varlist(
        host,
        s.varlist.as_ref(),
        VarlistMode::Existing,
        all_vars_if_empty,
    )?;
    let sample = build_sample(host, s.if_.as_ref(), s.in_.as_ref())?;
    let req = StatRequest {
        cmd,
        cmdline: cmdline(ast),
        vars,
        sample,
        options,
    };
    host.run_stat(&req)
}

/// The command line as submitted, after macro expansion — `e(cmdline)`.
fn cmdline(ast: &CommandAst) -> String {
    // The AST carries spans into the expanded text, not the text itself, so the
    // host is the only thing that can quote it back exactly. `rest` is what a
    // REST-slot command kept; for a slotted command it is empty and the host
    // fills `cmdline` in. Deliberately not reconstructed from the AST: a
    // round-tripped `e(cmdline)` that is not byte-identical to what the user
    // typed is worse than an empty one.
    rest(ast).to_owned()
}

// ---------------------------------------------------------------------------
// return list / ereturn list
// ---------------------------------------------------------------------------

/// `return list`.
pub fn return_list(host: &mut dyn CmdHost, ast: &CommandAst) -> CmdResult {
    list_class(host, ast, StoredClass::R, "r")
}

/// `ereturn list`.
pub fn ereturn_list(host: &mut dyn CmdHost, ast: &CommandAst) -> CmdResult {
    list_class(host, ast, StoredClass::E, "e")
}

/// The `return list` layout, which is part of the output contract: names right
/// aligned in 22 columns, scalars joined by ` =  ` and macros by ` : ` with the
/// value quoted (verified, `core_surface.log`).
fn list_class(
    host: &mut dyn CmdHost,
    ast: &CommandAst,
    class: StoredClass,
    sigil: &str,
) -> CmdResult {
    let sub = rest(ast).trim();
    if !sub.is_empty() && sub != "list" {
        return Err(err::unsupported(&format!("{sigil}eturn {sub}")));
    }
    let names: Vec<String> = host.stored_names(class);
    let mut scalars = Vec::new();
    let mut macros = Vec::new();
    for n in names {
        match host.stored(class, &n) {
            Some(ScalarValue::Num { value, .. }) => scalars.push((n, value)),
            Some(ScalarValue::Str { value }) => macros.push((n, value)),
            None => {}
        }
    }
    let mut out = Out::new();
    if !scalars.is_empty() {
        out.nl();
        out.txt("scalars:");
        out.nl();
        for (n, v) in &scalars {
            let key = format!("{sigil}({n})");
            out.spaces(22usize.saturating_sub(key.len()));
            out.txt(&key);
            out.txt(" =  ");
            // `%18.0g` then trim: `r(mean) = 21.2972972972973` keeps every
            // digit the double actually has, which is what makes a stored
            // result usable in the next command.
            out.res(stratum_core::fmt::fmt_g(*v, 18).trim());
            out.nl();
        }
    }
    if !macros.is_empty() {
        out.nl();
        out.txt("macros:");
        out.nl();
        for (n, v) in &macros {
            let key = format!("{sigil}({n})");
            out.spaces(22usize.saturating_sub(key.len()));
            out.txt(&key);
            out.txt(" : ");
            out.res(&format!("\"{v}\""));
            out.nl();
        }
    }
    host.emit(out.runs());
    Ok(CmdOutcome::text_only())
}

/// The r-class names `summarize` sets, in the order `return list` prints them.
///
/// Declared here rather than in `stratum-stats` because the ORDER is part of
/// the output contract (`05` §7.5) and this is the module that prints it; the
/// statistics crate supplies the values.
pub const SUMMARIZE_R: &[&str] = &["N", "sum_w", "mean", "Var", "sd", "min", "max", "sum"];

/// The e-class scalars `regress` sets, in `ereturn list` order (`05` §8.7).
pub const REGRESS_E: &[&str] = &[
    "N", "df_m", "df_r", "F", "r2", "rmse", "mss", "rss", "r2_a", "ll", "ll_0", "rank",
];

/// The e-class macros `regress` sets, in `ereturn list` order.
pub const REGRESS_E_MACROS: &[&str] = &[
    "cmdline",
    "title",
    "marginsok",
    "vce",
    "depvar",
    "cmd",
    "properties",
    "predict",
    "model",
    "estat_cmd",
];

/// Build a `StatRequest` without an AST, for callers that already have the
/// pieces (the command bar, `by:`-group replays).
#[must_use]
pub fn request(
    cmd: &'static str,
    cmdline: String,
    vars: Vec<u32>,
    sample: stratum_data::Sample,
) -> StatRequest {
    StatRequest {
        cmd,
        cmdline,
        vars,
        sample,
        options: Vec::new(),
    }
}

/// A `StataError` for a statistic that this build does not implement.
#[must_use]
pub fn not_implemented(cmd: &str) -> StataError {
    err::unsupported(cmd)
}
