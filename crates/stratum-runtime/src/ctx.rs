//! [`ExecCtx`] — the interpreter's world, and the ONLY way a command reaches it.
//!
//! ARCHITECTURE §5 puts it plainly: `ExecCtx` is "the *only* ambient access to
//! env/clock/fs, **all recorded**". That is not a tidiness rule, it is what makes
//! design 03 §6.3's soundness argument true:
//!
//! > the namespaces in `DepFootprint` are, by construction, the **complete** set
//! > of inputs a command implementation can observe … because the command ABI
//! > gives implementations `&mut ExecCtx` and no ambient access to `std::env`,
//! > `SystemTime`, or the filesystem except through `ExecCtx` helpers that record
//! > into the footprint.
//!
//! So every door to the outside world is a method here that writes an entry into
//! [`AccessLog`] on its way through. A command body that called `std::fs::read`
//! directly would make a `Current` block wrong, silently, and there would be
//! nothing in the record to notice it with. There is deliberately no method that
//! hands a command the [`RuntimeHost`] itself.
//!
//! # Why the host is a trait and not `stratum-dta`
//!
//! `use` reads a `.dta`. The obvious edge — `stratum-runtime -> stratum-dta` —
//! is the one ARCHITECTURE §5 lists, and it is still the right edge for the
//! binary to wire up. It is not the right edge for *this* crate, because a
//! direct call would be a filesystem read that no barrier saw. [`RuntimeHost`]
//! keeps the read inside the recorded surface and, as a side effect, lets this
//! crate compile and be tested before `stratum-dta` exists.
//!
//! # What this module does NOT own
//!
//! `DepFootprint` / `WriteFootprint` (design 03 §4.7) and the `r()`/`e()`/`s()`
//! *clear* semantics are `footprint.rs` and `results.rs`, which belong to W06b.
//! [`AccessLog`] and [`StoredResults`] here are the recording surface the
//! interpreter writes into — the raw material those modules project onto the
//! wire. See their notes below.

use camino::{Utf8Path, Utf8PathBuf};
use rustc_hash::FxHashMap;
use stratum_core::Value;
use stratum_data::{Frame, FrameSet};
use stratum_parse::ast::expr::{Expr, StoredClass};
use stratum_parse::lints::StataError;
use stratum_parse::macros::MacroEnv;
use stratum_proto::{Delimiter, ResultPayload, ScalarValue, StyledRun, UnixMs, VarIdx};

use crate::program::ProgramTable;
use crate::scope::CallStack;

/// Where a command's text output goes.
///
/// Styled runs, never a `String`: style cannot be recovered from plain text
/// after the fact (CONTRACTS §5.2 / A12), and `stratum_proto::styled::to_plain`
/// is the single flattening every consumer shares. A sink that wants bytes
/// calls it; a sink that wants colour reads the runs.
pub trait Output {
    /// Append styled output, as produced.
    fn emit(&mut self, runs: &[StyledRun]);
}

/// An [`Output`] that keeps everything, for tests and for the CLI.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Transcript {
    runs: Vec<StyledRun>,
}

impl Transcript {
    /// An empty transcript.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The runs, in order.
    #[must_use]
    pub fn runs(&self) -> &[StyledRun] {
        &self.runs
    }

    /// The flattened text. Goes through the ONE flattening function so that a
    /// golden comparison here and the log file cannot drift.
    #[must_use]
    pub fn text(&self) -> String {
        stratum_proto::styled::to_plain(&self.runs)
    }

    /// The flattened text split into lines, with no trailing empty element for
    /// a newline-terminated transcript. This is the shape a golden comparison
    /// wants.
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        let t = self.text();
        let t = t.strip_suffix('\n').unwrap_or(&t);
        if t.is_empty() {
            return Vec::new();
        }
        t.split('\n').map(str::to_owned).collect()
    }

    /// Forget everything emitted so far.
    pub fn clear(&mut self) {
        self.runs.clear();
    }
}

impl Output for Transcript {
    fn emit(&mut self, runs: &[StyledRun]) {
        for r in runs {
            match self.runs.last_mut() {
                Some(last) if last.style == r.style => last.text.push_str(&r.text),
                _ => self.runs.push(r.clone()),
            }
        }
    }
}

/// An [`Output`] that discards everything — the `quietly` sink.
#[derive(Copy, Clone, Debug, Default)]
pub struct Sink;

impl Output for Sink {
    fn emit(&mut self, _runs: &[StyledRun]) {}
}

/// What a successful [`RuntimeHost::load_dataset`] hands back.
///
/// Wider than a bare [`Frame`] because the `.dta` header carries one fact a
/// frame has no slot for: the `<timestamp>`, which `describe` prints verbatim
/// (A2 — the file's own bytes, never our clock). Only the host has seen those
/// bytes, so the timestamp must ride the return; the *path* is not here
/// because the caller already holds it.
#[derive(Debug)]
pub struct LoadedData {
    /// The loaded frame, label and metadata included.
    pub frame: Frame,
    /// The file's `<timestamp>` field, verbatim (`13 Apr 2022 17:45`). Empty
    /// when the file carried none.
    pub timestamp: String,
}

/// Everything outside the engine process, behind one trait.
///
/// The shipping implementation is [`crate::host::FsHost`], wired to
/// `stratum-dta`; the runtime's tests substitute their own. Every method is
/// *recorded* by the [`ExecCtx`] wrapper that calls it — see the module
/// header.
pub trait RuntimeHost {
    /// Read a `.dta` into a frame. Wired to `stratum-dta` by [`crate::host`].
    ///
    /// # Errors
    ///
    /// A Stata return code: `r(601)` for a file that is not there, `r(610)` for
    /// one that is not a `.dta`.
    fn load_dataset(&mut self, path: &Utf8Path) -> Result<LoadedData, StataError>;

    /// Write the current frame to a `.dta`.
    ///
    /// # Errors
    ///
    /// `r(603)` when the file cannot be written.
    fn save_dataset(&mut self, path: &Utf8Path, frame: &Frame) -> Result<(), StataError>;

    /// Resolve a `sysuse` name (`auto`) to a path under the shipped ado tree.
    ///
    /// # Errors
    ///
    /// `r(601)` when no such dataset ships with this build.
    fn sysuse_path(&mut self, name: &str) -> Result<Utf8PathBuf, StataError>;

    /// Read a text file — `do`, `include`, `infile`.
    ///
    /// # Errors
    ///
    /// `r(601)` when the file cannot be read.
    fn read_text(&mut self, path: &Utf8Path) -> Result<String, StataError>;

    /// Delete a file (`erase`).
    ///
    /// # Errors
    ///
    /// `r(601)` when it is not there, `r(603)` when it cannot be removed.
    fn erase(&mut self, path: &Utf8Path) -> Result<(), StataError>;

    /// Does this path exist? `save` without `replace` needs to know.
    fn exists(&mut self, path: &Utf8Path) -> bool;

    /// Wall clock, in the one representation that crosses the wire (A2).
    fn now_ms(&mut self) -> UnixMs;

    /// One environment variable. Never `std::env` from a command body.
    fn env(&mut self, key: &str) -> Option<String>;
}

/// A [`RuntimeHost`] with no world attached.
///
/// Every door answers with the return code Stata gives for "not there", and the
/// clock is frozen at zero. It exists so a test of dispatch, evaluation or macro
/// expansion does not have to invent a filesystem, and so that a caller who
/// forgot to install a host gets `r(601)` rather than a panic.
#[derive(Clone, Debug, Default)]
pub struct NoHost;

fn not_found(path: &Utf8Path) -> StataError {
    StataError::new(601, format!("file {path} not found")).token(path.to_string())
}

impl RuntimeHost for NoHost {
    fn load_dataset(&mut self, path: &Utf8Path) -> Result<LoadedData, StataError> {
        Err(not_found(path))
    }

    fn save_dataset(&mut self, path: &Utf8Path, _frame: &Frame) -> Result<(), StataError> {
        Err(
            StataError::new(603, format!("file {path} could not be opened"))
                .token(path.to_string()),
        )
    }

    fn sysuse_path(&mut self, name: &str) -> Result<Utf8PathBuf, StataError> {
        Err(StataError::new(601, format!("file {name}.dta not found")).token(name))
    }

    fn read_text(&mut self, path: &Utf8Path) -> Result<String, StataError> {
        Err(not_found(path))
    }

    fn erase(&mut self, path: &Utf8Path) -> Result<(), StataError> {
        Err(not_found(path))
    }

    fn exists(&mut self, _path: &Utf8Path) -> bool {
        false
    }

    fn now_ms(&mut self) -> UnixMs {
        0
    }

    fn env(&mut self, _key: &str) -> Option<String> {
        None
    }
}

/// Cooperative cancellation. Design 03 §9.2's safepoint check.
///
/// Checked once per chunk on every O(rows) loop in this crate, never per row: a
/// per-row atomic load on a 10 M-row `replace` is 10 M loads to answer a
/// question that changes at most once.
pub trait CancelToken: Send + Sync {
    /// Has the user asked for this to stop?
    fn cancelled(&self) -> bool;
}

/// A token that never fires.
#[derive(Copy, Clone, Debug, Default)]
pub struct NeverCancel;

impl CancelToken for NeverCancel {
    fn cancelled(&self) -> bool {
        false
    }
}

/// ADR-017 counters: what the interpreter actually did, in units that do not
/// move when the machine is busy.
///
/// > a performance acceptance bullet must assert a *counter* — work done,
/// > allocations, regions re-hashed, bytes copied — and not a duration.
///
/// Plain `u64` because the interpreter is single-threaded by construction
/// (design 03 §9.1: one control thread, `rayon` only inside a kernel). An
/// `AtomicU64` here would buy nothing and cost a fence per row.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counters {
    /// Commands entered by [`crate::dispatch::exec_command`].
    pub commands: u64,
    /// Calls to `Frame::col_mut` — the write barrier. **One per command that
    /// writes a column**, never one per element; that is the acceptance bullet.
    pub col_mut_calls: u64,
    /// Observations visited by an expression evaluator.
    pub rows_touched: u64,
    /// Name→`VarIdx` resolutions performed while compiling expressions. A
    /// function of the expression's SIZE, never of the row count: that is the
    /// property [`crate::eval::Compiled`] exists to have.
    pub name_resolutions: u64,
    /// Expression nodes evaluated.
    pub eval_nodes: u64,
    /// Macro expansions run (one per logical line executed).
    pub expansions: u64,
    /// Re-entries into the interpreter from macro expansion (`` `=exp' ``).
    pub host_callbacks: u64,
    /// Commands that unwound through `catch_unwind`.
    pub panics_caught: u64,
}

/// Which namespace a recorded access names.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Ns {
    /// A local or global macro.
    Macro,
    /// A scalar.
    Scalar,
    /// A matrix.
    Matrix,
    /// A program.
    Program,
    /// A `set` setting or a `c()` key.
    Setting,
}

/// The raw record the read/write barriers write into.
///
/// **This is not `DepFootprint`.** That type, and its projection onto the wire,
/// are design 03 §4.7 and belong to `footprint.rs` (W06b). What lives here is
/// what the interpreter writes as it runs, in the interpreter's own coordinates
/// (`VarIdx`, owned names), which `footprint.rs` resolves into `VarId`s and
/// versions at command end. Keeping them separate is what lets the barrier be
/// one `Vec::insert` on a cold path rather than a wire-type construction.
///
/// The self-read exclusion of §4.7 is applied here on the way IN —
/// [`AccessLog::note_read`] is a no-op for a variable this command has already
/// written — because it is cheaper not to record than to filter afterwards, and
/// because a caller that forgets the rule would otherwise make every
/// multi-statement block depend on itself and never be `Current`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AccessLog {
    /// Columns read, sorted and deduplicated, self-reads excluded.
    pub vars_read: Vec<VarIdx>,
    /// Columns written.
    pub vars_written: Vec<VarIdx>,
    /// Columns created by this command.
    pub vars_created: Vec<VarIdx>,
    /// `(namespace, name)` reads.
    pub named_reads: Vec<(Ns, String)>,
    /// `(namespace, name)` writes.
    pub named_writes: Vec<(Ns, String)>,
    /// Files read, in the order opened.
    pub files_read: Vec<Utf8PathBuf>,
    /// Files written.
    pub files_written: Vec<Utf8PathBuf>,
    /// The command observed the observation count.
    pub read_row_membership: bool,
    /// The command observed the observation order.
    pub read_row_order: bool,
    /// The command read variable metadata (labels, formats, names).
    pub read_var_layout: bool,
    /// The command read the clock or the environment — design 03 §6.3's
    /// residual escape hatches, recorded rather than denied.
    pub read_ambient: bool,
}

impl AccessLog {
    /// Forget everything. Called at command entry.
    pub fn clear(&mut self) {
        self.vars_read.clear();
        self.vars_written.clear();
        self.vars_created.clear();
        self.named_reads.clear();
        self.named_writes.clear();
        self.files_read.clear();
        self.files_written.clear();
        self.read_row_membership = false;
        self.read_row_order = false;
        self.read_var_layout = false;
        self.read_ambient = false;
    }

    fn insert(set: &mut Vec<VarIdx>, v: VarIdx) {
        if let Err(at) = set.binary_search(&v) {
            set.insert(at, v);
        }
    }

    /// Record a column read. **No-op when this command already wrote or created
    /// the column** — design 03 §4.7's self-read exclusion, normative.
    pub fn note_read(&mut self, v: VarIdx) {
        if self.vars_written.binary_search(&v).is_ok()
            || self.vars_created.binary_search(&v).is_ok()
        {
            return;
        }
        Self::insert(&mut self.vars_read, v);
    }

    /// Record a column write.
    pub fn note_write(&mut self, v: VarIdx) {
        Self::insert(&mut self.vars_written, v);
    }

    /// Record a column creation.
    pub fn note_create(&mut self, v: VarIdx) {
        Self::insert(&mut self.vars_created, v);
    }

    /// Record a read of a named namespace.
    pub fn note_named_read(&mut self, ns: Ns, name: &str) {
        if !self.named_reads.iter().any(|(n, k)| *n == ns && k == name) {
            self.named_reads.push((ns, name.to_owned()));
        }
    }

    /// Record a write to a named namespace.
    pub fn note_named_write(&mut self, ns: Ns, name: &str) {
        if !self.named_writes.iter().any(|(n, k)| *n == ns && k == name) {
            self.named_writes.push((ns, name.to_owned()));
        }
    }
}

/// `r()`, `e()` and `s()` — W06b's `results.rs`, swapped in.
///
/// The provisional twin that used to live here is gone: `results.rs` owns the
/// full clear semantics ([U] 18.8) and the insertion-order guarantee `return
/// list` depends on, and a second store would be exactly the divergence its
/// module header warns about. The interpreter-facing spelling (`StoredClass`,
/// `ScalarValue`) is bridged by [`stored_scalar`] / [`stored_names`] below, so
/// nothing that reads through the old accessors had to move.
pub use crate::results::StoredResults;

/// `results.rs`'s class for a parse-layer [`StoredClass`], or `None` for
/// `c()`, which is computed from the settings and never stored
/// (`cmd::settings::creturn` is the one place that answers it).
fn results_class(class: StoredClass) -> Option<crate::results::Class> {
    match class {
        StoredClass::R => Some(crate::results::Class::R),
        StoredClass::E => Some(crate::results::Class::E),
        StoredClass::S => Some(crate::results::Class::S),
        StoredClass::C => None,
    }
}

/// One stored result as the evaluator's [`ScalarValue`].
///
/// Scalars first, then macros: a name cannot be both in one class, and Stata
/// resolves `r(name)` against whichever kind the command posted. The display
/// string is `%10.0g`, the width `display` proves against the golden
/// (`cmd::display::DEFAULT_WIDTH`).
#[must_use]
pub fn stored_scalar(
    results: &StoredResults,
    class: StoredClass,
    name: &str,
) -> Option<ScalarValue> {
    let set = results.get(results_class(class)?);
    if let Some(v) = set.scalar(name) {
        return Some(ScalarValue::Num {
            value: v,
            display: stratum_core::fmt::fmt_g(v, 10).trim_start().to_owned(),
        });
    }
    set.get_macro(name).map(|v| ScalarValue::Str {
        value: v.to_owned(),
    })
}

/// Every scalar and macro name set in `class`, scalars first, each kind in
/// assignment order — the order `return list` prints (the listing partitions
/// by kind, so only the within-kind order is observable).
#[must_use]
pub fn stored_names(results: &StoredResults, class: StoredClass) -> Vec<String> {
    let Some(class) = results_class(class) else {
        return Vec::new();
    };
    let set = results.get(class);
    let mut out: Vec<String> = set.scalars().map(|(k, _)| k.to_owned()).collect();
    out.extend(set.macros().map(|(k, _)| k.to_owned()));
    out
}

/// Turn a stored scalar into the expression evaluator's value type.
#[must_use]
pub fn scalar_to_value(s: &ScalarValue) -> Value {
    match s {
        ScalarValue::Num { value, .. } => Value::Real(*value),
        ScalarValue::Str { value } => Value::Str(value.clone()),
    }
}

/// A number as a [`ScalarValue`], with the display string every renderer draws
/// (A6: "numbers destined for a human arrive pre-formatted").
#[must_use]
pub fn num_scalar(v: f64) -> ScalarValue {
    ScalarValue::Num {
        value: v,
        display: stratum_core::fmt::fmt_macro(v),
    }
}

/// The settings — W06c's `cmd/settings.rs`, swapped in.
///
/// The provisional twin that used to live here is gone, as its own doc
/// promised: `cmd/settings.rs` owns the real `Settings`, ADR-016's rejection
/// of `set linesize` at any value but 80, and the `c()` surface. The line
/// width is [`Settings::linesize`] — a method over a constant
/// (`cmd::settings::LINESIZE`), so no code path can report anything else.
pub use crate::cmd::settings::Settings;

/// The interpreter's world.
///
/// Commands receive `&mut dyn CmdHost` over this and nothing else. Everything
/// they may observe is reachable from here, and everything reachable from here
/// that leaves the engine is recorded in [`ExecCtx::access`].
pub struct ExecCtx<'h> {
    /// Frames, with a current one. Stata 16 frames; v1 creates only `default`.
    pub frames: FrameSet,
    /// Local and global macros, with the scope stack.
    pub macros: MacroEnv,
    /// The interpreter's call stack, in step with `macros`' scope stack.
    pub calls: CallStack,
    /// `program define` bodies.
    pub programs: ProgramTable,
    /// `scalar x = …`.
    pub scalars: FxHashMap<String, Value>,
    /// `r()` / `e()` / `s()`.
    pub results: StoredResults,
    /// `set` state.
    pub settings: Settings,
    /// `_rc`. Set by `capture`, read by the expression evaluator.
    pub rc: u32,
    /// The working directory, as this session sees it. `cd` writes it; nothing
    /// in the engine reads `std::env::current_dir`.
    pub cwd: Utf8PathBuf,
    /// Depth of nested `quietly` / non-`noisily` `capture`. Output is suppressed
    /// while it is non-zero.
    pub quiet_depth: u32,
    /// What this command has touched.
    pub access: AccessLog,
    /// ADR-017 counters.
    pub counters: Counters,
    /// The file the data in memory came from, for `describe`'s "Contains data
    /// from …". `None` after `clear` or for a synthesised dataset.
    pub data_source: Option<String>,
    /// The `.dta` header timestamp, as the file's own bytes (A2 — never our
    /// clock). Set by the load path together with `data_source`.
    pub data_timestamp: Option<String>,

    /// The source buffers currently being executed, innermost last, each with
    /// the delimiter mode in force at its first byte.
    ///
    /// A block command's body is a `Span` into the text it was parsed from, so
    /// running that body needs the text back. A stack rather than a field
    /// because a loop body can contain a program call whose body contains
    /// another loop, and each level's spans index its own text.
    ///
    /// The delimiter rides along because `#delimit ;` is FILE-scoped (design 02
    /// §13.2): re-segmenting a body in isolation without the mode it started in
    /// silently mis-parses every `;`, and the failure is a program body that
    /// runs as one enormous command.
    sources: Vec<(String, Delimiter)>,

    /// Typed payloads the commands of the current execution produced, in
    /// dispatch order. `BuiltinCommands` fills it from each `CmdOutcome`; the
    /// engine layer drains it into `ExecOutcome.payloads`. A buffer here
    /// rather than a widened `CommandSet::run` return because the payloads
    /// must survive the prefix/`catch_unwind` machinery between the command
    /// and its caller, and because widening the trait would touch every
    /// `CommandSet` for a value only one implementation produces.
    pending_payloads: Vec<ResultPayload>,

    out: &'h mut dyn Output,
    host: &'h mut dyn RuntimeHost,
    cancel: &'h dyn CancelToken,
}

impl<'h> ExecCtx<'h> {
    /// A context over a fresh, empty session.
    pub fn new(out: &'h mut dyn Output, host: &'h mut dyn RuntimeHost) -> Self {
        Self::with_cancel(out, host, &NeverCancel)
    }

    /// A context that can be interrupted.
    pub fn with_cancel(
        out: &'h mut dyn Output,
        host: &'h mut dyn RuntimeHost,
        cancel: &'h dyn CancelToken,
    ) -> Self {
        ExecCtx {
            frames: FrameSet::new(),
            macros: MacroEnv::new(),
            calls: CallStack::new(),
            programs: ProgramTable::new(),
            scalars: FxHashMap::default(),
            results: StoredResults::default(),
            settings: Settings::default(),
            rc: 0,
            cwd: Utf8PathBuf::from("."),
            quiet_depth: 0,
            access: AccessLog::default(),
            counters: Counters::default(),
            data_source: None,
            data_timestamp: None,
            sources: Vec::new(),
            pending_payloads: Vec::new(),
            out,
            host,
            cancel,
        }
    }

    // -----------------------------------------------------------------------
    // The payload buffer
    // -----------------------------------------------------------------------

    /// Append typed payloads a command produced.
    ///
    /// Never cleared by the command lifecycle: a nested `run_body` re-enters
    /// `exec_command`, and a clear there would drop the outer command's
    /// payloads. The one consumer is [`ExecCtx::take_payloads`].
    pub fn push_payloads(&mut self, mut payloads: Vec<ResultPayload>) {
        self.pending_payloads.append(&mut payloads);
    }

    /// Drain everything the commands run so far produced, in dispatch order.
    /// The engine layer calls this once per executed block.
    #[must_use]
    pub fn take_payloads(&mut self) -> Vec<ResultPayload> {
        std::mem::take(&mut self.pending_payloads)
    }

    // -----------------------------------------------------------------------
    // The source stack
    // -----------------------------------------------------------------------

    /// Push the text a command was parsed from, so that its block bodies can be
    /// sliced out of it. Delimiter mode `cr`.
    pub fn push_source(&mut self, text: String) {
        self.push_source_in(text, Delimiter::Cr);
    }

    /// Push the text together with the delimiter mode in force at its start.
    pub fn push_source_in(&mut self, text: String, delim: Delimiter) {
        self.sources.push((text, delim));
    }

    /// The delimiter mode of the innermost source, or `cr` at the top level.
    #[must_use]
    pub fn source_delimiter(&self) -> Delimiter {
        self.sources.last().map_or(Delimiter::Cr, |(_, d)| *d)
    }

    /// Pop it again. Paired with [`ExecCtx::push_source`] by the dispatcher.
    pub fn pop_source(&mut self) {
        self.sources.pop();
    }

    /// The innermost source buffer.
    #[must_use]
    pub fn source(&self) -> Option<&str> {
        self.sources.last().map(|(t, _)| t.as_str())
    }

    // -----------------------------------------------------------------------
    // Output
    // -----------------------------------------------------------------------

    /// Append styled output. A no-op while `quietly` is in force.
    pub fn emit(&mut self, runs: &[StyledRun]) {
        if self.quiet_depth == 0 {
            self.out.emit(runs);
        }
    }

    /// Is output suppressed right now?
    #[must_use]
    pub fn quiet(&self) -> bool {
        self.quiet_depth > 0
    }

    // -----------------------------------------------------------------------
    // The recorded doors to the outside world
    // -----------------------------------------------------------------------

    /// Load a `.dta`, recording the read.
    ///
    /// # Errors
    ///
    /// Whatever the host answers — `r(601)` for a missing file.
    pub fn load_dataset(&mut self, path: &Utf8Path) -> Result<LoadedData, StataError> {
        self.access.files_read.push(path.to_owned());
        self.host.load_dataset(path)
    }

    /// Write the current frame to a `.dta`, recording the write.
    ///
    /// # Errors
    ///
    /// Whatever the host answers.
    pub fn save_dataset(&mut self, path: &Utf8Path) -> Result<(), StataError> {
        self.access.files_written.push(path.to_owned());
        self.host.save_dataset(path, self.frames.current())
    }

    /// Resolve a `sysuse` name.
    ///
    /// # Errors
    ///
    /// `r(601)` when no such dataset ships with this build.
    pub fn sysuse_path(&mut self, name: &str) -> Result<Utf8PathBuf, StataError> {
        self.host.sysuse_path(name)
    }

    /// Read a text file, recording the read.
    ///
    /// # Errors
    ///
    /// Whatever the host answers.
    pub fn read_text(&mut self, path: &Utf8Path) -> Result<String, StataError> {
        self.access.files_read.push(path.to_owned());
        self.host.read_text(path)
    }

    /// Delete a file, recording the write.
    ///
    /// # Errors
    ///
    /// Whatever the host answers.
    pub fn erase(&mut self, path: &Utf8Path) -> Result<(), StataError> {
        self.access.files_written.push(path.to_owned());
        self.host.erase(path)
    }

    /// Does this path exist? Recorded, because `save` branching on it makes the
    /// command's behaviour depend on the filesystem.
    pub fn exists(&mut self, path: &Utf8Path) -> bool {
        self.access.files_read.push(path.to_owned());
        self.host.exists(path)
    }

    /// The wall clock. Recorded: a block that reads it is not reproducible, and
    /// design 03 §6.3 requires that to be visible rather than assumed away.
    pub fn now_ms(&mut self) -> UnixMs {
        self.access.read_ambient = true;
        self.host.now_ms()
    }

    /// One environment variable, recorded for the same reason as the clock.
    pub fn env(&mut self, key: &str) -> Option<String> {
        self.access.read_ambient = true;
        self.host.env(key)
    }

    /// Has the user interrupted? Called once per chunk, never per row.
    #[must_use]
    pub fn cancelled(&self) -> bool {
        self.cancel.cancelled()
    }

    // -----------------------------------------------------------------------
    // Command lifecycle
    // -----------------------------------------------------------------------

    /// Open a rollbackable command: clear the access log and arm the frame's
    /// journal. Called by [`crate::dispatch::exec_command`], once.
    pub fn begin_command(&mut self) {
        self.access.clear();
        self.frames.current_mut().begin_command();
    }

    /// The command succeeded.
    pub fn commit(&mut self) {
        self.frames.current_mut().commit();
    }

    /// The command failed or was interrupted — INV-2: the frame goes back to
    /// exactly what it was at entry.
    pub fn rollback(&mut self) {
        self.frames.current_mut().rollback();
    }
}

// ---------------------------------------------------------------------------
// The CmdHost impl — the wiring `cmd/mod.rs` declares for W06a
// ---------------------------------------------------------------------------

/// `cmd/**` reaches the interpreter through this impl and nothing else.
///
/// Every method is a forward to something `ExecCtx` already holds; the ones
/// that touch the world go through the RECORDED doors above, never through
/// [`RuntimeHost`] directly, so the access log stays complete (design 03
/// §6.3). `run_body` re-enters dispatch with [`crate::dispatch::BuiltinCommands`]:
/// this impl is only ever reached *through* that command set, so the body runs
/// under the same one.
impl crate::cmd::CmdHost for ExecCtx<'_> {
    fn frames(&self) -> &FrameSet {
        &self.frames
    }

    fn frames_mut(&mut self) -> &mut FrameSet {
        &mut self.frames
    }

    fn edit_var_meta(
        &mut self,
        idx: VarIdx,
        edit: crate::cmd::VarMetaEdit,
    ) -> Result<(), StataError> {
        // Metadata is a per-variable fact downstream blocks can read (a label
        // in `describe`, a format in `list`), so the edit is recorded as a
        // write to the variable.
        self.access.note_write(idx);
        let v = self
            .frames
            .current_mut()
            .var_mut(idx)
            .ok_or_else(|| StataError::new(111, format!("variable #{} not found", idx.0)))?;
        match edit {
            crate::cmd::VarMetaEdit::Label(l) => v.label = std::sync::Arc::from(l),
            crate::cmd::VarMetaEdit::Format(f) => v.format = f,
            crate::cmd::VarMetaEdit::ValueLabel(l) => {
                v.value_label = l.map(std::sync::Arc::from);
            }
        }
        Ok(())
    }

    fn data_source(&self) -> Option<&str> {
        self.data_source.as_deref()
    }

    fn clear_data_source(&mut self) {
        self.data_source = None;
        self.data_timestamp = None;
    }

    fn data_timestamp(&self) -> Option<&str> {
        self.data_timestamp.as_deref()
    }

    fn settings(&self) -> &Settings {
        &self.settings
    }

    fn settings_mut(&mut self) -> &mut Settings {
        &mut self.settings
    }

    fn expr_type(&mut self, e: &Expr) -> Result<crate::cmd::EvalType, StataError> {
        Ok(match self.expr_ty(e)? {
            crate::eval::Ty::Num => crate::cmd::EvalType::Numeric,
            crate::eval::Ty::Str => crate::cmd::EvalType::Str,
        })
    }

    fn eval_scalar(&mut self, e: &Expr) -> Result<Value, StataError> {
        ExecCtx::eval_scalar(self, e)
    }

    // The two row evaluators compile per call. The callers hand over one CHUNK
    // at a time, so the compile repeats once per 65 536 rows — a cost
    // proportional to the expression's size, not the row count, and paid on
    // the same cold path as the journal's chunk retention. A cross-call cache
    // would need an identity for `&Expr` that survives nothing this crate
    // controls.
    fn eval_num_rows(
        &mut self,
        e: &Expr,
        row0: u64,
        len: usize,
        out: &mut Vec<f64>,
    ) -> Result<(), StataError> {
        let prog = self.compile_expr(e)?;
        self.eval_compiled_num_rows(&prog, row0, len, out)
    }

    fn eval_str_rows(
        &mut self,
        e: &Expr,
        row0: u64,
        len: usize,
        out: &mut Vec<String>,
    ) -> Result<(), StataError> {
        let prog = self.compile_expr(e)?;
        self.eval_compiled_str_rows(&prog, row0, len, out)
    }

    fn emit(&mut self, runs: &[StyledRun]) {
        ExecCtx::emit(self, runs);
    }

    fn quiet(&self) -> bool {
        ExecCtx::quiet(self)
    }

    fn clear_r(&mut self) {
        self.results.clear(crate::results::Class::R);
    }

    fn set_r(&mut self, name: &str, v: ScalarValue) {
        let set = self.results.get_mut(crate::results::Class::R);
        match v {
            ScalarValue::Num { value, .. } => set.set_scalar(name, value),
            ScalarValue::Str { value } => set.set_macro(name, value),
        }
    }

    fn stored(&self, class: StoredClass, name: &str) -> Option<ScalarValue> {
        if class == StoredClass::C {
            return crate::cmd::settings::creturn(self, name);
        }
        stored_scalar(&self.results, class, name)
    }

    fn stored_names(&self, class: StoredClass) -> Vec<String> {
        if class == StoredClass::C {
            return crate::cmd::settings::C_NAMES
                .iter()
                .map(|s| (*s).to_owned())
                .collect();
        }
        stored_names(&self.results, class)
    }

    fn set_local(&mut self, name: &str, value: &str) {
        self.access.note_named_write(Ns::Macro, name);
        self.macros.set_local(name, value);
    }

    fn set_global(&mut self, name: &str, value: &str) {
        self.access.note_named_write(Ns::Macro, name);
        self.macros.set_global(name, value);
    }

    fn get_macro(&self, global: bool, name: &str) -> Option<String> {
        if global {
            self.macros.global(name).map(str::to_owned)
        } else {
            self.macros.local(name).map(str::to_owned)
        }
    }

    fn run_body(&mut self, body: stratum_proto::Span) -> Result<(), StataError> {
        ExecCtx::run_body(self, &crate::dispatch::BuiltinCommands, body)
    }

    fn last_rc(&self) -> u32 {
        self.rc
    }

    fn set_last_rc(&mut self, rc: u32) {
        self.rc = rc;
    }

    fn load_dta(
        &mut self,
        path: &Utf8Path,
        clear: bool,
    ) -> Result<crate::cmd::LoadReport, StataError> {
        let mut path = path.to_owned();
        if path.extension().is_none() {
            // `use auto` reads `auto.dta`; the extension is implied, exactly
            // as `save` implies it on the way out.
            path.set_extension("dta");
        }
        // The must-clear rule: unsaved changes are refused, not overwritten.
        // A frame fresh from `use`/`sysuse` or after `save` reports
        // `changed() == false` and loads over silently.
        if !clear && self.frames.current().changed() {
            return Err(StataError::new(
                4,
                "no; dataset in memory has changed since last saved",
            ));
        }
        let loaded = self.load_dataset(&path)?;
        let label = loaded.frame.label().to_owned();
        // Frame REPLACEMENT is outside the chunk journal (its granularity is
        // the column). The ordering is what keeps INV-2: every fallible step
        // is above this line, so an error leaves the old frame in place for
        // dispatch's rollback, and success swaps wholesale — there is nothing
        // half-written for a rollback to miss.
        *self.frames.current_mut() = loaded.frame;
        self.data_source = Some(path.to_string());
        self.data_timestamp = (!loaded.timestamp.is_empty()).then_some(loaded.timestamp);
        Ok(crate::cmd::LoadReport { label })
    }

    fn save_dta(&mut self, path: &Utf8Path, _replace: bool) -> Result<(), StataError> {
        // The exists/`replace` refusal (r(602)) already happened in
        // `cmd::io::save`, through the recorded `file_exists` door.
        self.save_dataset(path)?;
        // The dataset in memory is now in sync with THIS file: `describe`
        // names it, and a following `use` without `clear` is allowed. The
        // header timestamp of the written file is not read back (the writer
        // never consults a clock), so the stale one is dropped rather than
        // reported as if it were the new file's.
        self.frames.current_mut().mark_saved();
        self.data_source = Some(path.to_string());
        self.data_timestamp = None;
        Ok(())
    }

    fn sysuse_path(&mut self, name: &str) -> Result<Utf8PathBuf, StataError> {
        ExecCtx::sysuse_path(self, name)
    }

    fn erase_file(&mut self, path: &Utf8Path) -> Result<(), StataError> {
        ExecCtx::erase(self, path)
    }

    fn cwd(&self) -> &Utf8Path {
        &self.cwd
    }

    fn set_cwd(&mut self, path: &Utf8Path) -> Result<(), StataError> {
        // Recorded through `exists`: which directories exist is a filesystem
        // fact this command's behaviour branches on.
        if !self.exists(path) {
            return Err(
                StataError::new(170, format!("unable to change to {path}")).token(path.to_string())
            );
        }
        self.cwd = path.to_owned();
        Ok(())
    }

    fn file_exists(&mut self, path: &Utf8Path) -> bool {
        ExecCtx::exists(self, path)
    }

    fn run_stat(&mut self, req: &crate::cmd::StatRequest) -> crate::cmd::CmdResult {
        crate::stat_glue::run_stat(self, req)
    }

    fn implements(&self, cmd: &str) -> bool {
        crate::cmd::IMPLEMENTED.contains(&cmd)
    }

    fn implements_option(&self, cmd: &str, opt: &str) -> bool {
        use stratum_effects::CommandRegistry as _;
        if stratum_stats::effects::COMMANDS.contains(&cmd) {
            // The statistics crate is the authority on its own option surface
            // (A22): an option it has not implemented must exit 10, not be
            // quietly accepted.
            stratum_stats::effects::StatsEffects.implements_option(cmd, opt)
        } else {
            // The non-statistical surface parses its own options and rejects
            // what it does not take with r(198); no registry gates them.
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stratum_proto::StyleId;

    fn run(style: StyleId, text: &str) -> StyledRun {
        StyledRun {
            text: text.to_owned(),
            style,
        }
    }

    #[test]
    fn a_transcript_coalesces_adjacent_runs_of_one_style() {
        // A 74-row `list` emits thousands of runs; a renderer that kept them all
        // separate would put every table character in its own wire object.
        let mut t = Transcript::new();
        t.emit(&[run(StyleId::Text, "a"), run(StyleId::Text, "b")]);
        t.emit(&[run(StyleId::Result, "1")]);
        assert_eq!(t.runs().len(), 2);
        assert_eq!(t.text(), "ab1");
    }

    #[test]
    fn transcript_lines_drop_the_terminating_newline_only() {
        let mut t = Transcript::new();
        t.emit(&[run(StyleId::Text, "one\ntwo\n")]);
        assert_eq!(t.lines(), vec!["one".to_owned(), "two".to_owned()]);
    }

    #[test]
    fn the_self_read_exclusion_is_applied_on_the_way_in() {
        // Design 03 §4.7, normative: `gen z = x + 1` then `replace z = z*2`
        // depends on x, not on z. Without this every multi-statement block would
        // depend on itself and never be Current.
        let mut log = AccessLog::default();
        log.note_create(VarIdx(3));
        log.note_read(VarIdx(3));
        log.note_read(VarIdx(1));
        assert_eq!(log.vars_read, vec![VarIdx(1)]);
        assert_eq!(log.vars_created, vec![VarIdx(3)]);
    }

    #[test]
    fn recorded_reads_are_sorted_and_deduplicated() {
        let mut log = AccessLog::default();
        for v in [5u32, 1, 5, 3, 1] {
            log.note_read(VarIdx(v));
        }
        assert_eq!(log.vars_read, vec![VarIdx(1), VarIdx(3), VarIdx(5)]);
    }

    #[test]
    fn stored_results_keep_assignment_order() {
        // `return list` prints in the order Stata prints it; a map would print
        // in hash order, which is a different transcript on every run.
        let mut r = StoredResults::default();
        let set = r.get_mut(crate::results::Class::R);
        set.set_scalar("N", 74.0);
        set.set_scalar("mean", 21.0);
        set.set_scalar("N", 69.0);
        assert_eq!(stored_names(&r, StoredClass::R), vec!["N", "mean"]);
        assert_eq!(
            stored_scalar(&r, StoredClass::R, "N").map(|v| scalar_to_value(&v)),
            Some(Value::Real(69.0))
        );
    }

    #[test]
    fn c_is_not_a_stored_namespace() {
        // `c()` is computed from the settings; storing it would give two answers
        // to `c(linesize)`, one of which would be stale. The bridge answers
        // `None`, and `stored_names` lists nothing, for the class that is
        // never stored.
        let r = StoredResults::default();
        assert!(stored_scalar(&r, StoredClass::C, "linesize").is_none());
        assert!(stored_names(&r, StoredClass::C).is_empty());
    }

    #[test]
    fn linesize_is_eighty_and_is_not_a_field() {
        assert_eq!(crate::cmd::settings::LINESIZE, 80);
        assert_eq!(Settings::default().linesize(), 80);
    }
}
