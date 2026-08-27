//! `CmdHost::run_stat`'s back half: a [`StatRequest`] into `stratum-stats`
//! and a [`stratum_stats::StatResult`] back out.
//!
//! `cmd/estimation_glue.rs` owns the Stata-shaped front half — abbreviations,
//! varlist expansion, `if`/`in` to a `Sample`, option *rejection* — and hands
//! over a request with no parsing left in it. What remains here is exactly
//! three jobs, one per direction of the seam:
//!
//! 1. borrow the request's storage positions as [`VarRef`]s over the live
//!    frame (plus the per-command spec its options select);
//! 2. run the statistic;
//! 3. install the result: emit `classic_text(LINESIZE)` (with the blank line
//!    Stata's log puts before a command's block — the spacing *around* a
//!    block is the runtime's, per `stratum-stats`' own module header), apply
//!    the [U] 18.8 clear rule for the result's class, and copy the `ResultSet`
//!    into the live `r()`/`e()` singletons in the statistic's own insertion
//!    order — which is `ereturn list`'s print order (C31).
//!
//! Commands whose glue is not written yet answer **rc 10** through
//! [`crate::cmd::err::unsupported`]: "we are incomplete" must stay distinct
//! from a wrong answer (A16), and `dispatch` already reports rc 10 as
//! `STRATUM0010`.

use stratum_data::Frame;
use stratum_parse::StataError;
use stratum_proto::{StyleId, StyledRun, VarIdx};
use stratum_stats::{StatResult, VarRef, LINESIZE};

use crate::cmd::{err, CmdOutcome, CmdResult, StatRequest};
use crate::ctx::ExecCtx;
use crate::results::{Class, CommandClass, Matrix};

/// Run one resolved statistical command against the live frame.
pub(crate) fn run_stat(ctx: &mut ExecCtx<'_>, req: &StatRequest) -> CmdResult {
    // The columns, their metadata (labels and formats print in every table)
    // and the observation count all enter the output.
    for &v in &req.vars {
        ctx.access.note_read(VarIdx(v));
    }
    ctx.access.read_row_membership = true;
    ctx.access.read_var_layout = true;

    match req.cmd {
        "summarize" => summarize(ctx, req),
        "regress" => regress(ctx, req),
        // The front half validated these against the registry, so reaching
        // here means the command IS ours and this build has not written its
        // glue: exit 10, honestly, never a silently different number.
        other => Err(err::unsupported(other)),
    }
}

fn summarize(ctx: &mut ExecCtx<'_>, req: &StatRequest) -> CmdResult {
    let spec = stratum_stats::SummarizeSpec {
        detail: has(req, "detail"),
        meanonly: has(req, "meanonly"),
    };
    let result = {
        let frame = ctx.frames.current();
        let formats = format_strings(frame, &req.vars);
        let refs = var_refs(frame, &req.vars, &formats);
        stratum_stats::summarize(&refs, &req.sample, &spec)
    };
    install(ctx, &result)
}

/// What `vce()`/`robust` resolved to, before any [`VarRef`] borrows exist.
enum VceChoice {
    Ols,
    Robust,
    Cluster(VarIdx),
}

fn regress(ctx: &mut ExecCtx<'_>, req: &StatRequest) -> CmdResult {
    if req.vars.is_empty() {
        // Bare `regress` replays the last estimates in Stata; there is no
        // stored `RegressResult` on the context yet, so saying so beats
        // recomputing nothing.
        return Err(err::unsupported("regress without a variable list (replay)"));
    }
    // Options first, borrow-free: the spec below holds `VarRef`s into the
    // frame, and resolving a cluster variable mid-build would need a second
    // route into the same borrow.
    let mut noconstant = false;
    let mut beta = false;
    let mut level = ctx.settings.level;
    let mut vce = VceChoice::Ols;
    for (name, arg) in &req.options {
        match (name.as_str(), arg.as_deref()) {
            ("noconstant", _) => noconstant = true,
            ("beta", _) => beta = true,
            ("robust", _) => vce = VceChoice::Robust,
            ("level", Some(v)) => level = v.trim().parse().map_err(|_| err::invalid(v))?,
            ("level", None) => return Err(err::invalid("level")),
            ("vce", Some(arg)) => vce = parse_vce(ctx.frames.current(), arg)?,
            ("vce", None) => return Err(err::invalid("vce")),
            // The front half already rejected everything else.
            _ => {}
        }
    }

    let result = {
        let frame = ctx.frames.current();
        let formats = format_strings(frame, &req.vars);
        let refs = var_refs(frame, &req.vars, &formats);
        let cluster_fmt = match &vce {
            VceChoice::Cluster(idx) => {
                stratum_data::variable::format_string(&frame.vars()[idx.0 as usize].format)
            }
            _ => String::new(),
        };

        let mut spec =
            stratum_stats::RegressSpec::new(req.cmdline.clone(), refs[0], refs[1..].to_vec());
        spec.noconstant = noconstant;
        spec.beta = beta;
        spec.level = level;
        spec.vce = match vce {
            VceChoice::Ols => stratum_stats::VceSpec::Ols,
            VceChoice::Robust => stratum_stats::VceSpec::Robust,
            VceChoice::Cluster(idx) => {
                ctx.access.note_read(idx);
                stratum_stats::VceSpec::Cluster(var_ref(frame, idx, &cluster_fmt))
            }
        };
        stratum_stats::regress(&spec, &req.sample).map_err(stats_error)?
    };
    install(ctx, &result)
}

/// `vce(ols | robust | cluster clustvar)`. The vocabulary is closed — the
/// same one `stratum-stats`' own effect rows read.
fn parse_vce(frame: &Frame, arg: &str) -> Result<VceChoice, StataError> {
    let mut words = arg.split_whitespace();
    match (words.next(), words.next(), words.next()) {
        (Some("ols"), None, _) => Ok(VceChoice::Ols),
        (Some("robust"), None, _) => Ok(VceChoice::Robust),
        (Some("cluster"), Some(name), None) => frame
            .index_of(name)
            .map(VceChoice::Cluster)
            .ok_or_else(|| err::var_not_found(name)),
        _ => Err(err::invalid(arg)),
    }
}

/// Emit the classic block and copy the stored results into the live
/// singletons. One function so `summarize` and `regress` (and every statistic
/// after them) cannot disagree about the install order.
fn install(ctx: &mut ExecCtx<'_>, r: &impl StatResult) -> CmdResult {
    let runs = r.classic_text(LINESIZE);
    if !runs.is_empty() {
        // The single blank line between the command echo and its block —
        // classic_text deliberately excludes it (stratum-stats lib header).
        ctx.emit(&[StyledRun {
            text: "\n".to_owned(),
            style: StyleId::Text,
        }]);
        ctx.emit(&runs);
    }

    let (kind, set) = r.results();
    let (class, cmd_class) = match kind {
        stratum_stats::ResultKind::RClass => (Class::R, CommandClass::R),
        stratum_stats::ResultKind::EClass => (Class::E, CommandClass::E),
        stratum_stats::ResultKind::SClass => (Class::S, CommandClass::S),
    };
    // [U] 18.8: the class is REPLACED, never merged — and an e-class command
    // clears r() too.
    ctx.results.begin_command(cmd_class);
    {
        let dst = ctx.results.get_mut(class);
        for (k, v) in set.scalars() {
            dst.set_scalar(k, *v);
        }
        for (k, v) in set.macros() {
            dst.set_macro(k, v.clone());
        }
        for name in set.matrix_names() {
            let m = set.matrix(name).expect("name came from the same set");
            dst.set_matrix(
                name,
                Matrix {
                    rows: m.rows as u32,
                    cols: m.cols as u32,
                    rownames: m.rownames.clone(),
                    colnames: m.colnames.clone(),
                    // The collinearity colstripe stays in the payload; the
                    // stored twin has no slot for it until `matrix list`
                    // exists to print it.
                    data: m.data.clone(),
                },
            );
        }
    }
    if let Some(rows) = set.function("sample") {
        let mut bits = stratum_data::BitSet::new(rows.len());
        for i in 0..rows.len() {
            if rows.contains(i) {
                bits.set(i, true);
            }
        }
        ctx.results.set_sample(std::sync::Arc::new(bits));
    }
    Ok(CmdOutcome::one(r.payload()))
}

/// The display-format strings, pre-rendered because [`VarRef`] borrows them.
fn format_strings(frame: &Frame, vars: &[u32]) -> Vec<String> {
    vars.iter()
        .map(|&i| stratum_data::variable::format_string(&frame.vars()[i as usize].format))
        .collect()
}

/// Borrow the request's storage positions as [`VarRef`]s.
///
/// The positions came from `resolve_varlist` against this same frame inside
/// this same command, so indexing is infallible here.
fn var_refs<'f>(frame: &'f Frame, vars: &[u32], formats: &'f [String]) -> Vec<VarRef<'f>> {
    vars.iter()
        .zip(formats)
        .map(|(&i, fmt)| var_ref(frame, VarIdx(i), fmt))
        .collect()
}

fn var_ref<'f>(frame: &'f Frame, idx: VarIdx, fmt: &'f str) -> VarRef<'f> {
    let v = &frame.vars()[idx.0 as usize];
    VarRef {
        name: &v.name,
        label: &v.label,
        format: fmt,
        col: frame.col(idx).expect("a variable has a column"),
        value_label: v.value_label.as_deref().and_then(|l| frame.labels().get(l)),
    }
}

fn has(req: &StatRequest, opt: &str) -> bool {
    req.options.iter().any(|(n, _)| n == opt)
}

fn stats_error(e: stratum_stats::StatsError) -> StataError {
    StataError::new(u32::from(e.rc()), e.to_string())
}
