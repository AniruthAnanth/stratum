//! The built-in command surface — the `Pass 1` command list of
//! IMPLEMENTATION_PLAN §1, implemented against a host that supplies everything
//! ambient.
//!
//! # Why there is a trait here at all
//!
//! `docs/ownership.toml` splits `stratum-runtime` three ways. `ExecCtx`,
//! `dispatch` and `eval` are W06a's files; state, results and SMCL are W06b's;
//! this directory is W06c's. A command implementation that named `ExecCtx`
//! directly would make the three splits compile only together, and would put
//! this directory's tests at the mercy of an evaluator that is being written in
//! parallel. [`CmdHost`] is the seam: it is the complete list of what a built-in
//! command is allowed to reach for, every entry is something the interpreter
//! already owns, and it is deliberately flat (no `&mut dyn` sub-accessors)
//! because a command routinely needs the frame and the evaluator in the same
//! expression and nested borrows would make that impossible to write.
//!
//! **The wiring exists.** `ExecCtx` implements this trait in `ctx.rs`, and
//! `dispatch::BuiltinCommands` is the `CommandSet` adapter that forwards `run`
//! to [`builtin`] and `implements` to [`IMPLEMENTED`] — the only
//! implementation the shipping binary uses. `edit_var_meta` writes through
//! `Frame::var_mut` (W02), `load_dta`/`save_dta`/`sysuse_path` reach
//! `stratum-dta` through the recorded `RuntimeHost` doors (the shipping host
//! is `crate::host::FsHost`), and `run_stat` calls `stratum-stats` directly.
//!
//! Nothing in this directory constructs an `ExecCtx`, holds a lock, touches
//! the clock or opens a file.
//!
//! # Errors are `StataError`, not a local twin
//!
//! `stratum_parse::StataError` is already "a Stata return code, its message,
//! its span and its offending token", and it already renders to
//! `stratum_proto::Diagnostic` with `code = "STATA0111"`. Declaring a second
//! one here is exactly the twin A10 bans. Every constructor in [`err`]
//! populates `offending_token`, which the plan calls a merge blocker for the
//! r(111)/r(198)/r(199) class.
//!
//! # Output
//!
//! A command emits classic text as [`StyledRun`]s through [`CmdHost::emit`] and
//! returns typed payloads in [`CmdOutcome`]. It never builds a `String` for the
//! terminal: `stratum_proto::styled::to_plain` is the single flattening
//! function (A12), and the CLI, the log writer and the goldens all go through
//! it. [`Out`] is the builder every renderer in this directory uses.

pub mod control;
pub mod data;
pub mod display;
pub mod estimation_glue;
pub mod io;
pub mod manip;
pub mod settings;

use camino::Utf8Path;
use stratum_core::Value;
use stratum_data::FrameSet;
use stratum_parse::ast::expr::{Expr, StoredClass};
use stratum_parse::ast::CommandAst;
use stratum_parse::StataError;
use stratum_proto::{ResultPayload, ScalarValue, StyleId, StyledRun};

pub use settings::Settings;

/// What a built-in command produced, beyond the text it emitted.
///
/// Classic text does **not** travel in here — it goes through
/// [`CmdHost::emit`] as it is produced, so a long `list` streams instead of
/// buffering (design 03 §9.4). This carries only the typed payloads the result
/// card renders from.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct CmdOutcome {
    /// Typed payloads for `ResultEnvelope.payloads`, in render order.
    pub payloads: Vec<ResultPayload>,
}

impl CmdOutcome {
    /// A command that produced text and nothing else.
    #[must_use]
    pub fn text_only() -> Self {
        Self::default()
    }

    /// A command that produced exactly one payload.
    #[must_use]
    pub fn one(p: ResultPayload) -> Self {
        Self { payloads: vec![p] }
    }
}

/// What every built-in command returns.
pub type CmdResult = Result<CmdOutcome, StataError>;

/// A built-in command's entry point.
///
/// `fn` and not `Box<dyn Fn>`: the table is static, and an indirect call
/// through a fat pointer on every command dispatch buys nothing.
pub type Builtin = fn(&mut dyn CmdHost, &CommandAst) -> CmdResult;

/// What an expression is, before it is evaluated over rows.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum EvalType {
    /// Evaluates to a double.
    Numeric,
    /// Evaluates to a string.
    Str,
}

/// One variable-metadata edit. See [`CmdHost::edit_var_meta`].
#[derive(Clone, PartialEq, Debug)]
pub enum VarMetaEdit {
    /// `label variable x "…"`.
    Label(String),
    /// `format x %9.2f`.
    Format(stratum_core::fmt::StataFormat),
    /// `label values x lbl`, or `label values x .` to detach.
    ValueLabel(Option<String>),
}

/// What `use`/`sysuse` loaded, for the message the command prints.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct LoadReport {
    /// The dataset label, which `use` echoes in parentheses when non-empty.
    pub label: String,
}

/// A statistical command, resolved down to what `stratum-stats` needs.
///
/// The runtime does the Stata-shaped work — abbreviations, varlist expansion,
/// `if`/`in` to a [`stratum_data::Sample`], option validation — and hands the
/// statistics crate a request that has no parsing left in it.
#[derive(Clone, PartialEq, Debug)]
pub struct StatRequest {
    /// Canonical command name: `"summarize"`, `"regress"`, ….
    pub cmd: &'static str,
    /// The command line as submitted, after macro expansion (`e(cmdline)`).
    pub cmdline: String,
    /// Storage positions of the varlist, in the order given.
    pub vars: Vec<u32>,
    /// The `if`/`in` sample. Casewise deletion on top of this is the statistic's
    /// own business.
    pub sample: stratum_data::Sample,
    /// Options that survived validation, canonical name → argument text.
    pub options: Vec<(String, Option<String>)>,
}

/// Everything a built-in command may reach for.
///
/// Implemented once, by W06a's `ExecCtx`. Every method is something the
/// interpreter already owns; there is deliberately no method that would let a
/// command read the clock, the environment or the filesystem directly, because
/// design 03 §4.6 requires those reads to be recorded as taints.
pub trait CmdHost {
    // ---- dataset ----------------------------------------------------------

    /// The frame set. `frames().current()` is the dataset `list` prints.
    fn frames(&self) -> &FrameSet;

    /// The frame set, mutably. Column writes still go through
    /// `Frame::col_mut`, which is the write barrier.
    fn frames_mut(&mut self) -> &mut FrameSet;

    /// Edit one variable's METADATA — label, display format, value-label
    /// attachment.
    ///
    /// Backed by `Frame::var_mut` (W02), which bumps `var_layout` and not
    /// `gen` — renaming and relabelling do not change a value.
    fn edit_var_meta(
        &mut self,
        idx: stratum_proto::VarIdx,
        edit: VarMetaEdit,
    ) -> Result<(), StataError>;

    /// The file the data in memory came from, for `describe`'s "Contains data
    /// from …". `None` after `clear` or a synthesised dataset, which prints
    /// the bare "Contains data".
    fn data_source(&self) -> Option<&str>;

    /// Forget the dataset's file provenance — `clear`'s half of the
    /// [`CmdHost::data_source`] contract. The load path sets provenance on its
    /// own (`load_dta`), so this is the only method a command needs.
    fn clear_data_source(&mut self);

    /// The `.dta` header timestamp, rendered as Stata wrote it
    /// (`13 Apr 2022 17:45`). A string and not a `UnixMs` on purpose: it is
    /// the file's own bytes, not our clock, and reformatting it would need a
    /// timezone this layer must not have (A2).
    fn data_timestamp(&self) -> Option<&str>;

    // ---- settings ---------------------------------------------------------

    /// The `set` settings and `c()` values.
    fn settings(&self) -> &Settings;

    /// The settings, mutably. `set` is the only command that writes them.
    fn settings_mut(&mut self) -> &mut Settings;

    // ---- expression evaluation (W06a `eval.rs`) ---------------------------

    /// Is this expression numeric or string? Answered without evaluating it,
    /// because `generate` needs the type to pick a storage type before it
    /// creates the column.
    fn expr_type(&mut self, e: &Expr) -> Result<EvalType, StataError>;

    /// Evaluate once, outside any observation: `display`, `scalar =`, a
    /// control-flow condition. `_n`/`_N` refer to observation 1 / `_N`.
    fn eval_scalar(&mut self, e: &Expr) -> Result<Value, StataError>;

    /// Evaluate a numeric expression for rows `row0 .. row0 + len`, appending
    /// exactly `len` values to `out`.
    ///
    /// Chunk-wise and not whole-column on purpose: the caller passes
    /// [`stratum_data::CHUNK_ROWS`] at a time, so a `replace` over 10 M rows
    /// needs one 512 KiB scratch buffer rather than an 80 MB temporary. That is
    /// also the granule the storage and the undo journal use (A18, C35).
    fn eval_num_rows(
        &mut self,
        e: &Expr,
        row0: u64,
        len: usize,
        out: &mut Vec<f64>,
    ) -> Result<(), StataError>;

    /// The string counterpart of [`CmdHost::eval_num_rows`].
    fn eval_str_rows(
        &mut self,
        e: &Expr,
        row0: u64,
        len: usize,
        out: &mut Vec<String>,
    ) -> Result<(), StataError>;

    // ---- output -----------------------------------------------------------

    /// Append classic output. A no-op under `quietly` — commands still call it,
    /// and check [`CmdHost::quiet`] only when *building* the text is expensive.
    fn emit(&mut self, runs: &[StyledRun]);

    /// Is output suppressed right now (`quietly`, `capture` without `noisily`)?
    fn quiet(&self) -> bool;

    // ---- stored results (W06b `results.rs`) -------------------------------

    /// Drop the whole `r()` set. Every r-class command calls this before
    /// setting anything, because `r()` is replaced, never merged.
    fn clear_r(&mut self);

    /// Set one `r()` value. Insertion order is part of `return list`'s output
    /// contract, so the implementation must preserve it.
    fn set_r(&mut self, name: &str, v: ScalarValue);

    /// Read a stored result. `c()` is answered by
    /// [`settings::creturn`](crate::cmd::settings::creturn) and nothing else.
    fn stored(&self, class: StoredClass, name: &str) -> Option<ScalarValue>;

    /// Every name set in `class`, in the order they were set.
    ///
    /// `return list` prints in insertion order, so this is an ordered list and
    /// not a set: `r(N)` before `r(mean)` is part of the output contract, and a
    /// hash iteration here would reorder it per run and break byte-comparison
    /// against the golden.
    fn stored_names(&self, class: StoredClass) -> Vec<String>;

    // ---- macros (W06a `scope.rs`) -----------------------------------------

    /// Define a local macro in the current scope. `foreach` sets its loop
    /// variable through this.
    fn set_local(&mut self, name: &str, value: &str);

    /// Define a global macro.
    fn set_global(&mut self, name: &str, value: &str);

    /// Read a macro's text. `global` selects `$g` over `` `l' ``.
    ///
    /// `foreach x of local L` is the only reader in this directory, and it
    /// reads through the host rather than through `MacroEnv` because the local
    /// it wants belongs to the CURRENT call frame — which is `scope.rs`'s, not
    /// anything this trait could hold a reference to.
    fn get_macro(&self, global: bool, name: &str) -> Option<String>;

    // ---- re-entering the interpreter (control flow) -----------------------

    /// Execute a loop or branch body, given its extent in the **pre-expansion**
    /// logical-line text.
    ///
    /// A body is a span and not an AST because Stata re-expands it on every
    /// iteration — that is how `` `x' `` picks up the new loop value (02 §6.2)
    /// — so the host re-runs macro expansion, parse and dispatch per pass.
    fn run_body(&mut self, body: stratum_proto::Span) -> Result<(), StataError>;

    /// `_rc` after the last `capture`d command.
    fn last_rc(&self) -> u32;

    /// Set `_rc`. `capture` is the only caller.
    fn set_last_rc(&mut self, rc: u32);

    // ---- the world outside (W06a `ExecCtx`, W03 `stratum-dta`) ------------

    /// Replace the current frame with the contents of a `.dta` file.
    ///
    /// The host does the reading, the codepage translation and the frame swap;
    /// this directory owns only the messages and the argument grammar.
    fn load_dta(&mut self, path: &Utf8Path, clear: bool) -> Result<LoadReport, StataError>;

    /// Write the current frame to a `.dta` file.
    fn save_dta(&mut self, path: &Utf8Path, replace: bool) -> Result<(), StataError>;

    /// Resolve a `sysuse` name (`auto`) to a path under the ado tree.
    fn sysuse_path(&mut self, name: &str) -> Result<camino::Utf8PathBuf, StataError>;

    /// Delete a file (`erase`).
    fn erase_file(&mut self, path: &Utf8Path) -> Result<(), StataError>;

    /// The working directory, as `ExecCtx` records it. Never `std::env`.
    fn cwd(&self) -> &Utf8Path;

    /// Change the working directory.
    fn set_cwd(&mut self, path: &Utf8Path) -> Result<(), StataError>;

    /// Does this path exist? `confirm file` and `save` are the only askers.
    ///
    /// A recorded read, like every other door in this trait — which is why it
    /// takes `&mut self`: an existence check that a later `save` turns into a
    /// different answer is exactly the unrecorded dependency INV-1 forbids
    /// (design 03 §6.3).
    fn file_exists(&mut self, path: &Utf8Path) -> bool;

    // ---- statistics (W05 `stratum-stats`) ---------------------------------

    /// Run a statistical command and take its result.
    ///
    /// The host adapts `stratum_stats::StatResult` — emitting its
    /// `classic_text(linesize)` runs, filling `r()`/`e()` from its `ResultSet`
    /// — and returns the payloads. Everything Stata-shaped about the call has
    /// already happened in [`estimation_glue`].
    fn run_stat(&mut self, req: &StatRequest) -> CmdResult;

    /// Does this build implement `cmd`? Backed by
    /// `stratum_effects::CommandRegistry`. A command that is a real Stata
    /// command but is not implemented here exits 10, not 1 (plan §W09).
    fn implements(&self, cmd: &str) -> bool;

    /// Does this build implement `opt` for `cmd`? Also
    /// `stratum_effects::CommandRegistry`. A command can be implemented while
    /// one of its options is not, and answering that with "the command works"
    /// is how a user gets silently different numbers.
    fn implements_option(&self, cmd: &str, opt: &str) -> bool;
}

// ---------------------------------------------------------------------------
// The table
// ---------------------------------------------------------------------------

/// Look a built-in up by its CANONICAL name.
///
/// Abbreviation resolution has already happened in
/// `stratum_parse::CommandTable::resolve`; this is a lookup, not a second
/// abbreviation engine (the same rule `CommandRegistry::implements` states).
///
/// Returning `None` means "this build does not implement that command", which
/// is the exit-10 path — deliberately distinct from "that is not a command",
/// which the parser answered with r(199) long before dispatch.
#[must_use]
pub fn builtin(canonical: &str) -> Option<Builtin> {
    Some(match canonical {
        // data.rs — describing and reshaping the dataset without changing values
        "describe" => data::describe,
        "list" => data::list,
        "count" => data::count,
        "clear" => data::clear,
        "sort" => data::sort,
        "gsort" => data::gsort,
        "label" => data::label,
        "format" => data::format,
        "ds" => data::ds,
        // manip.rs — creating and changing variables
        "generate" => manip::generate,
        "replace" => manip::replace,
        "drop" => manip::drop,
        "keep" => manip::keep,
        "rename" => manip::rename,
        // io.rs
        "use" => io::use_,
        "sysuse" => io::sysuse,
        "save" => io::save,
        "cd" => io::cd,
        "pwd" => io::pwd,
        "erase" => io::erase,
        // display.rs
        "display" => display::display,
        // control.rs
        "capture" => control::capture,
        "quietly" => control::quietly,
        "noisily" => control::noisily,
        "version" => control::version,
        "exit" => control::exit,
        "error" => control::error,
        "continue" => control::r#continue,
        "assert" => control::assert,
        "confirm" => control::confirm,
        // settings.rs
        "set" => settings::set,
        "creturn" => settings::creturn_list,
        // estimation_glue.rs
        "summarize" => estimation_glue::summarize,
        "tabulate" => estimation_glue::tabulate,
        "correlate" => estimation_glue::correlate,
        "pwcorr" => estimation_glue::pwcorr,
        "ttest" => estimation_glue::ttest,
        "regress" => estimation_glue::regress,
        "predict" => estimation_glue::predict,
        "return" => estimation_glue::return_list,
        "ereturn" => estimation_glue::ereturn_list,
        _ => return None,
    })
}

/// Every canonical command name [`builtin`] answers to.
///
/// This is the list `CommandRegistry::implements` is built from, so the quick
/// actions on a result card (A22) and the CLI's exit-10 path agree with what
/// dispatch will actually do — by construction, not by a second list someone
/// has to remember to update.
pub const IMPLEMENTED: &[&str] = &[
    "assert",
    "capture",
    "cd",
    "clear",
    "confirm",
    "continue",
    "correlate",
    "count",
    "creturn",
    "describe",
    "display",
    "drop",
    "ds",
    "erase",
    "ereturn",
    "error",
    "exit",
    "format",
    "generate",
    "gsort",
    "keep",
    "label",
    "list",
    "noisily",
    "predict",
    "pwcorr",
    "pwd",
    "quietly",
    "regress",
    "rename",
    "replace",
    "return",
    "save",
    "set",
    "sort",
    "summarize",
    "sysuse",
    "tabulate",
    "ttest",
    "use",
    "version",
];

// ---------------------------------------------------------------------------
// Styled output
// ---------------------------------------------------------------------------

/// A styled-output builder.
///
/// Every renderer in this directory writes through one of these. Runs are
/// coalesced as they are pushed, so the number of runs scales with the number
/// of STYLE CHANGES and not with the number of characters, cells or format
/// directives: `list make price mpg` is two runs per column per row — the
/// value in `{res}` and the structure around it in `{txt}` — and every
/// separator, border and pad between two values of the same class is merged
/// into one run. `tests/cmd_surface.rs` asserts the invariant directly: no two
/// adjacent runs ever share a style. That is what keeps a `ResultEnvelope`
/// small enough to coalesce inside the 16 ms budget (ARCHITECTURE C23).
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Out {
    runs: Vec<StyledRun>,
}

impl Out {
    /// An empty builder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Append text in a style, merging with the previous run when the style
    /// matches.
    pub fn push(&mut self, style: StyleId, text: &str) {
        if text.is_empty() {
            return;
        }
        match self.runs.last_mut() {
            Some(last) if last.style == style => last.text.push_str(text),
            _ => self.runs.push(StyledRun {
                text: text.to_owned(),
                style,
            }),
        }
    }

    /// Append `{txt}`-class text: table structure, headers, labels.
    pub fn txt(&mut self, text: &str) {
        self.push(StyleId::Text, text);
    }

    /// Append `{res}`-class text: a value the command computed.
    pub fn res(&mut self, text: &str) {
        self.push(StyleId::Result, text);
    }

    /// Append error-class text.
    pub fn err(&mut self, text: &str) {
        self.push(StyleId::Error, text);
    }

    /// End the line. Stata's line terminator is `\n`, on every platform: the
    /// log file and the goldens are byte-compared across three OSes.
    pub fn nl(&mut self) {
        self.push(StyleId::Text, "\n");
    }

    /// Append `n` spaces as `{txt}`.
    pub fn spaces(&mut self, n: usize) {
        if n > 0 {
            self.push(StyleId::Text, &" ".repeat(n));
        }
    }

    /// The runs, in order.
    #[must_use]
    pub fn runs(&self) -> &[StyledRun] {
        &self.runs
    }

    /// Take the runs.
    #[must_use]
    pub fn into_runs(self) -> Vec<StyledRun> {
        self.runs
    }

    /// The flattened bytes. Goes through `stratum_proto::styled::to_plain`
    /// rather than concatenating here, because that function is the ONE
    /// flattening (A12) and a second copy is how the goldens and the log drift.
    #[must_use]
    pub fn to_plain(&self) -> String {
        stratum_proto::styled::to_plain(&self.runs)
    }

    /// True when nothing has been written.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.runs.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// The return codes this directory raises, with the message StataMP 18.5
/// prints for each.
///
/// Every message here is transcribed from `tests/golden/stata18/errors.log`;
/// the ones that are not in that file are marked. **Every constructor sets
/// `offending_token`** where there is an offending word — the plan calls a
/// missing one a merge blocker, so the ones that genuinely have no token
/// (`too few variables specified`) are the only ones without.
pub mod err {
    use stratum_parse::StataError;

    /// r(111), varlist position: `summarize nosuchvar`.
    #[must_use]
    pub fn var_not_found(name: &str) -> StataError {
        StataError::new(111, format!("variable {name} not found")).token(name)
    }

    /// r(111), expression position: `count if nosuchvar > 1` prints the name
    /// WITHOUT the leading `variable ` — verified, `errors.log`.
    #[must_use]
    pub fn name_not_found(name: &str) -> StataError {
        StataError::new(111, format!("{name} not found")).token(name)
    }

    /// r(110): `generate price = 1` on a dataset that already has `price`.
    #[must_use]
    pub fn already_defined(name: &str) -> StataError {
        StataError::new(110, format!("variable {name} already defined")).token(name)
    }

    /// r(102): `generate = 1`.
    #[must_use]
    pub fn too_few_vars() -> StataError {
        StataError::new(102, "too few variables specified")
    }

    /// r(103).
    #[must_use]
    pub fn too_many_vars() -> StataError {
        StataError::new(103, "too many variables specified")
    }

    /// r(100): a required option or argument is missing. `ttest price` →
    /// `by() option required`.
    #[must_use]
    pub fn required(what: &str) -> StataError {
        StataError::new(100, format!("{what} required")).token(what)
    }

    /// r(109): `generate x = "text" + 1`.
    #[must_use]
    pub fn type_mismatch() -> StataError {
        StataError::new(109, "type mismatch")
    }

    /// r(107): `encode price, gen(newv)`.
    #[must_use]
    pub fn not_with_numeric() -> StataError {
        StataError::new(107, "not possible with numeric variable")
    }

    /// r(7): `confirm numeric variable make`.
    #[must_use]
    pub fn found_where_expected(tok: &str, expected: &str) -> StataError {
        StataError::new(7, format!("'{tok}' found where {expected} expected")).token(tok)
    }

    /// r(198): an option the command does not take.
    #[must_use]
    pub fn option_not_allowed(opt: &str) -> StataError {
        StataError::new(198, format!("option {opt} not allowed")).token(opt)
    }

    /// r(198), the generic invalid-syntax case. `what` is the offending word.
    #[must_use]
    pub fn invalid(what: &str) -> StataError {
        StataError::new(198, format!("{what} invalid")).token(what)
    }

    /// r(198): `list in 999`.
    #[must_use]
    pub fn obs_out_of_range() -> StataError {
        StataError::new(198, "observation numbers out of range")
    }

    /// r(198): `list in 0`.
    #[must_use]
    pub fn invalid_obs_number(tok: &str) -> StataError {
        StataError::new(198, format!("'{tok}' invalid observation number")).token(tok)
    }

    /// r(198): a name that is not a legal Stata identifier.
    #[must_use]
    pub fn invalid_name(name: &str) -> StataError {
        StataError::new(198, format!("{name} invalid name")).token(name)
    }

    /// r(199): `foo bar baz`. Raised by dispatch, not here, but the
    /// constructor lives with its siblings.
    #[must_use]
    pub fn unrecognized(cmd: &str) -> StataError {
        StataError::new(199, format!("command {cmd} is unrecognized")).token(cmd)
    }

    /// r(601): `use /no/such/file.dta`.
    #[must_use]
    pub fn file_not_found(path: &str) -> StataError {
        StataError::new(601, format!("file {path} not found")).token(path)
    }

    /// r(602): `save x.dta` where `x.dta` exists and `replace` was not given.
    #[must_use]
    pub fn file_already_exists(path: &str) -> StataError {
        StataError::new(602, format!("file {path} already exists")).token(path)
    }

    /// r(9): `assert` failed.
    #[must_use]
    pub fn assertion_false() -> StataError {
        StataError::new(9, "assertion is false")
    }

    /// r(2000): no observations.
    #[must_use]
    pub fn no_observations() -> StataError {
        StataError::new(2000, "no observations")
    }

    /// r(4): no data in memory.
    #[must_use]
    pub fn no_data() -> StataError {
        StataError::new(4, "no data in memory")
    }

    /// **STRATUM0010 / rc 10** — A16. Not a Stata code: `10` is our
    /// "unsupported in this version", and the plan requires it to stay distinct
    /// from `1` so a compatibility project can separate "we are wrong" from "we
    /// are incomplete".
    #[must_use]
    pub fn unsupported(what: &str) -> StataError {
        StataError::new(10, format!("unsupported in this version: {what}")).token(what)
    }
}

/// Turn a [`StataError`] into the wire diagnostic, with our `STRATUM0010`
/// spelling for rc 10.
///
/// `StataError::to_diagnostic` codes everything as `STATA{rc:04}`, which is
/// right for the Stata return codes and wrong for ours: rc 10 is not a Stata
/// code, and A16 names the diagnostic `STRATUM0010`.
#[must_use]
pub fn to_diagnostic(e: &StataError) -> stratum_proto::Diagnostic {
    let mut d = e.to_diagnostic();
    if e.rc == 10 {
        d.code = "STRATUM0010".to_owned();
    }
    d
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// `stratum_data`'s frame as a `stratum_parse::VarIndex`, so varlist
/// resolution runs against the live dataset.
///
/// CONTRACTS §13 says `stratum-data` implements `VarIndex`; it does not, and it
/// cannot without depending on `stratum-parse`, which would invert the layer
/// order (ARCHITECTURE §8: parse must not be reachable from data, and data is
/// below parse in the crate table). The adapter belongs on the first crate that
/// depends on both — this one. Reported in W06c's return.
pub struct FrameVarIndex<'a> {
    frame: &'a stratum_data::Frame,
}

impl<'a> FrameVarIndex<'a> {
    /// Wrap a frame.
    #[must_use]
    pub fn new(frame: &'a stratum_data::Frame) -> Self {
        Self { frame }
    }
}

impl stratum_parse::varlist::VarIndex for FrameVarIndex<'_> {
    fn len(&self) -> usize {
        self.frame.n_vars() as usize
    }

    fn name(&self, pos: usize) -> &str {
        &self.frame.vars()[pos].name
    }

    fn position(&self, name: &str) -> Option<usize> {
        self.frame.index_of(name).map(|idx| idx.0 as usize)
    }

    fn storage_type(&self, pos: usize) -> stratum_proto::StorageType {
        self.frame.vars()[pos].ty
    }
}

/// Resolve the command's varlist against the current frame.
///
/// `mode` is [`stratum_parse::VarlistMode::Existing`] for everything that reads
/// and `New` for `generate`/`egen`. An empty varlist yields every variable in
/// storage order when `all_if_empty`, and an empty vector otherwise.
pub fn resolve_varlist(
    host: &dyn CmdHost,
    vl: Option<&stratum_parse::ast::varlist::VarList>,
    mode: stratum_parse::VarlistMode,
    all_if_empty: bool,
) -> Result<Vec<u32>, StataError> {
    let frame = host.frames().current();
    let Some(vl) = vl else {
        return Ok(if all_if_empty {
            (0..frame.n_vars()).collect()
        } else {
            Vec::new()
        });
    };
    let index = FrameVarIndex::new(frame);
    let cx = stratum_parse::VarlistCtx {
        vars: &index,
        varabbrev: host.settings().varabbrev,
    };
    stratum_parse::expand_varlist(vl, &cx, mode)
}

/// Build the `if`/`in` sample for a command.
///
/// `in` resolves first because it bounds the rows the `if` has to be evaluated
/// over — `count if foreign == 1 in 1/40` evaluates the expression 40 times and
/// not 74. Evaluation is chunk-wise at [`stratum_data::CHUNK_ROWS`]; nothing
/// here allocates a vector proportional to `_N`.
pub fn build_sample(
    host: &mut dyn CmdHost,
    if_: Option<&Expr>,
    in_: Option<&stratum_parse::ast::command::InRange>,
) -> Result<stratum_data::Sample, StataError> {
    let nobs = host.frames().current().n_obs();
    let mut b = stratum_data::SampleBuilder::new(nobs);
    if let Some(r) = in_ {
        b = b.r#in(to_data_range(r)?).map_err(|e| sample_error(e, r))?;
    }
    if let Some(cond) = if_ {
        // The `in` bound is already in the builder, so evaluating over the
        // whole frame here would still be correct — but it would be O(_N) work
        // for a 10-row `in` range on a 10 M-row dataset.
        let (lo, hi) = match in_ {
            Some(r) => to_data_range(r)?
                .resolve(nobs)
                .map_err(|e| sample_error(e, r))?,
            None => (0, nobs),
        };
        let mut scratch: Vec<f64> = Vec::with_capacity(stratum_data::CHUNK_ROWS);
        let mut row = lo;
        while row < hi {
            let len = usize::try_from((hi - row).min(stratum_data::CHUNK_ROWS as u64))
                .expect("min with a usize constant fits usize");
            scratch.clear();
            host.eval_num_rows(cond, row, len, &mut scratch)?;
            b.if_chunk(row, &scratch);
            row += len as u64;
        }
    }
    Ok(b.build())
}

/// `stratum_parse`'s `InRange` in `stratum_data`'s spelling.
///
/// The two crates name the endpoints differently — `from`/`to` in the AST,
/// `first`/`last` in the data engine — and `stratum_parse::ObsRef::Num` is
/// SIGNED where `stratum_data::Bound` splits the sign into two variants. `in
/// -10/l` is `Bound::FromEnd(10)`, and `-1` is the last observation, which is
/// why the count is `|n|` and not `|n| - 1`.
fn to_data_range(
    r: &stratum_parse::ast::command::InRange,
) -> Result<stratum_data::InRange, StataError> {
    Ok(stratum_data::InRange {
        first: to_bound(r.from, r)?,
        last: to_bound(r.to, r)?,
    })
}

fn to_bound(
    o: stratum_parse::ast::command::ObsRef,
    r: &stratum_parse::ast::command::InRange,
) -> Result<stratum_data::Bound, StataError> {
    use stratum_parse::ast::command::ObsRef;
    Ok(match o {
        ObsRef::First => stratum_data::Bound::First,
        ObsRef::Last => stratum_data::Bound::Last,
        // `in 0` is r(198) `'0' invalid observation number`, and it is a
        // different message from `in 999` on a 74-row dataset — verified,
        // `errors.log`. Caught here rather than in the data engine only so the
        // offending token is the `0` the user typed.
        ObsRef::Num(0) => return Err(err::invalid_obs_number("0").at(r.span)),
        ObsRef::Num(n) if n < 0 => stratum_data::Bound::FromEnd(n.unsigned_abs()),
        ObsRef::Num(n) => stratum_data::Bound::Abs(n.unsigned_abs()),
    })
}

fn sample_error(
    e: stratum_data::SampleError,
    r: &stratum_parse::ast::command::InRange,
) -> StataError {
    StataError::new(u32::from(e.rc()), format!("{e}")).at(r.span)
}

/// Read a command's options into `(canonical_name, argument_text)` pairs,
/// rejecting anything not in `allowed` with r(198).
///
/// Stata reports the option **as typed** — `summarize price, detial` says
/// `option detial not allowed`, not `option detail not allowed` — so the
/// rejection carries `item.name`, which is also what goes in
/// `offending_token`.
pub fn take_options(
    cmd: &stratum_parse::ast::command::Slots,
    allowed: &[&str],
) -> Result<Vec<(String, Option<String>)>, StataError> {
    let mut out = Vec::with_capacity(cmd.options.items.len());
    for item in &cmd.options.items {
        let canonical = item.canonical.unwrap_or(item.name.as_str());
        if !allowed.contains(&canonical) {
            return Err(err::option_not_allowed(&item.name).at(item.span));
        }
        out.push((canonical.to_owned(), option_arg_text(item)));
    }
    Ok(out)
}

/// The text of an option's argument, whatever shape the parser gave it.
#[must_use]
pub fn option_arg_text(item: &stratum_parse::ast::command::OptionItem) -> Option<String> {
    use stratum_parse::ast::command::OptionArg;
    match item.arg.as_ref()? {
        OptionArg::Raw(r) => Some(r.text.clone()),
        OptionArg::Int(i) => Some(i.to_string()),
        OptionArg::Real(r) => Some(stratum_core::fmt::fmt_macro(*r)),
        OptionArg::Str(s) => Some(s.clone()),
        _ => None,
    }
}

/// Is `opt` present in a parsed option list (by canonical name, not negated)?
#[must_use]
pub fn has_option(cmd: &stratum_parse::ast::command::Slots, canonical: &str) -> bool {
    cmd.options
        .items
        .iter()
        .any(|i| !i.negated && i.canonical.unwrap_or(i.name.as_str()) == canonical)
}

/// The `Slots` of a `Command::Known`, or `None` for anything else.
///
/// Every entry in [`builtin`] is registered for a known command, so the `None`
/// arm is a dispatch bug rather than a user error; callers turn it into r(198)
/// rather than panicking, because a panic here would be caught by the
/// per-command `catch_unwind` and reported as an internal error, which is a
/// worse diagnostic for the same bug.
#[must_use]
pub fn slots(ast: &CommandAst) -> Option<&stratum_parse::ast::command::Slots> {
    match &ast.cmd {
        stratum_parse::ast::command::Command::Known(k) => Some(&k.slots),
        _ => None,
    }
}

/// The `rest` text of a `REST`-slot command (`display`, `set`, `label`), or
/// `""`.
#[must_use]
pub fn rest(ast: &CommandAst) -> &str {
    slots(ast)
        .and_then(|s| s.rest.as_ref())
        .map(|r| r.text.as_str())
        .unwrap_or("")
}

/// The span of the `rest` text, for error reporting.
#[must_use]
pub fn rest_span(ast: &CommandAst) -> stratum_proto::Span {
    slots(ast)
        .and_then(|s| s.rest.as_ref())
        .map(|r| r.span)
        .unwrap_or(ast.span)
}
