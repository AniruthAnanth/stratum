//! Control flow and the small session verbs: `foreach`, `forvalues`, `while`,
//! `if`/`else`, `continue`, `exit`, `error`, `version`, `assert`, `confirm`,
//! and the `capture` / `quietly` / `noisily` prefix semantics.
//!
//! # Loop bodies are spans, and re-expanded every pass
//!
//! `BlockCommand` carries its body as a [`Span`] into the **pre-expansion**
//! logical-line text, never as a parsed AST, because Stata re-expands the body
//! on every iteration — that is how `` `x' `` picks up the new loop value
//! (design 02 §6.2). [`CmdHost::run_body`] re-runs expansion, parse and
//! dispatch per pass; this module only decides what the loop variable is and
//! how many times to call it.
//!
//! # `continue` and `break` travel as return codes
//!
//! There is no second error channel. A `continue` unwinds through
//! [`CmdHost::run_body`] as a `StataError` carrying [`RC_CONTINUE`], which the
//! loop here recognises and swallows; anything else propagates. The codes are
//! outside Stata's range on purpose, so one can never collide with a real
//! `r(198)` a user could produce.

use stratum_core::Value;
use stratum_parse::ast::command::{BlockCommand, ForeachSource};
use stratum_parse::ast::CommandAst;
use stratum_parse::StataError;
use stratum_parse::VarlistMode;
use stratum_proto::VarIdx;

use super::{
    build_sample, err, resolve_varlist, rest, rest_span, slots, CmdHost, CmdOutcome, CmdResult, Out,
};

/// `continue` — abandon this pass of the innermost loop.
///
/// Above every Stata return code (the highest real one is in the 3000s), so it
/// can never be confused with a user-visible failure.
pub const RC_CONTINUE: u32 = 900_001;

/// `continue, break` — abandon the loop.
pub const RC_BREAK: u32 = 900_002;

/// `exit` with no code: stop this do-file, without an error.
pub const RC_EXIT: u32 = 900_003;

/// Is this a control-flow signal rather than a failure?
#[must_use]
pub fn is_signal(rc: u32) -> bool {
    matches!(rc, RC_CONTINUE | RC_BREAK | RC_EXIT)
}

// ---------------------------------------------------------------------------
// Block commands
// ---------------------------------------------------------------------------

/// Run one structural command.
///
/// # ESCALATION — THIS IS CURRENTLY A TWIN, AND ONE OF THE TWO MUST GO
///
/// The plan gives `cmd/control.rs` to W06c and `dispatch.rs` to W06a, and this
/// function was written to the note that dispatch would route `Command::Block`
/// here. **It does not.** `dispatch.rs` has its own `run_block`, and
/// `Command::Block(b) => run_block(ctx, set, b)` reaches that one, so the
/// shipping binary never calls this function. Two implementations of `forvalues`
/// counting and `foreach` expansion is exactly the divergence this codebase
/// treats as a defect, and it is reported in W06c's return rather than resolved
/// unilaterally, because the fix is an edit to a file W06c does not own.
///
/// **The one that should survive is `dispatch.rs`'s**, on evidence and not on
/// ownership: it can see `ExecCtx::cancelled()` — so a runaway `while` is
/// interruptible, which design 03 §9.2 requires and [`CmdHost`] cannot express
/// — and it manages `quiet_depth` across a whole `quietly { … }` body. This
/// copy can do neither. When that is settled, this function and its two helpers
/// come out, and `capture_result`, [`is_signal`], [`rc_value`] and the simple
/// verbs below stay: those ARE reached, through [`super::builtin`].
///
/// Until then it is kept, tested and correct rather than deleted: the two
/// agreed on every case `tests/cmd_surface.rs` drives, including the empty
/// range (`forvalues i = 5/1` runs zero times, not once), which is the case
/// that would have diverged silently.
///
/// # Errors
///
/// Whatever the body raised, or [`RC_BREAK`] escaping an outermost loop, which
/// the caller treats as ordinary completion.
pub fn run_block(host: &mut dyn CmdHost, block: &BlockCommand) -> CmdResult {
    match block {
        BlockCommand::Foreach {
            loopvar,
            source,
            body,
        } => {
            let values = foreach_values(host, source)?;
            for v in values {
                host.set_local(loopvar, &v);
                if let Some(out) = one_pass(host, *body)? {
                    return Ok(out);
                }
            }
            Ok(CmdOutcome::text_only())
        }
        BlockCommand::Forvalues {
            loopvar,
            range,
            body,
        } => {
            let step = range.step.unwrap_or(1.0);
            if step == 0.0 {
                return Err(err::invalid("range"));
            }
            // Counted rather than accumulated, so a fractional step cannot
            // drift: `forvalues x = 0(0.1)1` runs 11 times on every platform,
            // where `v += step` would land on 0.9999999999999999 and run 10.
            let spans = (range.to - range.from) / step;
            // A range that runs the wrong way is EMPTY, not one pass.
            // `forvalues i = 5/1` and `forvalues i = 1(-1)5` both do nothing in
            // Stata, and `0..=n` with `n` clamped to zero would run the body
            // once with `i = 5` — the idiom `forvalues i = 1/\`n'` on an empty
            // `n` is exactly how a do-file says "skip this".
            if !spans.is_finite() || spans < 0.0 {
                return Ok(CmdOutcome::text_only());
            }
            let n = spans.floor() as u64;
            for i in 0..=n {
                let v = range.from + step * i as f64;
                host.set_local(loopvar, &stratum_core::fmt::fmt_macro(v));
                if let Some(out) = one_pass(host, *body)? {
                    return Ok(out);
                }
            }
            Ok(CmdOutcome::text_only())
        }
        BlockCommand::While { cond, body } => {
            // Bounded only by the user's own condition, exactly like Stata; the
            // cancellation ladder (design 03 §9.2) is what stops a runaway
            // loop, and it lives in the host's `run_body`.
            loop {
                if !host.eval_scalar(cond)?.truthy() {
                    return Ok(CmdOutcome::text_only());
                }
                if let Some(out) = one_pass(host, *body)? {
                    return Ok(out);
                }
            }
        }
        BlockCommand::IfElse { arms } => {
            for (cond, body) in arms {
                let take = match cond {
                    Some(e) => host.eval_scalar(e)?.truthy(),
                    None => true,
                };
                if take {
                    host.run_body(*body)?;
                    return Ok(CmdOutcome::text_only());
                }
            }
            Ok(CmdOutcome::text_only())
        }
        BlockCommand::Capture { body } => {
            let rc = match host.run_body(*body) {
                Ok(()) => 0,
                Err(e) if is_signal(e.rc) => return Err(e),
                Err(e) => e.rc,
            };
            host.set_last_rc(rc);
            Ok(CmdOutcome::text_only())
        }
        BlockCommand::Quietly { body } | BlockCommand::Noisily { body } => {
            host.run_body(*body)?;
            Ok(CmdOutcome::text_only())
        }
        BlockCommand::Anonymous { body } => {
            host.run_body(*body)?;
            Ok(CmdOutcome::text_only())
        }
        BlockCommand::Program { .. } | BlockCommand::Input { .. } => {
            Err(err::unsupported("this block form in cmd::control"))
        }
        BlockCommand::Mata { .. } => Err(err::unsupported("mata")),
        BlockCommand::Python { .. } => Err(err::unsupported("python")),
    }
}

/// One loop pass. `Ok(Some(_))` means the loop was broken out of.
fn one_pass(
    host: &mut dyn CmdHost,
    body: stratum_proto::Span,
) -> Result<Option<CmdOutcome>, StataError> {
    match host.run_body(body) {
        Ok(()) => Ok(None),
        Err(e) if e.rc == RC_CONTINUE => Ok(None),
        Err(e) if e.rc == RC_BREAK => Ok(Some(CmdOutcome::text_only())),
        Err(e) => Err(e),
    }
}

/// The values a `foreach` iterates over, as strings.
fn foreach_values(
    host: &mut dyn CmdHost,
    source: &ForeachSource,
) -> Result<Vec<String>, StataError> {
    Ok(match source {
        ForeachSource::In(raw) => raw.text.split_whitespace().map(str::to_owned).collect(),
        ForeachSource::OfLocal(name) => host
            .get_macro(false, name)
            .unwrap_or_default()
            .split_whitespace()
            .map(str::to_owned)
            .collect(),
        ForeachSource::OfGlobal(name) => host
            .get_macro(true, name)
            .unwrap_or_default()
            .split_whitespace()
            .map(str::to_owned)
            .collect(),
        ForeachSource::OfVarlist(vl) => {
            let idxs = resolve_varlist(host, Some(vl), VarlistMode::Existing, false)?;
            let frame = host.frames().current();
            idxs.iter()
                .filter_map(|i| frame.var(VarIdx(*i)).map(|v| v.name.to_string()))
                .collect()
        }
        ForeachSource::OfNewlist(vl) => {
            // A newlist names variables that must NOT exist yet, so it is read
            // straight off the syntax rather than resolved.
            vl.items
                .iter()
                .filter_map(|i| match &i.kind {
                    stratum_parse::ast::varlist::VarItemKind::Single(a) => {
                        Some(a.base.as_text().to_owned())
                    }
                    stratum_parse::ast::varlist::VarItemKind::Interact { .. } => None,
                })
                .collect()
        }
        ForeachSource::OfNumlist(nl) => {
            // `expand`, not a hand-rolled loop: the numlist keeps its ranges
            // unexpanded precisely so `1/10000000` costs a multiply to COUNT,
            // and `foreach` is the one caller that genuinely has to
            // materialise. Everything else asks `NumList::count` first.
            let mut v = Vec::with_capacity(nl.count().min(1 << 20) as usize);
            nl.expand()
                .into_iter()
                .for_each(|x| v.push(stratum_core::fmt::fmt_macro(x)));
            v
        }
    })
}

// ---------------------------------------------------------------------------
// Prefix semantics, for `dispatch.rs` to call
// ---------------------------------------------------------------------------

/// `capture` around one already-parsed command: swallow the failure, record
/// `_rc`, and let the do-file continue.
///
/// `capture summarize nosuchvar` leaves `_rc == 111` and no diagnostic;
/// `capture summarize price` leaves `_rc == 0` (verified, `errors.log`).
///
/// # Errors
///
/// Only a control-flow signal ([`is_signal`]) escapes; those are not failures
/// and `capture` must not eat them.
pub fn capture_result(host: &mut dyn CmdHost, r: CmdResult) -> CmdResult {
    match r {
        Ok(o) => {
            host.set_last_rc(0);
            Ok(o)
        }
        Err(e) if is_signal(e.rc) => Err(e),
        Err(e) => {
            host.set_last_rc(e.rc);
            Ok(CmdOutcome::text_only())
        }
    }
}

/// `capture` with nothing after it.
///
/// **The prefix form never reaches here.** `capture summarize price` parses as
/// `Prefix::Capture` on a `summarize` command (`cmdsig.rs` marks `capture`
/// `PFX`), and `dispatch.rs` consumes the prefix and calls
/// [`capture_result`]. A `Command::Known("capture")` therefore only exists for
/// the degenerate bare word, which Stata accepts and which resets `_rc` —
/// there was a command, it did nothing, and nothing failed.
pub fn capture(host: &mut dyn CmdHost, _ast: &CommandAst) -> CmdResult {
    host.set_last_rc(0);
    Ok(CmdOutcome::text_only())
}

/// `quietly` with nothing after it. See [`capture`]; the prefix form is
/// `Prefix::Quietly` and dispatch owns it.
pub fn quietly(_host: &mut dyn CmdHost, _ast: &CommandAst) -> CmdResult {
    Ok(CmdOutcome::text_only())
}

/// `noisily` with nothing after it. See [`capture`].
pub fn noisily(_host: &mut dyn CmdHost, _ast: &CommandAst) -> CmdResult {
    Ok(CmdOutcome::text_only())
}

// ---------------------------------------------------------------------------
// Simple verbs
// ---------------------------------------------------------------------------

/// `version [#]`. Accepted; v1 runs one set of semantics.
pub fn version(host: &mut dyn CmdHost, ast: &CommandAst) -> CmdResult {
    let text = rest(ast).trim();
    if text.is_empty() {
        let mut out = Out::new();
        out.txt("version 18.5");
        out.nl();
        host.emit(out.runs());
    }
    Ok(CmdOutcome::text_only())
}

/// `exit [#]` — stop the do-file. A nonzero code is a failure; a bare `exit`
/// is not.
pub fn exit(_host: &mut dyn CmdHost, ast: &CommandAst) -> CmdResult {
    let text = rest(ast).trim();
    let code: u32 = text
        .split_whitespace()
        .next()
        .and_then(|w| w.parse().ok())
        .unwrap_or(0);
    Err(if code == 0 {
        StataError::new(RC_EXIT, "")
    } else {
        StataError::new(code, "")
    })
}

/// `error #` — raise a return code deliberately.
pub fn error(_host: &mut dyn CmdHost, ast: &CommandAst) -> CmdResult {
    let text = rest(ast).trim();
    let code: u32 = text
        .split_whitespace()
        .next()
        .and_then(|w| w.parse().ok())
        .unwrap_or(1);
    Err(StataError::new(code, message_for(code)))
}

/// `continue [, break]`.
pub fn r#continue(_host: &mut dyn CmdHost, ast: &CommandAst) -> CmdResult {
    let brk = rest(ast).contains("break");
    Err(StataError::new(
        if brk { RC_BREAK } else { RC_CONTINUE },
        "",
    ))
}

/// `assert exp [if] [in]`.
///
/// Prints `74 contradictions in 74 observations` and then fails with
/// `assertion is false`, r(9) — both verified in `errors.log`.
pub fn assert(host: &mut dyn CmdHost, ast: &CommandAst) -> CmdResult {
    let s = slots(ast).ok_or_else(|| err::invalid("assert"))?;
    // `assert price > 100000` puts the expression in `rest`, not in `if_`:
    // the universal grammar has no bare-expression slot.
    let text = rest(ast).trim();
    let cond = if text.is_empty() {
        s.if_.clone().ok_or_else(|| err::invalid("assert"))?
    } else {
        parse_condition(text)
    };
    let sample = build_sample(host, s.if_.as_ref(), s.in_.as_ref())?;
    let n = sample.len();
    let mut bad = 0u64;
    let mut buf = Vec::with_capacity(stratum_data::CHUNK_ROWS);
    for run in sample.runs() {
        let mut row = run.start;
        let end = run.start + run.len;
        while row < end {
            let len =
                usize::try_from((end - row).min(stratum_data::CHUNK_ROWS as u64)).expect("bounded");
            buf.clear();
            host.eval_num_rows(&cond, row, len, &mut buf)?;
            bad += buf.iter().filter(|v| **v == 0.0).count() as u64;
            row += len as u64;
        }
    }
    if bad == 0 {
        return Ok(CmdOutcome::text_only());
    }
    let mut out = Out::new();
    out.res(&bad.to_string());
    out.txt(" contradiction");
    out.txt(if bad == 1 { "" } else { "s" });
    out.txt(" in ");
    out.res(&n.to_string());
    out.txt(" observation");
    out.txt(if n == 1 { "" } else { "s" });
    out.nl();
    host.emit(out.runs());
    Err(err::assertion_false().at(ast.span))
}

/// `confirm [new|numeric|string] variable name` / `confirm file name`.
pub fn confirm(host: &mut dyn CmdHost, ast: &CommandAst) -> CmdResult {
    let text = rest(ast).trim();
    let span = rest_span(ast);
    let mut words = text.split_whitespace().peekable();
    let mut want_new = false;
    let mut want_numeric = false;
    let mut want_string = false;
    let mut kind = "";
    while let Some(w) = words.peek() {
        match *w {
            "new" => want_new = true,
            "numeric" => want_numeric = true,
            "string" | "str" => want_string = true,
            "variable" | "var" | "v" => {
                kind = "variable";
                words.next();
                break;
            }
            "file" => {
                kind = "file";
                words.next();
                break;
            }
            _ => break,
        }
        words.next();
    }
    let name = words.next().unwrap_or("");
    match kind {
        "file" => {
            // Existence is the host's question; `confirm file` is the one place
            // a command asks it directly.
            let path = camino::Utf8Path::new(name);
            if host.file_exists(path) {
                Ok(CmdOutcome::text_only())
            } else {
                Err(err::file_not_found(name).at(span))
            }
        }
        _ => {
            let frame = host.frames().current();
            let found = frame.index_of(name);
            match (want_new, found) {
                (true, Some(_)) => Err(err::already_defined(name).at(span)),
                (true, None) => Ok(CmdOutcome::text_only()),
                (false, None) => Err(err::var_not_found(name).at(span)),
                (false, Some(idx)) => {
                    let is_str = matches!(
                        frame.var(idx).map(|v| v.ty),
                        Some(stratum_data::StorageType::Str { .. })
                            | Some(stratum_data::StorageType::StrL)
                    );
                    if want_numeric && is_str {
                        Err(err::found_where_expected(name, "numeric variable").at(span))
                    } else if want_string && !is_str {
                        Err(err::found_where_expected(name, "string variable").at(span))
                    } else {
                        Ok(CmdOutcome::text_only())
                    }
                }
            }
        }
    }
}

fn parse_condition(text: &str) -> stratum_parse::ast::expr::Expr {
    let toks = stratum_parse::tokens(text, stratum_parse::lex::LexMode::Expanded);
    let mut cur =
        stratum_parse::parse::Cursor::new(text, &toks, stratum_parse::parse::ParseMode::Execute);
    stratum_parse::parse::parse_expr(&mut cur)
}

/// The message Stata prints for a bare `error #`.
///
/// Only the codes v1 can raise are listed; anything else gets the generic
/// wording rather than an invented one, because a wrong error message in a
/// differentially-tested product is worse than a vague one.
fn message_for(rc: u32) -> String {
    match rc {
        1 => "interrupted".to_owned(),
        4 => "no data in memory".to_owned(),
        7 => "invalid type".to_owned(),
        9 => "assertion is false".to_owned(),
        100 => "varlist required".to_owned(),
        101 => "varlist not allowed".to_owned(),
        102 => "too few variables specified".to_owned(),
        103 => "too many variables specified".to_owned(),
        109 => "type mismatch".to_owned(),
        110 => "already defined".to_owned(),
        111 => "not found".to_owned(),
        198 => "invalid syntax".to_owned(),
        199 => "unrecognized command".to_owned(),
        601 => "file not found".to_owned(),
        602 => "file already exists".to_owned(),
        2000 => "no observations".to_owned(),
        _ => String::new(),
    }
}

/// `_rc` as a [`Value`], for `eval.rs` to answer `_rc` with.
#[must_use]
pub fn rc_value(host: &dyn CmdHost) -> Value {
    Value::Real(f64::from(host.last_rc()))
}
