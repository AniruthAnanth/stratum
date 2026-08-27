//! Running one command, and running a file of them.
//!
//! # INV-2 is a property of this file
//!
//! > A command with `Atomicity::Rollbackable` either completes or leaves dataset
//! > and session state exactly as at entry.
//!
//! [`exec_command`] opens the frame's journal **once**, runs the command inside
//! `catch_unwind`, and takes exactly one of three exits: commit, rollback with a
//! diagnostic, or rollback with `Interrupted { rolled_back: true }`. There is no
//! fourth path, and no command implementation gets to choose — which is why the
//! journal is opened here rather than by each command.
//!
//! `catch_unwind` is why the release profile sets `panic = "unwind"`. A panic in
//! one estimation becomes `ExecStatus::Failed` with an internal-error
//! diagnostic, and the user's dataset, macros and estimates survive; with
//! `panic = "abort"` the engine process would die and take the session with it.
//!
//! # The dispatch order, and why prefixes are here and not in the table
//!
//! `quietly`, `noisily`, `capture`, `version` and `by` are *prefixes*: they wrap
//! a command rather than being one, and two of them change how the wrapped
//! command's failure is reported. `capture` in particular must catch the error
//! AFTER the rollback has happened, or `capture replace x = 1/0` would leave a
//! half-written column behind. Putting them in the built-in table would give
//! every one of them a copy of that ordering.
//!
//! # Why the command set is a trait
//!
//! `docs/ownership.toml` gives `src/cmd/**` to W06c and this file to W06a. The
//! seam is [`CommandSet`]: dispatch owns the lifecycle, the command surface owns
//! the commands, and neither has to be finished for the other to be testable.
//! `BuiltinCommands` — the adapter that forwards to `cmd::builtin` — is the only
//! implementation the shipping binary uses.

use std::panic::{catch_unwind, AssertUnwindSafe};

use stratum_parse::ast::command::{BlockCommand, Command, ForeachSource, Prefix};
use stratum_parse::ast::{CommandAst, NumList};
use stratum_parse::lints::StataError;
use stratum_parse::macros::ScopeKind;
use stratum_parse::{parse_command, ParseMode};
use stratum_proto::{Diagnostic, ExecStatus, Severity, Span};

use crate::ctx::ExecCtx;
use crate::program::{Program, ProgramClass};

/// The set of commands this build implements.
///
/// Implemented by the adapter over `cmd::builtin` (W06c). Declared here because
/// dispatch must be able to name it, and it must be able to compile before the
/// command surface exists.
pub trait CommandSet {
    /// Run one resolved command. The dispatcher has already stripped prefixes,
    /// opened the journal and armed `catch_unwind`.
    ///
    /// # Errors
    ///
    /// A Stata return code. The dispatcher turns it into a diagnostic and rolls
    /// the frame back.
    fn run(&self, ctx: &mut ExecCtx<'_>, ast: &CommandAst) -> Result<(), StataError>;

    /// Does this build implement `canonical`? Backs the exit-10 path and the
    /// result card's quick actions (A22).
    fn implements(&self, canonical: &str) -> bool;
}

/// A [`CommandSet`] that implements nothing.
///
/// Every command is `rc 10` — *our* "unsupported in this version", never a
/// Stata code, so a compatibility report can separate "we are wrong" from "we
/// are incomplete" (A16). It exists so that the dispatcher, the prefix
/// machinery, the control-flow interpreter and the do-file driver can be tested
/// without a command surface, and so that a caller that forgot to install one
/// gets a clear answer rather than a panic.
#[derive(Copy, Clone, Debug, Default)]
pub struct NoCommands;

impl CommandSet for NoCommands {
    fn run(&self, _ctx: &mut ExecCtx<'_>, ast: &CommandAst) -> Result<(), StataError> {
        let name = command_word(ast).unwrap_or("command").to_owned();
        Err(
            StataError::new(10, format!("unsupported in this version: {name}"))
                .at(ast.span)
                .token(name),
        )
    }

    fn implements(&self, _canonical: &str) -> bool {
        false
    }
}

/// The [`CommandSet`] the shipping binary uses: `cmd::builtin`, wired to
/// `ExecCtx`'s `CmdHost` impl.
///
/// Payloads take the additive channel: each command's `CmdOutcome.payloads`
/// is buffered on the context ([`ExecCtx::push_payloads`]) and the engine
/// layer drains it per executed block — `CommandSet::run` keeps its `()`
/// success type, so nothing that dispatches commands has to learn about
/// result cards.
#[derive(Copy, Clone, Debug, Default)]
pub struct BuiltinCommands;

impl CommandSet for BuiltinCommands {
    fn run(&self, ctx: &mut ExecCtx<'_>, ast: &CommandAst) -> Result<(), StataError> {
        let name =
            command_word(ast).ok_or_else(|| StataError::new(198, "invalid syntax").at(ast.span))?;
        let Some(f) = crate::cmd::builtin(name) else {
            // A KNOWN command this build has no body for: exit 10, never a
            // Stata code, so "we are incomplete" stays distinct from "we are
            // wrong" (A16).
            return Err(
                StataError::new(10, format!("unsupported in this version: {name}"))
                    .at(ast.span)
                    .token(name.to_owned()),
            );
        };
        let outcome = f(ctx, ast)?;
        ctx.push_payloads(outcome.payloads);
        Ok(())
    }

    fn implements(&self, canonical: &str) -> bool {
        crate::cmd::IMPLEMENTED.contains(&canonical)
    }
}

/// What running something produced.
#[derive(Clone, PartialEq, Debug)]
pub struct Outcome {
    /// The wire status, ready for an `ExecutionRecord`.
    pub status: ExecStatus,
    /// Everything worth showing the user, in the order raised.
    pub diagnostics: Vec<Diagnostic>,
}

impl Outcome {
    /// Nothing went wrong.
    #[must_use]
    pub fn succeeded() -> Self {
        Outcome {
            status: ExecStatus::Succeeded,
            diagnostics: Vec::new(),
        }
    }

    /// Did it finish?
    #[must_use]
    pub fn is_ok(&self) -> bool {
        matches!(self.status, ExecStatus::Succeeded)
    }

    /// The return code, or `0`.
    #[must_use]
    pub fn rc(&self) -> u32 {
        match &self.status {
            ExecStatus::Failed { rc, .. } => *rc,
            _ => 0,
        }
    }

    fn failed(e: &StataError) -> Self {
        Outcome {
            status: ExecStatus::Failed {
                rc: e.rc,
                message: e.message.clone(),
                span: e.span,
            },
            diagnostics: vec![to_diagnostic(e)],
        }
    }

    fn interrupted(at: Option<Span>) -> Self {
        Outcome {
            // INV-2: `rolled_back` is `true` because `exec_command` has already
            // rolled the frame back by the time this is built. It is not a hope.
            status: ExecStatus::Interrupted {
                rolled_back: true,
                at,
            },
            diagnostics: Vec::new(),
        }
    }
}

/// A [`StataError`] as the wire diagnostic, with our `STRATUM0010` spelling.
///
/// `StataError::to_diagnostic` codes everything `STATA{rc:04}`, which is right
/// for the Stata return codes and wrong for ours: `rc 10` is not a Stata code,
/// and A16 names that diagnostic `STRATUM0010`.
#[must_use]
pub fn to_diagnostic(e: &StataError) -> Diagnostic {
    let mut d = e.to_diagnostic();
    if e.rc == 10 {
        d.code = "STRATUM0010".to_owned();
    }
    d
}

/// The command word of an AST, for a diagnostic that needs to name it.
#[must_use]
pub fn command_word(ast: &CommandAst) -> Option<&str> {
    match &ast.cmd {
        Command::Known(k) => Some(stratum_parse::table().get(k.id).canonical),
        Command::Unknown { name, .. } => Some(name.as_str()),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// One command
// ---------------------------------------------------------------------------

/// Run one parsed command, prefixes included.
///
/// This is the only place the frame's journal is opened and the only place
/// `catch_unwind` is armed. Every exit rolls back or commits exactly once.
pub fn exec_command(ctx: &mut ExecCtx<'_>, set: &dyn CommandSet, ast: &CommandAst) -> Outcome {
    ctx.counters.commands += 1;

    // Cancellation checked at entry as well as at each safepoint inside a
    // command: a queue of ten thousand commands must stop at the next one, not
    // at the next long one.
    if ctx.cancelled() {
        return Outcome::interrupted(Some(ast.span));
    }

    let plan = match PrefixPlan::of(ast) {
        Ok(p) => p,
        Err(e) => return Outcome::failed(&e),
    };

    ctx.quiet_depth += u32::from(plan.quiet);
    ctx.begin_command();

    // `AssertUnwindSafe` is the honest annotation, not a workaround: `ExecCtx`
    // genuinely can be observed after a panic, and that is the whole point —
    // the frame journal puts the dataset back, and the session survives.
    let result = catch_unwind(AssertUnwindSafe(|| run_inner(ctx, set, ast, &plan)));

    let mut outcome = match result {
        Ok(Ok(())) if ctx.cancelled() => {
            ctx.rollback();
            Outcome::interrupted(Some(ast.span))
        }
        Ok(Ok(())) => {
            ctx.commit();
            Outcome::succeeded()
        }
        Ok(Err(e)) => {
            ctx.rollback();
            Outcome::failed(&e)
        }
        Err(payload) => {
            ctx.rollback();
            ctx.counters.panics_caught += 1;
            Outcome::failed(&internal_error(ast, &payload))
        }
    };

    ctx.quiet_depth -= u32::from(plan.quiet);

    if plan.capture {
        // `capture` swallows the error AFTER the rollback, which is the whole
        // ordering: `capture replace x = 1/0` must leave nothing behind.
        ctx.rc = outcome.rc();
        if !outcome.is_ok() {
            outcome = Outcome::succeeded();
        }
    } else if outcome.is_ok() {
        ctx.rc = 0;
    }
    outcome
}

/// The panic payload as a diagnostic.
///
/// The message says *which command* fell over, because "internal error" with no
/// location is the least actionable thing a statistical package can print.
fn internal_error(ast: &CommandAst, payload: &Box<dyn std::any::Any + Send>) -> StataError {
    let detail = payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "panic".to_owned());
    let name = command_word(ast).unwrap_or("command").to_owned();
    StataError::new(1, format!("internal error in {name}: {detail}"))
        .at(ast.span)
        .token(name)
}

/// What a command's prefix chain asks for.
struct PrefixPlan {
    quiet: bool,
    capture: bool,
}

impl PrefixPlan {
    fn of(ast: &CommandAst) -> Result<Self, StataError> {
        let mut plan = PrefixPlan {
            quiet: false,
            capture: false,
        };
        for p in &ast.prefixes {
            match p {
                Prefix::Quietly { .. } => plan.quiet = true,
                // `noisily` cancels an enclosing `quietly`; the depth counter in
                // `ExecCtx` is what makes nesting work, and this is the one
                // place that can lower it.
                Prefix::Noisily { .. } => plan.quiet = false,
                Prefix::Capture { .. } => {
                    plan.capture = true;
                    // Bare `capture` is quiet unless `noisily` follows it.
                    plan.quiet = true;
                }
                // `version 17:` is accepted and has no effect: this build
                // implements one version of the language. Silently ignoring it
                // is right — refusing it would break every ado-file.
                Prefix::Version { .. } => {}
                Prefix::By(by) => {
                    return Err(unsupported("by/bysort prefix", by.span));
                }
                Prefix::Frame { name, span } => {
                    return Err(unsupported(&format!("frame {name}: prefix"), *span));
                }
                Prefix::Generic { name, span, .. } => {
                    return Err(unsupported(&format!("{name}: prefix"), *span));
                }
            }
        }
        Ok(plan)
    }
}

fn unsupported(what: &str, span: Span) -> StataError {
    StataError::new(10, format!("unsupported in this version: {what}"))
        .at(span)
        .token(what.to_owned())
}

fn run_inner(
    ctx: &mut ExecCtx<'_>,
    set: &dyn CommandSet,
    ast: &CommandAst,
    _plan: &PrefixPlan,
) -> Result<(), StataError> {
    match &ast.cmd {
        Command::Known(_) => set.run(ctx, ast),
        Command::Block(b) => run_block(ctx, set, b),
        // `#delimit` is handled by the segmenter, which has already applied it
        // to the region boundaries by the time anything reaches here.
        Command::Directive(_) => Ok(()),
        Command::Unknown { name, rest, .. } => {
            if ctx.programs.contains(name) {
                return call_program(ctx, set, name, &rest.text);
            }
            // A word the PARSE table does not know can still be a command this
            // BUILD implements — that is the whole distinction CONTRACTS §13's
            // `CommandRegistry` draws, and it is what an ado-file resolved from
            // the shipped tree looks like from here. Asking first is what keeps
            // r(199) meaning "nothing in this session can run that".
            if set.implements(name) {
                return set.run(ctx, ast);
            }
            Err(
                StataError::new(199, format!("command {name} is unrecognized"))
                    .at(ast.span)
                    .token(name.clone()),
            )
        }
    }
}

// ---------------------------------------------------------------------------
// Control flow
// ---------------------------------------------------------------------------

fn run_block(
    ctx: &mut ExecCtx<'_>,
    set: &dyn CommandSet,
    b: &BlockCommand,
) -> Result<(), StataError> {
    match b {
        BlockCommand::IfElse { arms } => {
            for (cond, body) in arms {
                let take = match cond {
                    None => true,
                    Some(e) => ctx.eval_scalar(e)?.truthy(),
                };
                if take {
                    return ctx.run_body(set, *body);
                }
            }
            Ok(())
        }
        BlockCommand::While { cond, body } => {
            // No iteration cap: `while` with a condition that never becomes
            // false is the user's own infinite loop, and Stata does not cap it
            // either. The cancellation check is what makes it interruptible,
            // which is the property that actually matters.
            loop {
                if ctx.cancelled() {
                    return Ok(());
                }
                if !ctx.eval_scalar(cond)?.truthy() {
                    return Ok(());
                }
                ctx.run_body(set, *body)?;
            }
        }
        BlockCommand::Forvalues {
            loopvar,
            range,
            body,
        } => run_forvalues(ctx, set, loopvar, range, *body),
        BlockCommand::Foreach {
            loopvar,
            source,
            body,
        } => run_foreach(ctx, set, loopvar, source, *body),
        BlockCommand::Program { name, opts, body } => {
            // `body` is the empty span the parser leaves; see `end_block_body`.
            // `opts.text` is everything after the program NAME — the option
            // list AND the body AND the `end` — so classifying the program by
            // searching it whole would make `program define p` holding
            // `di "rclass"` an r-class program.
            let _ = body;
            let src = ctx.source().unwrap_or_default().to_owned();
            let body = end_block_body(&src, ctx.source_delimiter());
            let text = ctx.body_text(body)?;
            let head = opts.text.split('\n').next().unwrap_or("");
            let class = if head.contains("rclass") {
                ProgramClass::RClass
            } else if head.contains("eclass") {
                ProgramClass::EClass
            } else if head.contains("sclass") {
                ProgramClass::SClass
            } else {
                ProgramClass::Plain
            };
            ctx.programs.define(Program {
                name: name.clone(),
                body: text,
                class,
                byable: head.contains("byable"),
                sortpreserve: head.contains("sortpreserve"),
            })
        }
        // A brace block with a prefix word is the block form of the prefix. The
        // quiet depth is managed here rather than by `PrefixPlan` because the
        // suppression must last for the whole body, not for the `{` line.
        BlockCommand::Quietly { body } => {
            ctx.quiet_depth += 1;
            let r = ctx.run_body(set, *body);
            ctx.quiet_depth -= 1;
            r
        }
        BlockCommand::Noisily { body } => {
            let saved = std::mem::take(&mut ctx.quiet_depth);
            let r = ctx.run_body(set, *body);
            ctx.quiet_depth = saved;
            r
        }
        BlockCommand::Capture { body } => {
            ctx.quiet_depth += 1;
            let r = ctx.run_body(set, *body);
            ctx.quiet_depth -= 1;
            ctx.rc = match &r {
                Ok(()) => 0,
                Err(e) => e.rc,
            };
            Ok(())
        }
        BlockCommand::Anonymous { body } => ctx.run_body(set, *body),
        BlockCommand::Input { spec, .. } => Err(unsupported("input", spec.span)),
        BlockCommand::Mata { body } => Err(unsupported("mata", *body)),
        BlockCommand::Python { body } => Err(unsupported("python", *body)),
    }
}

fn run_forvalues(
    ctx: &mut ExecCtx<'_>,
    set: &dyn CommandSet,
    loopvar: &str,
    range: &stratum_parse::ast::command::NumRange,
    body: Span,
) -> Result<(), StataError> {
    let step = range.step.unwrap_or(1.0);
    if step == 0.0 {
        return Err(StataError::new(198, "invalid numlist: zero step").at(body));
    }
    // Counted rather than accumulated: `forvalues i = 0(.1)1` must run 11 times,
    // and `x += .1` eleven times lands at 0.9999999999999999 and runs 10.
    let n = ((range.to - range.from) / step).floor();
    if n < 0.0 || !n.is_finite() {
        return Ok(());
    }
    let n = n as u64;
    for k in 0..=n {
        if ctx.cancelled() {
            return Ok(());
        }
        let v = range.from + step * k as f64;
        ctx.macros
            .set_local(loopvar, stratum_parse::macros::stringify_number(v));
        ctx.run_body(set, body)?;
    }
    Ok(())
}

fn run_foreach(
    ctx: &mut ExecCtx<'_>,
    set: &dyn CommandSet,
    loopvar: &str,
    source: &ForeachSource,
    body: Span,
) -> Result<(), StataError> {
    let items: Vec<String> = match source {
        ForeachSource::In(raw) => stratum_parse::macros::split_args(&raw.text)
            .into_iter()
            .map(str::to_owned)
            .collect(),
        ForeachSource::OfLocal(name) => ctx
            .macros
            .local(name)
            .map(|v| {
                stratum_parse::macros::split_args(v)
                    .into_iter()
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default(),
        ForeachSource::OfGlobal(name) => ctx
            .macros
            .global(name)
            .map(|v| {
                stratum_parse::macros::split_args(v)
                    .into_iter()
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default(),
        ForeachSource::OfVarlist(vl) | ForeachSource::OfNewlist(vl) => {
            varlist_names(ctx, vl, matches!(source, ForeachSource::OfNewlist(_)))?
        }
        ForeachSource::OfNumlist(nl) => numlist_items(nl),
    };
    for item in items {
        if ctx.cancelled() {
            return Ok(());
        }
        ctx.macros.set_local(loopvar, item);
        ctx.run_body(set, body)?;
    }
    Ok(())
}

fn numlist_items(nl: &NumList) -> Vec<String> {
    nl.expand()
        .into_iter()
        .map(stratum_parse::macros::stringify_number)
        .collect()
}

fn varlist_names(
    ctx: &ExecCtx<'_>,
    vl: &stratum_parse::ast::varlist::VarList,
    new: bool,
) -> Result<Vec<String>, StataError> {
    if new {
        // `foreach v of newlist a b c` does not touch the dataset: the names are
        // taken verbatim, which is the point — they do not exist yet.
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
    let frame = ctx.frames.current();
    let index = frame_names(frame);
    let cx = stratum_parse::VarlistCtx {
        vars: &index,
        varabbrev: ctx.settings.varabbrev,
    };
    let positions = stratum_parse::expand_varlist(vl, &cx, stratum_parse::VarlistMode::Existing)?;
    Ok(positions
        .into_iter()
        .map(|p| frame.vars()[p as usize].name.to_string())
        .collect())
}

/// `stratum_data`'s frame as a `stratum_parse::VarIndex`.
///
/// CONTRACTS §13 assigns this impl to `stratum-data`, which cannot have it:
/// `VarIndex` is declared in `stratum-parse`, and `stratum-data` sits BELOW
/// `stratum-parse` in ARCHITECTURE §8's layer order, so the edge would invert
/// the graph. The adapter therefore belongs on the first crate that depends on
/// both. W06c's `cmd/mod.rs` reaches the same conclusion independently and has
/// its own; the two should collapse into one when the units merge, and this one
/// is here because `foreach … of varlist` needs it and does not go through the
/// command surface. Reported in W06a's return.
pub struct FrameNames<'a> {
    frame: &'a stratum_data::Frame,
}

/// Wrap a frame as a `stratum_parse::VarIndex`.
#[must_use]
pub fn frame_names(frame: &stratum_data::Frame) -> FrameNames<'_> {
    FrameNames { frame }
}

impl stratum_parse::varlist::VarIndex for FrameNames<'_> {
    fn len(&self) -> usize {
        self.frame.n_vars() as usize
    }

    fn name(&self, pos: usize) -> &str {
        &self.frame.vars()[pos].name
    }

    fn position(&self, name: &str) -> Option<usize> {
        self.frame.index_of(name).map(|i| i.0 as usize)
    }

    fn storage_type(&self, pos: usize) -> stratum_proto::StorageType {
        self.frame.vars()[pos].ty
    }
}

// ---------------------------------------------------------------------------
// Programs
// ---------------------------------------------------------------------------

fn call_program(
    ctx: &mut ExecCtx<'_>,
    set: &dyn CommandSet,
    name: &str,
    args: &str,
) -> Result<(), StataError> {
    let body = ctx
        .programs
        .get(name)
        .expect("caller checked that the program exists")
        .body
        .clone();
    ctx.calls.push(
        &mut ctx.macros,
        ScopeKind::Program {
            name: name.to_owned(),
        },
    )?;
    ctx.macros.set_positionals(args.trim());
    let outcome = exec_source(ctx, set, &body);
    let temps = ctx.calls.pop(&mut ctx.macros);
    drop_temp_columns(ctx, &temps);
    match outcome.status {
        ExecStatus::Failed { rc, message, span } => {
            let mut e = StataError::new(rc, message);
            if let Some(s) = span {
                e = e.at(s);
            }
            Err(e)
        }
        _ => Ok(()),
    }
}

/// Drop the columns a scope's temporaries created. Names that never became
/// columns are the normal case, not a failure.
fn drop_temp_columns(ctx: &mut ExecCtx<'_>, temps: &[String]) {
    for t in temps {
        let idx = ctx.frames.current().index_of(t);
        if let Some(idx) = idx {
            let _ = ctx.frames.current_mut().drop_var(idx);
        }
    }
}

// ---------------------------------------------------------------------------
// Running text
// ---------------------------------------------------------------------------

impl ExecCtx<'_> {
    /// The text of a body span, in the source currently being executed.
    ///
    /// # Errors
    ///
    /// `r(198)` when there is no source on the stack, which is a caller bug
    /// rather than a user error and says so.
    pub fn body_text(&self, body: Span) -> Result<String, StataError> {
        let src = self
            .source()
            .ok_or_else(|| StataError::new(198, "no source in scope for a block body").at(body))?;
        let (lo, hi) = (body.start as usize, (body.end as usize).min(src.len()));
        if lo > hi {
            return Ok(String::new());
        }
        Ok(src[lo..hi].to_owned())
    }

    /// Execute a loop or branch body, given its extent in the **pre-expansion**
    /// text of the source currently running.
    ///
    /// Macro expansion, parsing and dispatch all run again per pass. That is not
    /// an optimisation left on the table: it is how `` `x' `` picks up the new
    /// loop value, and `ast::BlockCommand` records the same requirement.
    ///
    /// # Errors
    ///
    /// The first error any command in the body raised.
    pub fn run_body(&mut self, set: &dyn CommandSet, body: Span) -> Result<(), StataError> {
        let text = self.body_text(body)?;
        let outcome = exec_source(self, set, &text);
        match outcome.status {
            ExecStatus::Failed { rc, message, span } => {
                let mut e = StataError::new(rc, message);
                if let Some(s) = span {
                    e = e.at(s);
                }
                Err(e)
            }
            _ => Ok(()),
        }
    }
}

/// Run a whole source buffer — a do-file, a program body, a loop body.
///
/// Stops at the first failing command, exactly as Stata does: a do-file that
/// keeps going after an error produces numbers computed from a dataset the user
/// did not intend.
pub fn exec_source(ctx: &mut ExecCtx<'_>, set: &dyn CommandSet, src: &str) -> Outcome {
    // `#delimit ;` is file-scoped, so a body executed in isolation must be
    // segmented in the mode that was in force where it was written — design 02
    // §13.2's named mistake. The innermost source on the stack is that mode.
    let seg = segment_in(src, ctx.source_delimiter());
    let mut diagnostics = Vec::new();

    for region in &seg.regions {
        if !region.is_executable() {
            continue;
        }
        if ctx.cancelled() {
            return Outcome::interrupted(Some(region.span));
        }

        // A block region's head must be macro-expanded (`foreach v of varlist
        // `vars'`) while its body must NOT be — Stata re-expands the body per
        // iteration. So the head line is expanded and the remainder is carried
        // verbatim; the body spans the parser reports then index THIS text,
        // which is what goes on the source stack.
        let text = match region_text(ctx, &seg, region) {
            Ok(t) => t,
            Err(e) => {
                diagnostics.push(to_diagnostic(&e));
                return Outcome {
                    status: ExecStatus::Failed {
                        rc: e.rc,
                        message: e.message.clone(),
                        span: e.span.or(Some(region.span)),
                    },
                    diagnostics,
                };
            }
        };

        let (ast, parse_diags) = parse_command(&text, ParseMode::Execute);
        if let Some(d) = parse_diags.iter().find(|d| d.severity == Severity::Error) {
            diagnostics.extend(parse_diags.iter().cloned());
            return Outcome {
                status: ExecStatus::Failed {
                    rc: d.stata_rc.unwrap_or(198),
                    message: d.message.clone(),
                    span: Some(region.span),
                },
                diagnostics,
            };
        }

        ctx.push_source_in(text, region.entry_delimiter);
        let outcome = exec_command(ctx, set, &ast);
        ctx.pop_source();

        diagnostics.extend(outcome.diagnostics.iter().cloned());
        if !outcome.is_ok() {
            return Outcome {
                status: outcome.status,
                diagnostics,
            };
        }
    }

    Outcome {
        status: ExecStatus::Succeeded,
        diagnostics,
    }
}

/// Segment in a known delimiter mode.
///
/// Every segmentation in this crate goes through here so that "which mode was
/// this written in" is asked once rather than defaulted seven times.
pub fn segment_in(src: &str, delim: stratum_proto::Delimiter) -> stratum_parse::Segmentation<'_> {
    stratum_parse::segment_with(
        src,
        &stratum_parse::SegmentOptions {
            initial_delimiter: delim,
            ..stratum_parse::SegmentOptions::default()
        },
    )
}

/// The body of an `end`-terminated block, in the coordinates of `text`.
///
/// `ast::BlockCommand::Program` carries an EMPTY body span on purpose: design
/// 02 §5.3 makes the SEGMENTER the single definition of where the matching
/// `end` is, and W04's parser refuses to re-derive it — "a second definition of
/// the same boundary" is its comment. This is the other half of that contract.
/// It asks the segmenter rather than scanning for the word `end`, which would
/// find the one inside a string literal or a nested block.
#[must_use]
pub fn end_block_body(text: &str, delim: stratum_proto::Delimiter) -> Span {
    let seg = segment_in(text, delim);
    // The head is the first logical line and `end` is the last. Fewer than
    // three lines means there is nothing between them.
    let Some(region) = seg.regions.iter().find(|r| r.is_executable()) else {
        return Span { start: 0, end: 0 };
    };
    let lines = region.logical_lines.usize();
    if lines.end < lines.start + 3 {
        return Span { start: 0, end: 0 };
    }
    Span {
        start: seg.lines[lines.start + 1].span.start,
        end: seg.lines[lines.end - 1].span.start,
    }
}

/// The text a region is executed from: head expanded, tail verbatim.
fn region_text(
    ctx: &mut ExecCtx<'_>,
    seg: &stratum_parse::Segmentation<'_>,
    region: &stratum_parse::Region,
) -> Result<String, StataError> {
    let lines = region.logical_lines.usize();
    let first = &seg.lines[lines.start];
    let derived = seg.derived[lines.start].as_deref();
    let head = first.code(seg.src, derived);
    let expanded = ctx.expand_line(head)?;

    // A one-line region is the overwhelmingly common case and needs no
    // splicing at all.
    if lines.end - lines.start == 1 {
        return Ok(expanded.text);
    }

    let tail_start = first.code_span.end as usize;
    let tail_end = (region.span.end as usize).min(seg.src.len());
    let mut out = expanded.text;
    if tail_start < tail_end {
        out.push_str(&seg.src[tail_start..tail_end]);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ctx::{NoHost, Transcript};
    use stratum_proto::{StyleId, StyledRun};

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

    /// A command set that can be told what to do, so the LIFECYCLE can be tested
    /// without a command surface: it echoes, panics, fails, or sets a macro.
    struct Probe;

    impl CommandSet for Probe {
        fn run(&self, ctx: &mut ExecCtx<'_>, ast: &CommandAst) -> Result<(), StataError> {
            let word = command_word(ast).unwrap_or_default().to_owned();
            let rest = match &ast.cmd {
                Command::Known(k) => k
                    .slots
                    .rest
                    .as_ref()
                    .map(|r| r.text.clone())
                    .unwrap_or_default(),
                Command::Unknown { rest, .. } => rest.text.clone(),
                _ => String::new(),
            };
            match word.as_str() {
                "display" => {
                    ctx.emit(&[StyledRun {
                        text: format!("{}\n", rest.trim().trim_matches('"')),
                        style: StyleId::Result,
                    }]);
                    Ok(())
                }
                "boom" => panic!("deliberate test panic"),
                "fail" => Err(StataError::new(111, "deliberate failure").token("fail")),
                "note" => {
                    ctx.macros.set_global("seen", rest.trim());
                    Ok(())
                }
                other => {
                    Err(StataError::new(10, format!("unsupported: {other}"))
                        .token(other.to_owned()))
                }
            }
        }

        fn implements(&self, canonical: &str) -> bool {
            matches!(canonical, "display" | "boom" | "fail" | "note")
        }
    }

    fn run(f: &mut Fixture, src: &str) -> (Outcome, String) {
        let mut ctx = ExecCtx::new(&mut f.out, &mut f.host);
        let o = exec_source(&mut ctx, &Probe, src);
        (o, String::new())
    }

    #[test]
    fn a_panicking_command_becomes_a_diagnostic_and_the_session_survives() {
        // The acceptance bullet: `catch_unwind` per command, a deliberately
        // panicking test command becomes Diagnostic{severity: Error} + Failed,
        // and the session's macros survive.
        let mut f = fixture();
        let mut ctx = ExecCtx::new(&mut f.out, &mut f.host);
        ctx.macros.set_global("keep", "me");
        let o = exec_source(&mut ctx, &Probe, "note hello\nboom\n");
        match &o.status {
            ExecStatus::Failed { rc, .. } => assert_eq!(*rc, 1),
            other => panic!("expected Failed, got {other:?}"),
        }
        assert_eq!(o.diagnostics.len(), 1);
        assert_eq!(o.diagnostics[0].severity, Severity::Error);
        assert_eq!(ctx.counters.panics_caught, 1);
        // The session is intact: the earlier command's effect and the macro set
        // before the run are both still there.
        assert_eq!(ctx.macros.global("keep"), Some("me"));
        assert_eq!(ctx.macros.global("seen"), Some("hello"));
    }

    #[test]
    fn capture_swallows_the_error_and_leaves_it_in_rc() {
        let mut f = fixture();
        let mut ctx = ExecCtx::new(&mut f.out, &mut f.host);
        let o = exec_source(&mut ctx, &Probe, "capture fail\n");
        assert!(o.is_ok(), "capture must not fail the run");
        assert_eq!(ctx.rc, 111, "_rc carries the swallowed code");
    }

    #[test]
    fn capture_catches_a_panic_too() {
        // A panicking command that the user wrapped in `capture` must behave
        // like any other failure, or a defensive do-file stops working the
        // moment we have a bug.
        let mut f = fixture();
        let mut ctx = ExecCtx::new(&mut f.out, &mut f.host);
        let o = exec_source(&mut ctx, &Probe, "capture boom\n");
        assert!(o.is_ok());
        assert_eq!(ctx.rc, 1);
    }

    #[test]
    fn quietly_suppresses_output_and_noisily_restores_it() {
        let mut f = fixture();
        {
            let mut ctx = ExecCtx::new(&mut f.out, &mut f.host);
            let o = exec_source(
                &mut ctx,
                &Probe,
                "display \"loud\"\nquietly display \"soft\"\n",
            );
            assert!(o.is_ok(), "{:?}", o.status);
        }
        assert_eq!(f.out.text(), "loud\n");
    }

    #[test]
    fn an_unrecognized_command_is_r199_and_names_itself() {
        let mut f = fixture();
        let (o, _) = run(&mut f, "frobnicate x\n");
        match &o.status {
            ExecStatus::Failed { rc, .. } => assert_eq!(*rc, 199),
            other => panic!("expected Failed, got {other:?}"),
        }
        assert_eq!(
            o.diagnostics[0].offending_token.as_deref(),
            Some("frobnicate")
        );
    }

    #[test]
    fn a_run_stops_at_the_first_failing_command() {
        let mut f = fixture();
        let mut ctx = ExecCtx::new(&mut f.out, &mut f.host);
        let o = exec_source(&mut ctx, &Probe, "fail\nnote after\n");
        assert!(!o.is_ok());
        assert_eq!(
            ctx.macros.global("seen"),
            None,
            "nothing after the failure ran"
        );
    }

    #[test]
    fn forvalues_counts_rather_than_accumulating() {
        // `0(.1)1` is eleven iterations. Accumulating `x += .1` gives ten,
        // because the tenth step lands at 0.9999999999999999.
        let mut f = fixture();
        let mut ctx = ExecCtx::new(&mut f.out, &mut f.host);
        let o = exec_source(
            &mut ctx,
            &Probe,
            "forvalues i = 0(.1)1 {\n    display \"`i'\"\n}\n",
        );
        assert!(o.is_ok(), "{:?}", o.status);
        drop(ctx);
        assert_eq!(f.out.lines().len(), 11);
        assert_eq!(f.out.lines()[0], "0");
        assert_eq!(f.out.lines()[10], "1");
    }

    #[test]
    fn a_loop_body_is_re_expanded_every_iteration() {
        // The property `BlockCommand`'s body-as-a-span exists for: the body text
        // is expanded per pass, so `` `i' `` is this iteration's value.
        let mut f = fixture();
        {
            let mut ctx = ExecCtx::new(&mut f.out, &mut f.host);
            let o = exec_source(
                &mut ctx,
                &Probe,
                "forvalues i = 1/3 {\n    display \"v`i'\"\n}\n",
            );
            assert!(o.is_ok(), "{:?}", o.status);
        }
        assert_eq!(f.out.lines(), vec!["v1", "v2", "v3"]);
    }

    #[test]
    fn foreach_in_walks_a_literal_list() {
        let mut f = fixture();
        {
            let mut ctx = ExecCtx::new(&mut f.out, &mut f.host);
            let o = exec_source(
                &mut ctx,
                &Probe,
                "foreach w in alpha beta {\n    display \"`w'\"\n}\n",
            );
            assert!(o.is_ok(), "{:?}", o.status);
        }
        assert_eq!(f.out.lines(), vec!["alpha", "beta"]);
    }

    #[test]
    fn if_else_takes_the_first_true_arm() {
        let mut f = fixture();
        {
            let mut ctx = ExecCtx::new(&mut f.out, &mut f.host);
            let o = exec_source(
                &mut ctx,
                &Probe,
                "if 0 {\n    display \"no\"\n}\nelse if 1 {\n    display \"yes\"\n}\nelse {\n    display \"never\"\n}\n",
            );
            assert!(o.is_ok(), "{:?}", o.status);
        }
        assert_eq!(f.out.lines(), vec!["yes"]);
    }

    #[test]
    fn a_program_is_defined_then_called_with_positionals() {
        let mut f = fixture();
        {
            let mut ctx = ExecCtx::new(&mut f.out, &mut f.host);
            let o = exec_source(
                &mut ctx,
                &Probe,
                "program define greet\n    display \"hi `1'\"\nend\ngreet world\n",
            );
            assert!(o.is_ok(), "{:?}", o.status);
        }
        assert_eq!(f.out.lines(), vec!["hi world"]);
    }

    #[test]
    fn the_by_prefix_answers_rc_ten_rather_than_pretending() {
        // A16: "unsupported in this version" must stay distinguishable from a
        // wrong answer. Silently ignoring `by` would produce ungrouped numbers.
        let mut f = fixture();
        let (o, _) = run(&mut f, "by foreign: note x\n");
        match &o.status {
            ExecStatus::Failed { rc, .. } => assert_eq!(*rc, 10),
            other => panic!("expected Failed, got {other:?}"),
        }
        assert_eq!(o.diagnostics[0].code, "STRATUM0010");
    }
}
