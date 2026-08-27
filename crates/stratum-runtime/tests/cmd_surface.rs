//! W06c acceptance — the built-in command surface.
//!
//! # What this file drives, and what it deliberately does not
//!
//! `crates/stratum-runtime/src/cmd/**` is written against [`CmdHost`], which is
//! the complete list of what a built-in command may reach for. That seam exists
//! so the command surface can be exercised without an `ExecCtx`, and this file
//! is the reason it is shaped that way: `TestHost` below implements the trait
//! over a real [`stratum_data::FrameSet`] and a ~200-line expression evaluator
//! that covers exactly the expressions the goldens use. Nothing here constructs
//! an interpreter, so a failure in this file is a failure in `cmd/**` and
//! nowhere else.
//!
//! # The goldens are the authority
//!
//! Every expected string carrying a `GOLDEN` marker is transcribed from
//! `tests/golden/stata18/*.log`, captured from StataMP 18.5 and irreplaceable.
//! Where a golden cannot be reproduced from a synthesised frame the reason is
//! stated at the assertion, never worked around by editing the expectation:
//!
//! * `tests/golden/stata18/*.log` were captured under `set linesize 100`, which
//!   A16 rejects. Every renderer in `cmd/**` therefore takes the width as a
//!   parameter and this file drives both widths.
//! * `stratum_data` exposes no route to a variable's `label`, `format` or
//!   `value_label` (no `Frame::var_mut`), so a synthesised `price` carries the
//!   default `%8.0g` and not `auto.dta`'s `%8.0gc`. The `describe` and `list`
//!   geometry is asserted against literals derived from the same layout rule
//!   the golden follows; the golden's own bytes are asserted wherever the
//!   rendering does not depend on metadata this crate cannot set. Escalated in
//!   W06c's return.
//!
//! # Counters, not clocks (ADR-017)
//!
//! The performance assertions here count version bumps, evaluator calls, emit
//! calls and styled runs. No test asserts a duration.

#![allow(clippy::needless_pass_by_value)]

use std::collections::{BTreeMap, VecDeque};

use camino::{Utf8Path, Utf8PathBuf};
use stratum_core::math;
use stratum_core::missing::{is_missing, missing_f64, SYSMISS};
use stratum_core::Value;
use stratum_data::column::NumCol;
use stratum_data::{Column, Frame, FrameSet, StorageType};
use stratum_parse::ast::expr::{BinOp, Expr, StoredClass, SysVar, UnOp};
use stratum_parse::ast::CommandAst;
use stratum_parse::{parse_command, ParseMode, StataError};
use stratum_proto::{ScalarValue, StyleId, StyledRun, VarIdx};
use stratum_runtime::cmd::{
    self, builtin, err, settings::Settings, CmdHost, CmdResult, EvalType, LoadReport, StatRequest,
    VarMetaEdit, IMPLEMENTED,
};

// ---------------------------------------------------------------------------
// The host
// ---------------------------------------------------------------------------

/// What a command asked the world for, so a test can assert the routing rather
/// than the side effect.
#[derive(Clone, PartialEq, Debug)]
enum Ask {
    LoadDta(Utf8PathBuf, bool),
    SaveDta(Utf8PathBuf, bool),
    Erase(Utf8PathBuf),
    SetCwd(Utf8PathBuf),
    MetaEdit(VarIdx, VarMetaEdit),
    Stat(String),
}

/// Counters the acceptance bullets are expressed in (ADR-017).
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
struct Counts {
    /// Calls into the row evaluator.
    eval_calls: u64,
    /// Rows the row evaluator was asked to produce, summed.
    eval_rows: u64,
    /// Calls to `CmdHost::emit`.
    emits: u64,
    /// The largest number of styled runs handed to one `emit`.
    max_runs_per_emit: usize,
}

struct TestHost {
    frames: FrameSet,
    settings: Settings,
    out: Vec<StyledRun>,
    /// How many runs each `emit` carried, in order.
    ///
    /// `Out` coalesces within one emit and cannot coalesce across two: a
    /// streamed command hands the sink a batch and starts a fresh builder, so
    /// the run that ends batch *n* and the run that opens batch *n+1* reach the
    /// transcript unmerged even when they share a style. That seam is a
    /// property of streaming, not of the renderer, and
    /// [`list_coalesces_runs_rather_than_emitting_one_per_cell`] has to be able
    /// to tell the two apart.
    batches: Vec<usize>,
    quiet: bool,
    r: Vec<(String, ScalarValue)>,
    e: Vec<(String, ScalarValue)>,
    s: Vec<(String, ScalarValue)>,
    locals: BTreeMap<String, String>,
    globals: BTreeMap<String, String>,
    scalars: BTreeMap<String, Value>,
    /// The observation `eval_scalar` and the row evaluator are currently on.
    obs: u64,
    last_rc: u32,
    cwd: Utf8PathBuf,
    files: Vec<Utf8PathBuf>,
    asks: Vec<Ask>,
    counts: Counts,
    /// Names `implements_option` answers `false` for, as `cmd:opt`.
    unimplemented_options: Vec<String>,
    /// How many times `run_body` was called.
    body_runs: u32,
    /// What successive `run_body` calls return: `0` is success, anything else
    /// is the return code that pass raises. An exhausted queue succeeds, so a
    /// test that only cares about the pass COUNT leaves it empty.
    body_rcs: VecDeque<u32>,
    /// Every `(name, value)` a command wrote to a local, in order.
    ///
    /// A loop's whole observable contract at this seam is "set the loop
    /// variable, then run the body, once per value", so the sequence of writes
    /// IS the assertion — a `foreach` that iterated the right number of times
    /// over the wrong values passes a pass-count check and fails this one.
    locals_log: Vec<(String, String)>,
}

impl TestHost {
    fn new(frame: Frame) -> Self {
        let mut frames = FrameSet::new();
        *frames.current_mut() = frame;
        Self {
            frames,
            settings: Settings::default(),
            out: Vec::new(),
            batches: Vec::new(),
            quiet: false,
            r: Vec::new(),
            e: Vec::new(),
            s: Vec::new(),
            locals: BTreeMap::new(),
            globals: BTreeMap::new(),
            scalars: BTreeMap::new(),
            obs: 0,
            last_rc: 0,
            cwd: Utf8PathBuf::from("/w"),
            files: Vec::new(),
            asks: Vec::new(),
            counts: Counts::default(),
            unimplemented_options: Vec::new(),
            body_runs: 0,
            body_rcs: VecDeque::new(),
            locals_log: Vec::new(),
        }
    }

    /// Run one command, given as the text a user typed.
    fn run(&mut self, line: &str) -> CmdResult {
        let (ast, _diags) = parse_command(line, ParseMode::Execute);
        let canonical = canonical_name(&ast, line);
        let f = builtin(&canonical)
            .unwrap_or_else(|| panic!("no builtin for {canonical:?} (from {line:?})"));
        self.out.clear();
        self.batches.clear();
        self.counts = Counts::default();
        f(self, &ast)
    }

    /// The classic text the last command emitted, flattened by the ONE
    /// flattening function (A12).
    fn text(&self) -> String {
        stratum_proto::styled::to_plain(&self.out)
    }

    fn runs(&self) -> &[StyledRun] {
        &self.out
    }

    fn set_scalar_r(&mut self, name: &str, v: f64) {
        self.r.push((
            name.to_owned(),
            ScalarValue::Num {
                value: v,
                display: stratum_core::fmt::fmt_g(v, 10).trim_start().to_owned(),
            },
        ));
    }

    // ---- the evaluator -----------------------------------------------------

    /// Evaluate one expression at observation `obs`.
    ///
    /// A test fixture, not a second interpreter: it covers the literals,
    /// operators and functions the golden corpus uses and raises r(111)/r(109)
    /// exactly where `eval.rs` does, because `cmd/**`'s error paths are keyed
    /// on those codes.
    fn value_at(&self, e: &Expr, obs: u64) -> Result<Value, StataError> {
        Ok(match e {
            Expr::Num(v, _) => Value::Real(*v),
            Expr::Missing(tag, _) => Value::Real(missing_f64(*tag)),
            Expr::Str(s, _) => Value::Str(s.clone()),
            Expr::Paren(inner, _) => self.value_at(inner, obs)?,
            Expr::Sys(sv, _) => Value::Real(match sv {
                SysVar::NLower => (obs + 1) as f64,
                SysVar::NUpper => self.frames.current().n_obs() as f64,
                SysVar::Pi => core::f64::consts::PI,
                SysVar::Rc => f64::from(self.last_rc),
            }),
            Expr::Name(n, sp) => {
                let frame = self.frames.current();
                if let Some(idx) = frame.index_of(n) {
                    read_cell(frame, idx, obs)
                } else if let Some(v) = self.scalars.get(n) {
                    v.clone()
                } else {
                    // `count if nosuchvar > 1` prints the bare name, with no
                    // leading `variable ` — GOLDEN errors.log.
                    return Err(err::name_not_found(n).at(*sp));
                }
            }
            Expr::Index { base, idx, .. } => {
                let at = self.value_at(idx, obs)?.as_real().unwrap_or(SYSMISS);
                if is_missing(at) || at < 1.0 {
                    Value::Real(SYSMISS)
                } else {
                    self.value_at(base, at as u64 - 1)?
                }
            }
            Expr::Unary { op, rhs, span } => {
                let v = self.value_at(rhs, obs)?;
                let x = v.as_real().ok_or_else(|| err::type_mismatch().at(*span))?;
                Value::Real(match op {
                    UnOp::Neg if is_missing(x) => x,
                    UnOp::Neg => -x,
                    UnOp::Pos => x,
                    UnOp::Not if is_missing(x) => SYSMISS,
                    UnOp::Not => f64::from(u8::from(x == 0.0)),
                })
            }
            Expr::Binary { op, lhs, rhs, span } => {
                let a = self.value_at(lhs, obs)?;
                let b = self.value_at(rhs, obs)?;
                binary(*op, &a, &b).ok_or_else(|| err::type_mismatch().at(*span))?
            }
            Expr::Call { name, args, span } => {
                let mut vs = Vec::with_capacity(args.len());
                for a in args {
                    vs.push(self.value_at(a, obs)?);
                }
                call(name, &vs).ok_or_else(|| err::type_mismatch().at(*span))?
            }
            Expr::Stored { class, key, span } => {
                let name = match &**key {
                    Expr::Name(n, _) => n.clone(),
                    Expr::Str(s, _) => s.clone(),
                    other => return Err(err::invalid("stored").at(other.span())),
                };
                match self.stored(*class, &name) {
                    Some(ScalarValue::Num { value, .. }) => Value::Real(value),
                    Some(ScalarValue::Str { value }) => Value::Str(value),
                    // Stata answers an unset `r()` with missing, not an error.
                    None if *class != StoredClass::C => Value::Real(SYSMISS),
                    None => return Err(err::invalid(&name).at(*span)),
                }
            }
            other => return Err(err::invalid("expression").at(other.span())),
        })
    }
}

fn read_cell(frame: &Frame, idx: VarIdx, obs: u64) -> Value {
    let var = frame.var(idx).expect("resolved index");
    let col = frame.col(idx).expect("a variable has a column");
    match var.ty {
        StorageType::Str { .. } | StorageType::StrL => {
            Value::Str(String::from_utf8_lossy(col.get_bytes(obs).unwrap_or_default()).into_owned())
        }
        _ => Value::Real(col.get_f64(obs).unwrap_or(SYSMISS)),
    }
}

fn binary(op: BinOp, a: &Value, b: &Value) -> Option<Value> {
    if let (Value::Str(x), Value::Str(y)) = (a, b) {
        return Some(match op {
            BinOp::Add => Value::Str(format!("{x}{y}")),
            BinOp::Eq => Value::bool(x == y),
            BinOp::Ne => Value::bool(x != y),
            BinOp::Lt => Value::bool(x < y),
            BinOp::Le => Value::bool(x <= y),
            BinOp::Gt => Value::bool(x > y),
            BinOp::Ge => Value::bool(x >= y),
            _ => return None,
        });
    }
    let (x, y) = (a.as_real()?, b.as_real()?);
    // Comparisons are total over the raw doubles: Stata's missing values ARE
    // the largest doubles, which is why `5 > .` is 0 with no special case.
    Some(match op {
        BinOp::Eq => Value::bool(x == y),
        BinOp::Ne => Value::bool(x != y),
        BinOp::Lt => Value::bool(x < y),
        BinOp::Le => Value::bool(x <= y),
        BinOp::Gt => Value::bool(x > y),
        BinOp::Ge => Value::bool(x >= y),
        _ if is_missing(x) || is_missing(y) => Value::Real(SYSMISS),
        BinOp::Add => Value::Real(x + y),
        BinOp::Sub => Value::Real(x - y),
        BinOp::Mul => Value::Real(x * y),
        BinOp::Div if y == 0.0 => Value::Real(SYSMISS),
        BinOp::Div => Value::Real(x / y),
        // `stratum_core::math`, not `f64::powf`: ADR-004 pins every
        // transcendental to libm so the same expression is bit-identical on
        // all three OSes. A fixture that used the host's libm would make this
        // file's expectations platform-dependent, which is the one thing the
        // determinism hash exists to prevent.
        BinOp::Pow => Value::Real(math::powf(x, y)),
        BinOp::And => Value::bool(x != 0.0 && y != 0.0),
        BinOp::Or => Value::bool(x != 0.0 || y != 0.0),
    })
}

fn call(name: &str, args: &[Value]) -> Option<Value> {
    let num = |i: usize| args.get(i).and_then(Value::as_real);
    Some(match name {
        "missing" => Value::bool(match args.first()? {
            Value::Real(v) => is_missing(*v),
            Value::Str(s) => s.is_empty(),
        }),
        "log" | "ln" => Value::Real(math::ln(num(0)?)),
        "exp" => Value::Real(math::exp(num(0)?)),
        "sqrt" => Value::Real(math::sqrt(num(0)?)),
        "abs" => Value::Real(num(0)?.abs()),
        "int" => Value::Real(num(0)?.trunc()),
        "round" => {
            let (x, to) = (num(0)?, num(1).unwrap_or(1.0));
            Value::Real((x / to).round() * to)
        }
        "length" => Value::Real(match args.first()? {
            Value::Str(s) => s.chars().count() as f64,
            Value::Real(v) => stratum_core::fmt::fmt_macro(*v).len() as f64,
        }),
        "substr" => {
            let s = match args.first()? {
                Value::Str(s) => s.clone(),
                Value::Real(_) => return None,
            };
            let start = num(1)? as usize;
            let len = num(2)? as usize;
            Value::Str(s.chars().skip(start.saturating_sub(1)).take(len).collect())
        }
        "string" => Value::Str(stratum_core::fmt::fmt_macro(num(0)?)),
        _ => return None,
    })
}

impl CmdHost for TestHost {
    fn frames(&self) -> &FrameSet {
        &self.frames
    }

    fn frames_mut(&mut self) -> &mut FrameSet {
        &mut self.frames
    }

    fn edit_var_meta(&mut self, idx: VarIdx, edit: VarMetaEdit) -> Result<(), StataError> {
        self.asks.push(Ask::MetaEdit(idx, edit));
        Ok(())
    }

    fn data_source(&self) -> Option<&str> {
        Some("/w/auto.dta")
    }

    fn clear_data_source(&mut self) {
        // The fixture's provenance is hardwired; the tests that exercise
        // `clear` assert the frame, not the header line.
    }

    fn data_timestamp(&self) -> Option<&str> {
        Some("13 Apr 2022 17:45")
    }

    fn settings(&self) -> &Settings {
        &self.settings
    }

    fn settings_mut(&mut self) -> &mut Settings {
        &mut self.settings
    }

    fn expr_type(&mut self, e: &Expr) -> Result<EvalType, StataError> {
        Ok(match self.value_at(e, self.obs)? {
            Value::Real(_) => EvalType::Numeric,
            Value::Str(_) => EvalType::Str,
        })
    }

    fn eval_scalar(&mut self, e: &Expr) -> Result<Value, StataError> {
        self.value_at(e, self.obs)
    }

    fn eval_num_rows(
        &mut self,
        e: &Expr,
        row0: u64,
        len: usize,
        out: &mut Vec<f64>,
    ) -> Result<(), StataError> {
        self.counts.eval_calls += 1;
        self.counts.eval_rows += len as u64;
        for r in row0..row0 + len as u64 {
            out.push(self.value_at(e, r)?.as_real().unwrap_or(SYSMISS));
        }
        Ok(())
    }

    fn eval_str_rows(
        &mut self,
        e: &Expr,
        row0: u64,
        len: usize,
        out: &mut Vec<String>,
    ) -> Result<(), StataError> {
        self.counts.eval_calls += 1;
        self.counts.eval_rows += len as u64;
        for r in row0..row0 + len as u64 {
            out.push(match self.value_at(e, r)? {
                Value::Str(s) => s,
                Value::Real(v) => stratum_core::fmt::fmt_macro(v),
            });
        }
        Ok(())
    }

    fn emit(&mut self, runs: &[StyledRun]) {
        self.counts.emits += 1;
        self.counts.max_runs_per_emit = self.counts.max_runs_per_emit.max(runs.len());
        if self.quiet {
            return;
        }
        self.batches.push(runs.len());
        self.out.extend_from_slice(runs);
    }

    fn quiet(&self) -> bool {
        self.quiet
    }

    fn clear_r(&mut self) {
        self.r.clear();
    }

    fn set_r(&mut self, name: &str, v: ScalarValue) {
        self.r.retain(|(n, _)| n != name);
        self.r.push((name.to_owned(), v));
    }

    fn stored(&self, class: StoredClass, name: &str) -> Option<ScalarValue> {
        if class == StoredClass::C {
            return cmd::settings::creturn(self, name);
        }
        let table = match class {
            StoredClass::R => &self.r,
            StoredClass::E => &self.e,
            _ => &self.s,
        };
        table
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.clone())
    }

    fn stored_names(&self, class: StoredClass) -> Vec<String> {
        let table = match class {
            StoredClass::R => &self.r,
            StoredClass::E => &self.e,
            StoredClass::S => &self.s,
            StoredClass::C => {
                return cmd::settings::C_NAMES
                    .iter()
                    .map(|s| (*s).to_owned())
                    .collect()
            }
        };
        table.iter().map(|(n, _)| n.clone()).collect()
    }

    fn set_local(&mut self, name: &str, value: &str) {
        self.locals_log.push((name.to_owned(), value.to_owned()));
        self.locals.insert(name.to_owned(), value.to_owned());
    }

    fn set_global(&mut self, name: &str, value: &str) {
        self.globals.insert(name.to_owned(), value.to_owned());
    }

    fn get_macro(&self, global: bool, name: &str) -> Option<String> {
        let t = if global { &self.globals } else { &self.locals };
        t.get(name).cloned()
    }

    fn run_body(&mut self, _body: stratum_proto::Span) -> Result<(), StataError> {
        self.body_runs += 1;
        match self.body_rcs.pop_front().unwrap_or(0) {
            0 => Ok(()),
            // The message is empty for the control-flow signals, exactly as
            // `control.rs` raises them; a real failure carries its own text
            // from the command that raised it.
            rc => Err(StataError::new(rc, "")),
        }
    }

    fn last_rc(&self) -> u32 {
        self.last_rc
    }

    fn set_last_rc(&mut self, rc: u32) {
        self.last_rc = rc;
    }

    fn load_dta(&mut self, path: &Utf8Path, clear: bool) -> Result<LoadReport, StataError> {
        if !self.files.iter().any(|f| f == path) {
            return Err(err::file_not_found(path.as_str()));
        }
        self.asks.push(Ask::LoadDta(path.to_owned(), clear));
        Ok(LoadReport {
            label: "1978 automobile data".to_owned(),
        })
    }

    fn save_dta(&mut self, path: &Utf8Path, replace: bool) -> Result<(), StataError> {
        self.asks.push(Ask::SaveDta(path.to_owned(), replace));
        Ok(())
    }

    fn sysuse_path(&mut self, name: &str) -> Result<Utf8PathBuf, StataError> {
        Ok(Utf8PathBuf::from(format!("/ado/base/a/{name}.dta")))
    }

    fn erase_file(&mut self, path: &Utf8Path) -> Result<(), StataError> {
        if !self.files.iter().any(|f| f == path) {
            return Err(err::file_not_found(path.as_str()));
        }
        self.asks.push(Ask::Erase(path.to_owned()));
        Ok(())
    }

    fn cwd(&self) -> &Utf8Path {
        &self.cwd
    }

    fn set_cwd(&mut self, path: &Utf8Path) -> Result<(), StataError> {
        self.asks.push(Ask::SetCwd(path.to_owned()));
        self.cwd = path.to_owned();
        Ok(())
    }

    fn file_exists(&mut self, path: &Utf8Path) -> bool {
        self.files.iter().any(|f| f == path)
    }

    fn run_stat(&mut self, req: &StatRequest) -> CmdResult {
        self.asks.push(Ask::Stat(format!(
            "{} vars={:?} n={} opts={:?}",
            req.cmd,
            req.vars,
            req.sample.len(),
            req.options
        )));
        Ok(cmd::CmdOutcome::text_only())
    }

    fn implements(&self, cmd: &str) -> bool {
        IMPLEMENTED.contains(&cmd)
    }

    fn implements_option(&self, cmd: &str, opt: &str) -> bool {
        !self.unimplemented_options.contains(&format!("{cmd}:{opt}"))
    }
}

/// The canonical command name for a parsed line.
fn canonical_name(ast: &CommandAst, line: &str) -> String {
    use stratum_parse::ast::command::Command;
    match &ast.cmd {
        Command::Known(k) => stratum_parse::cmdtable::command(k.id).canonical.to_owned(),
        _ => line.split_whitespace().next().unwrap_or("").to_owned(),
    }
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// The first five rows of `auto.dta`, as the golden `list` block shows them.
///
/// `price` and `mpg` are `int` and `make` is `str13`, which is what
/// `stratum_data` can express today. `auto.dta` gives `price` the display
/// format `%8.0gc` and `make` the format `%-18s`; neither can be set from
/// outside `stratum-data` (see the module note), so the cells here render as
/// `4099` rather than the golden's `4,099`.
fn auto5() -> Frame {
    let mut f = Frame::new("default");
    f.set_label("1978 automobile data");
    f.add_column(
        "make",
        str_col(
            13,
            &[
                "AMC Concord",
                "AMC Pacer",
                "AMC Spirit",
                "Buick Century",
                "Buick Electra",
            ],
        ),
    )
    .expect("make");
    f.add_column(
        "price",
        Column::Int(NumCol::from_slice(&[4099i16, 4749, 3799, 4816, 7827])),
    )
    .expect("price");
    f.add_column(
        "mpg",
        Column::Int(NumCol::from_slice(&[22i16, 17, 22, 20, 15])),
    )
    .expect("mpg");
    f.mark_saved();
    f
}

/// A 74-row frame whose `foreign` has 22 ones — the shape `count`, `count if`
/// and `assert` are asserted against in `core_surface.log` and `errors.log`.
fn auto74() -> Frame {
    let mut f = Frame::new("default");
    let price: Vec<i16> = (0..74).map(|i| 3291 + i * 10).collect();
    let foreign: Vec<i8> = (0..74).map(|i| i8::from(i >= 52)).collect();
    f.add_column("price", Column::Int(NumCol::from_slice(&price)))
        .expect("price");
    f.add_column("foreign", Column::Byte(NumCol::from_slice(&foreign)))
        .expect("foreign");
    f.mark_saved();
    f
}

/// A fixed-width string column built through the write barrier, which is the
/// only route `stratum_data` offers into `FixedStrCol`.
fn str_col(width: u16, values: &[&str]) -> Column {
    let mut f = Frame::new("scratch");
    let idx = f
        .add_column(
            "v",
            Column::new_missing(StorageType::Str { width }, values.len() as u64),
        )
        .expect("scratch column");
    {
        let mut cm = f.col_mut(idx).expect("scratch column is writable");
        for (row, v) in values.iter().enumerate() {
            cm.set_bytes(row as u64, v.as_bytes()).expect("fits width");
        }
    }
    f.col(idx).expect("just added").clone()
}

/// One golden log, read from the committed capture.
fn golden(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/golden/stata18")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// The lines of a golden between the line that equals `after` and the next
/// line that starts with `". "` — i.e. one command's output.
fn golden_block<'a>(log: &'a str, after: &str) -> Vec<&'a str> {
    let mut lines = log.lines().skip_while(|l| l.trim_end() != after);
    lines.next();
    lines
        .take_while(|l| !l.starts_with(". ") && *l != ".")
        .collect()
}

// ---------------------------------------------------------------------------
// The table
// ---------------------------------------------------------------------------

#[test]
fn the_builtin_table_and_the_implemented_list_are_the_same_set() {
    // `CommandRegistry::implements` is built from IMPLEMENTED, and the exit-10
    // path and the result card's quick actions both trust it. A name in one
    // list and not the other is a command that dispatch runs but the registry
    // denies, or the reverse.
    for name in IMPLEMENTED {
        assert!(
            builtin(name).is_some(),
            "IMPLEMENTED lists {name:?} but `builtin` has no entry for it"
        );
    }
    let mut sorted = IMPLEMENTED.to_vec();
    sorted.sort_unstable();
    assert_eq!(
        sorted, *IMPLEMENTED,
        "IMPLEMENTED must stay sorted so a merge conflict is a conflict, not a silent duplicate"
    );
    sorted.dedup();
    assert_eq!(
        sorted.len(),
        IMPLEMENTED.len(),
        "IMPLEMENTED has a duplicate"
    );
}

#[test]
fn an_unimplemented_command_is_absent_rather_than_wrong() {
    // Absent means exit 10 ("we have not written it"), which the CLI keeps
    // distinct from r(199) ("that is not a command"). `merge` is a real Stata
    // command that Pass 1 does not implement.
    assert!(builtin("merge").is_none());
    assert!(builtin("egen").is_none());
    assert!(builtin("nosuchcommand").is_none());
}

// ---------------------------------------------------------------------------
// count — GOLDEN core_surface.log
// ---------------------------------------------------------------------------

#[test]
fn count_matches_the_golden_bytes() {
    let mut h = TestHost::new(auto74());
    h.run("count").expect("count");
    // GOLDEN core_surface.log: `. count` prints "  74".
    assert_eq!(h.text(), "  74\n");
    assert_eq!(
        h.stored(StoredClass::R, "N"),
        Some(ScalarValue::Num {
            value: 74.0,
            display: "74".to_owned()
        })
    );

    h.run("count if foreign == 1").expect("count if");
    // GOLDEN core_surface.log: `. count if foreign == 1` prints "  22".
    assert_eq!(h.text(), "  22\n");
}

#[test]
fn count_evaluates_only_the_rows_the_in_range_selects() {
    // The counter the `build_sample` doc comment claims: `in` bounds the rows
    // the `if` is evaluated over, so this is 40 evaluations and not 74.
    let mut h = TestHost::new(auto74());
    h.run("count if foreign == 1 in 1/40").expect("count");
    assert_eq!(
        h.counts.eval_rows, 40,
        "the `in` range bounds the `if` scan"
    );
    assert_eq!(h.text(), "  0\n");
}

// ---------------------------------------------------------------------------
// list
// ---------------------------------------------------------------------------

#[test]
fn list_renders_the_golden_geometry() {
    let mut h = TestHost::new(auto5());
    h.run("list make price mpg in 1/5").expect("list");
    // Widths: make 13 (Buick Century), price 5 (the header), mpg 3 (the
    // header). Gutter 3. Rule 1 + 21 + 3*2 + 1 = 29, exactly the golden's.
    let want = "\n     +-----------------------------+\n     \
                | make            price   mpg |\n     \
                |-----------------------------|\n  \
                1. | AMC Concord      4099    22 |\n  \
                2. | AMC Pacer        4749    17 |\n  \
                3. | AMC Spirit       3799    22 |\n  \
                4. | Buick Century    4816    20 |\n  \
                5. | Buick Electra    7827    15 |\n     \
                +-----------------------------+\n";
    assert_eq!(h.text(), want);
}

#[test]
fn the_golden_list_block_follows_the_same_geometry_rule() {
    // The golden's own `list make price mpg in 1/5` cannot be reproduced from
    // a synthesised frame (`price` needs `%8.0gc`), so what is checked here is
    // the RULE: the border length the renderer computes from a set of column
    // widths is the border length StataMP printed.
    let log = golden("core_surface.log");
    let block = golden_block(&log, ". list make price mpg in 1/5");
    let rows: Vec<&str> = block
        .iter()
        .copied()
        .filter(|l| !l.trim().is_empty())
        .collect();
    let top = rows.first().expect("the golden block has a top border");
    let dashes = top.trim().trim_matches('+').len();
    // Cell texts, straight out of the golden's own data rows.
    let widths = [
        "Buick Century".len(), // make
        "4,099".len(),         // price, %8.0gc
        "mpg".len(),           // mpg, header-wide
    ];
    let inner = 1 + widths.iter().sum::<usize>() + 3 * (widths.len() - 1) + 1;
    assert_eq!(dashes, inner, "border = 1 + widths + 3*(k-1) + 1");
    // Gutter: `max(3, digits(max obs))`, and the border is indented by
    // gutter + 2.
    assert_eq!(top.len() - top.trim_start().len(), 3 + 2);
}

#[test]
fn list_streams_and_does_not_buffer_the_whole_table() {
    // Design 03 §9.4. The counter: the largest batch handed to `emit` does not
    // grow with the number of rows, so a 10 M-row `list` never builds a
    // 10 M-row buffer. Two sizes, one bound.
    let small = list_emit_profile(1_000);
    let large = list_emit_profile(5_000);
    assert_eq!(
        small.max_runs_per_emit, large.max_runs_per_emit,
        "the buffer high-water mark is independent of _N"
    );
    assert!(
        large.emits > small.emits,
        "more rows means more emits, not a bigger one: {small:?} vs {large:?}"
    );
}

#[test]
fn list_coalesces_runs_rather_than_emitting_one_per_cell() {
    // `Out::push` merges same-style neighbours, which is what keeps a
    // ResultEnvelope small enough to coalesce inside the frame budget.
    let mut h = TestHost::new(auto5());
    h.run("list make price mpg in 1/5").expect("list");
    // Two runs per column per row — the value in `{res}` and the structure
    // around it in `{txt}` — plus the borders and the header. The number that
    // matters is that it scales with COLUMNS, never with characters: an
    // uncoalesced renderer emits a run per pad, per separator and per border
    // character, which is an order of magnitude more.
    let runs = h.runs().len();
    let rows = 5;
    let cols = 3;
    assert!(
        runs <= 2 * cols * rows + 8,
        "{rows} rows x {cols} columns coalesced to {runs} runs"
    );
    // And inside one emitted batch no two adjacent runs share a style — that IS
    // the coalescing. The check is per batch and not over the concatenation
    // because `Out` cannot merge across an emit: `list` hands the sink a batch
    // and starts a fresh builder, so the run closing batch *n* and the run
    // opening batch *n+1* arrive unmerged. That seam costs one run per batch —
    // it scales with 256-row batches, never with characters — and asserting it
    // away over the whole stream would only be satisfiable by buffering the
    // whole table, which is the thing design 03 §9.4 forbids.
    let mut at = 0usize;
    let mut seams = 0usize;
    for (b, len) in h.batches.iter().copied().enumerate() {
        let batch = &h.runs()[at..at + len];
        for pair in batch.windows(2) {
            assert_ne!(
                pair[0].style, pair[1].style,
                "batch {b}: adjacent runs with the same style were not merged: {pair:?}"
            );
        }
        if at > 0 && h.runs()[at - 1].style == batch[0].style {
            seams += 1;
        }
        at += len;
    }
    assert_eq!(at, h.runs().len(), "every run belongs to an emitted batch");
    assert!(
        seams < h.batches.len(),
        "a same-style seam is allowed only at a batch boundary: {seams} seams \
         across {} batches",
        h.batches.len()
    );
}

fn list_emit_profile(rows: u64) -> Counts {
    let mut f = Frame::new("default");
    let v: Vec<i32> = (0..rows as i32).collect();
    f.add_column("x", Column::Long(NumCol::from_slice(&v)))
        .expect("x");
    let mut h = TestHost::new(f);
    h.run("list x").expect("list");
    h.counts
}

// ---------------------------------------------------------------------------
// describe
// ---------------------------------------------------------------------------

#[test]
fn describe_renders_the_header_and_the_variable_table() {
    let mut h = TestHost::new(auto5());
    h.run("describe").expect("describe");
    let text = h.text();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "");
    assert_eq!(lines[1], "Contains data from /w/auto.dta");
    assert_eq!(
        lines[2],
        " Observations:             5                  1978 automobile data"
    );
    assert_eq!(
        lines[3],
        "    Variables:             3                  13 Apr 2022 17:45"
    );
    assert_eq!(lines[4], "-".repeat(80));
    assert_eq!(lines[5], "Variable      Storage   Display    Value");
    assert_eq!(
        lines[6],
        "    name         type    format    label      Variable label"
    );
    // A synthesised frame carries the default format for its type and no
    // label, so the row is the layout with empty trailing fields.
    assert_eq!(lines[7].trim_end(), "make            str13   %9s");
    assert_eq!(lines[7].len(), 46, "the label column starts at 46");
    assert_eq!(lines[8].trim_end(), "price           int     %8.0g");
    assert_eq!(lines[9].trim_end(), "mpg             int     %8.0g");
    assert_eq!(lines[10], "-".repeat(80));
    assert_eq!(lines[11], "Sorted by: ");
    assert_eq!(
        h.stored(StoredClass::R, "k"),
        Some(ScalarValue::Num {
            value: 3.0,
            display: "3".to_owned()
        })
    );
}

#[test]
fn the_golden_describe_puts_every_field_where_this_renderer_does() {
    // The four field widths and LABEL_COL, checked against StataMP's own bytes
    // rather than against the constants that produced ours.
    let log = golden("core_surface.log");
    let block = golden_block(&log, ". describe");
    let rows: Vec<&str> = block
        .iter()
        .copied()
        .skip_while(|l| !l.starts_with("make"))
        .take_while(|l| !l.starts_with('-'))
        .collect();
    assert_eq!(rows.len(), 12, "auto.dta has 12 variables");
    for row in rows {
        // name in [0,16), type in [16,24), format in [24,35), value label in
        // [35,46), variable label from 46.
        assert!(row.len() > 46, "{row:?}");
        assert_eq!(&row[16..24], pad_to(&row[16..24], 8), "{row:?}");
        let ty = row[16..24].trim_end();
        assert!(
            !ty.is_empty() && row[16..17] != *" ",
            "storage type starts at column 16: {row:?}"
        );
        assert_eq!(
            &row[24..25],
            "%",
            "display format starts at column 24: {row:?}"
        );
        let label = &row[46..];
        assert!(
            !label.starts_with(' '),
            "variable label starts at column 46: {row:?}"
        );
    }
    // And the header lines are the ones this renderer prints, verbatim.
    assert!(block.contains(&"Variable      Storage   Display    Value"));
    assert!(block.contains(&"    name         type    format    label      Variable label"));
}

fn pad_to(s: &str, n: usize) -> String {
    format!("{s:<n$}")
}

#[test]
fn describe_of_a_varlist_omits_the_header_block() {
    // GOLDEN core_surface.log `. describe mileage` prints the two heading
    // lines, one rule, and the row — no "Contains data", no "Sorted by:".
    let mut h = TestHost::new(auto5());
    h.run("describe price").expect("describe price");
    let text = h.text();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "");
    assert_eq!(lines[1], "Variable      Storage   Display    Value");
    assert_eq!(
        lines[2],
        "    name         type    format    label      Variable label"
    );
    assert_eq!(lines[3], "-".repeat(80));
    assert_eq!(lines[4].trim_end(), "price           int     %8.0g");
    assert_eq!(lines.len(), 5);
}

#[test]
fn describe_notes_an_unsaved_change() {
    // GOLDEN core_surface.log, after `keep make price`:
    //     Sorted by:
    //          Note: Dataset has changed since last saved.
    let mut h = TestHost::new(auto5());
    h.run("drop mpg").expect("drop");
    h.run("describe").expect("describe");
    let text = h.text();
    assert!(
        text.contains("     Note: Dataset has changed since last saved.\n"),
        "{text}"
    );
}

// ---------------------------------------------------------------------------
// display — GOLDEN core_surface.log, "===== display expressions ====="
// ---------------------------------------------------------------------------

#[test]
fn display_reproduces_the_golden_expression_block() {
    // Twelve lines of StataMP output, byte for byte. This block is the whole
    // reason `DEFAULT_WIDTH` is 10 and not the manual's 9.
    let cases: &[(&str, &str)] = &[
        ("di 2 + 3 * 4", "14\n"),
        ("di log(exp(1))", "1\n"),
        ("di sqrt(16)", "4\n"),
        ("di round(3.14159, 0.01)", "3.14\n"),
        ("di 1/0", ".\n"),
        ("di .", ".\n"),
        ("di missing(.)", "1\n"),
        ("di 5 > .", "0\n"),
        ("di (\"abc\" + \"def\")", "abcdef\n"),
        ("di length(\"hello\")", "5\n"),
        ("di substr(\"hello\", 2, 3)", "ell\n"),
    ];
    for (src, want) in cases {
        let mut h = TestHost::new(auto5());
        h.run(src).unwrap_or_else(|e| panic!("{src}: {e:?}"));
        assert_eq!(h.text(), *want, "{src}");
    }
}

#[test]
fn display_of_a_stored_result_uses_the_width_the_golden_proves() {
    // GOLDEN core_surface.log:
    //     . di "`v' mean = " r(mean)
    //     price mean = 6165.2568
    //     mpg mean = 21.297297
    // `%9.0g` would print 6165.257. The machine says otherwise, so
    // DEFAULT_WIDTH is 10.
    let mut h = TestHost::new(auto5());
    h.set_scalar_r("mean", 6_165.256_756_756_76);
    h.run("di \"price mean = \" r(mean)").expect("display");
    assert_eq!(h.text(), "price mean = 6165.2568\n");

    h.clear_r();
    h.set_scalar_r("mean", 21.297_297_297_297_3);
    h.run("di \"mpg mean = \" r(mean)").expect("display");
    assert_eq!(h.text(), "mpg mean = 21.297297\n");
}

#[test]
fn display_styles_text_and_result_separately() {
    // Classic text is a Vec<StyledRun>, not a String: a literal is `{res}` by
    // default and `as text` switches the class. The CLI flattens with
    // `styled::to_plain`, so the bytes are the same either way — the runs are
    // what the result card colours.
    let mut h = TestHost::new(auto5());
    h.run("di as text \"n = \" as result 5").expect("display");
    assert_eq!(h.text(), "n = 5\n");
    let runs = h.runs();
    assert_eq!(runs[0].style, StyleId::Text);
    assert_eq!(runs[0].text, "n = ");
    assert_eq!(runs[1].style, StyleId::Result);
    assert_eq!(runs[1].text, "5");
    assert_eq!(runs[2].style, StyleId::Text);
    assert_eq!(runs[2].text, "\n");
}

#[test]
fn display_honours_an_explicit_format_and_the_no_advance_directive() {
    let mut h = TestHost::new(auto5());
    h.run("di %9.2f 3.14159").expect("display");
    assert_eq!(
        h.text(),
        "     3.14\n",
        "an explicit format keeps its field"
    );

    let mut h = TestHost::new(auto5());
    h.run("di \"a\" _n \"b\"").expect("display");
    assert_eq!(h.text(), "a\nb\n");

    let mut h = TestHost::new(auto5());
    h.run("di \"a\" _c").expect("display");
    assert_eq!(h.text(), "a", "_continue suppresses the trailing newline");

    let mut h = TestHost::new(auto5());
    h.run("di _col(5) \"x\"").expect("display");
    assert_eq!(h.text(), "    x\n");

    let mut h = TestHost::new(auto5());
    h.run("di \"ab\" _skip(3) \"cd\"").expect("display");
    assert_eq!(h.text(), "ab   cd\n");
}

#[test]
fn a_display_directive_this_build_lacks_is_rc10_rather_than_silence() {
    // `_dup(#)` repeats the previous item. Emitting nothing for it would print
    // `x` where StataMP prints `xxx` — a wrong answer that no error surfaces.
    // rc 10 keeps "we have not written it" distinct from "you are wrong".
    let mut h = TestHost::new(auto5());
    let e = h.run("di \"x\" _dup(3)").expect_err("rc 10");
    assert_eq!(e.rc, 10);
    assert_eq!(
        e.message,
        "unsupported in this version: display directive _dup(3)"
    );
    assert_eq!(cmd::to_diagnostic(&e).code, "STRATUM0010");
    assert!(e.span.is_some());
}

// ---------------------------------------------------------------------------
// return list / ereturn list — GOLDEN core_surface.log
// ---------------------------------------------------------------------------

#[test]
fn return_list_reproduces_the_golden_block() {
    let mut h = TestHost::new(auto5());
    // The exact r() set `summarize mpg` leaves, with the exact doubles the
    // golden prints back.
    for (name, v) in [
        ("N", 74.0),
        ("sum_w", 74.0),
        ("mean", 21.297_297_297_297_3),
        ("Var", 33.472_047_389_855_61),
        ("sd", 5.785_503_209_735_141),
        ("min", 12.0),
        ("max", 41.0),
        ("sum", 1576.0),
    ] {
        h.set_scalar_r(name, v);
    }
    h.run("return list").expect("return list");
    // Written as lines and joined, because a `\` continuation inside a Rust
    // string literal eats the next line's leading whitespace — and the leading
    // whitespace IS the layout being asserted.
    let want = [
        "",
        "scalars:",
        "                  r(N) =  74",
        "              r(sum_w) =  74",
        "               r(mean) =  21.2972972972973",
        "                r(Var) =  33.47204738985561",
        "                 r(sd) =  5.785503209735141",
        "                r(min) =  12",
        "                r(max) =  41",
        "                r(sum) =  1576",
        "",
    ]
    .join("\n");
    assert_eq!(h.text(), want);

    // And the same bytes are in the golden. Compared as one string rather than
    // line by line: `str::lines` drops the final empty line that the trailing
    // `\n` implies, so a line-indexed loop reads one past the end of `h.text()`
    // for output that — correctly — ends in a newline. `block.join("\n")`
    // reconstitutes the golden's own bytes exactly, blank lines included.
    let log = golden("core_surface.log");
    let block = golden_block(&log, ". return list");
    assert_eq!(
        h.text(),
        block.join("\n"),
        "`return list` must be StataMP's bytes, not merely its lines"
    );
}

#[test]
fn return_list_prints_in_insertion_order_not_hash_order() {
    // `r(N)` before `r(mean)` is part of the output contract; a hash iteration
    // would reorder it per run and break the byte comparison above.
    let mut h = TestHost::new(auto5());
    for name in cmd::estimation_glue::SUMMARIZE_R {
        h.set_scalar_r(name, 1.0);
    }
    h.run("return list").expect("return list");
    let names: Vec<String> = h
        .text()
        .lines()
        .filter_map(|l| l.trim().split(" =").next().map(str::to_owned))
        .filter(|l| l.starts_with("r("))
        .collect();
    let want: Vec<String> = cmd::estimation_glue::SUMMARIZE_R
        .iter()
        .map(|n| format!("r({n})"))
        .collect();
    assert_eq!(names, want);
}

#[test]
fn ereturn_list_separates_scalars_from_macros() {
    // GOLDEN core_surface.log: scalars are joined by " =  ", macros by " : "
    // with the value quoted.
    let mut h = TestHost::new(auto5());
    h.e.push((
        "N".to_owned(),
        ScalarValue::Num {
            value: 74.0,
            display: "74".to_owned(),
        },
    ));
    h.e.push((
        "cmdline".to_owned(),
        ScalarValue::Str {
            value: "regress price mpg weight foreign".to_owned(),
        },
    ));
    h.run("ereturn list").expect("ereturn list");
    assert_eq!(
        h.text(),
        "\nscalars:\n                  e(N) =  74\n\nmacros:\n            e(cmdline) : \"regress price mpg weight foreign\"\n"
    );
    let log = golden("core_surface.log");
    assert!(log.contains("                  e(N) =  74"));
    assert!(log.contains("            e(cmdline) : \"regress price mpg weight foreign\""));
}

// ---------------------------------------------------------------------------
// assert / confirm — GOLDEN errors.log
// ---------------------------------------------------------------------------

#[test]
fn assert_prints_the_contradiction_count_and_fails_with_r9() {
    let mut h = TestHost::new(auto74());
    let e = h
        .run("assert price > 100000")
        .expect_err("assert must fail");
    // GOLDEN errors.log:
    //     74 contradictions in 74 observations
    //     assertion is false
    //     rc = 9
    assert_eq!(h.text(), "74 contradictions in 74 observations\n");
    assert_eq!(e.rc, 9);
    assert_eq!(e.message, "assertion is false");

    let mut h = TestHost::new(auto74());
    h.run("assert price > 0")
        .expect("a true assertion is silent");
    assert_eq!(h.text(), "");
}

#[test]
fn assert_singularises_its_counts() {
    let mut f = Frame::new("default");
    f.add_column("x", Column::Byte(NumCol::from_slice(&[0i8])))
        .expect("x");
    let mut h = TestHost::new(f);
    h.run("assert x > 0").expect_err("fails");
    assert_eq!(h.text(), "1 contradiction in 1 observation\n");
}

#[test]
fn confirm_answers_every_case_the_golden_lists() {
    // GOLDEN errors.log, "----- confirm".
    let mut h = TestHost::new(auto5());
    h.run("confirm variable price").expect("rc = 0");

    let e = h.run("confirm variable nosuchvar").expect_err("rc = 111");
    assert_eq!(
        (e.rc, e.message.as_str()),
        (111, "variable nosuchvar not found")
    );
    assert_eq!(e.offending_token.as_deref(), Some("nosuchvar"));

    let e = h.run("confirm numeric variable make").expect_err("rc = 7");
    assert_eq!(
        (e.rc, e.message.as_str()),
        (7, "'make' found where numeric variable expected")
    );
    assert_eq!(e.offending_token.as_deref(), Some("make"));

    let e = h.run("confirm new variable price").expect_err("rc = 110");
    assert_eq!(
        (e.rc, e.message.as_str()),
        (110, "variable price already defined")
    );

    h.run("confirm new variable brandnew").expect("rc = 0");
    h.run("confirm string variable make").expect("rc = 0");
    let e = h.run("confirm string variable price").expect_err("rc = 7");
    assert_eq!(e.message, "'price' found where string variable expected");
}

// ---------------------------------------------------------------------------
// generate / replace, and the write-barrier counter
// ---------------------------------------------------------------------------

#[test]
fn a_replace_over_many_rows_bumps_the_version_exactly_once() {
    // The plan's write-barrier bullet, expressed at the COMMAND layer:
    // `Frame::col_mut` bumps on every call, so "one bump per command commit"
    // is the same statement as "`manip::replace` acquires `col_mut` once".
    // Asserted at two sizes, because a per-chunk acquisition would scale.
    for rows in [50u64, 5_000] {
        let mut f = Frame::new("default");
        let v: Vec<i32> = (0..rows as i32).collect();
        f.add_column("x", Column::Long(NumCol::from_slice(&v)))
            .expect("x");
        f.mark_saved();
        let mut h = TestHost::new(f);
        let before = h.frames().current().version();
        h.run("replace x = x + 1").expect("replace");
        let after = h.frames().current().version();
        assert_eq!(
            after.0 - before.0,
            1,
            "{rows} rows must be one version bump, not one per element or per chunk"
        );
        assert_eq!(
            h.frames().current().col(VarIdx(0)).expect("x").get_f64(0),
            Some(1.0)
        );
    }
}

#[test]
fn generate_creates_a_column_and_reports_the_missing_count() {
    let mut h = TestHost::new(auto5());
    h.run("gen double half = price / 2").expect("generate");
    let frame = h.frames().current();
    let idx = frame.index_of("half").expect("half exists");
    assert_eq!(frame.var(idx).expect("half").ty, StorageType::Double);
    assert_eq!(frame.col(idx).expect("half").get_f64(0), Some(2049.5));
    assert_eq!(h.text(), "", "a generate with no missings says nothing");

    // GOLDEN core_surface.log's `gen` on a variable with missings prints
    // `(N missing values generated)`.
    let mut h = TestHost::new(auto5());
    h.run("gen double r = price / (mpg - 22)")
        .expect("generate");
    assert_eq!(h.text(), "(2 missing values generated)\n");
}

#[test]
fn replace_reports_the_number_of_real_changes() {
    // GOLDEN core_surface.log: `. replace hi = 0 if mpg > 30` → "(0 real
    // changes made)".
    let mut h = TestHost::new(auto5());
    h.run("replace mpg = 0 if mpg > 30").expect("replace");
    assert_eq!(h.text(), "(0 real changes made)\n");

    let mut h = TestHost::new(auto5());
    h.run("replace mpg = mpg + 1").expect("replace");
    assert_eq!(h.text(), "(5 real changes made)\n");
}

#[test]
fn generate_and_replace_raise_the_golden_error_codes() {
    // GOLDEN errors.log, every case in this directory's half of the file.
    let mut h = TestHost::new(auto5());

    let e = h.run("gen price = 1").expect_err("r(110)");
    assert_eq!(
        (e.rc, e.message.as_str()),
        (110, "variable price already defined")
    );
    assert_eq!(e.offending_token.as_deref(), Some("price"));

    let e = h.run("replace nosuchvar = 1").expect_err("r(111)");
    assert_eq!(
        (e.rc, e.message.as_str()),
        (111, "variable nosuchvar not found")
    );
    assert_eq!(e.offending_token.as_deref(), Some("nosuchvar"));

    let e = h.run("gen x = \"text\" + 1").expect_err("r(109)");
    assert_eq!((e.rc, e.message.as_str()), (109, "type mismatch"));

    let e = h.run("replace price = 1 if nosuchvar").expect_err("r(111)");
    assert_eq!(
        (e.rc, e.message.as_str()),
        (111, "nosuchvar not found"),
        "an expression-position name has no leading `variable `"
    );
    assert_eq!(e.offending_token.as_deref(), Some("nosuchvar"));

    let e = h.run("drop nosuchvar").expect_err("r(111)");
    assert_eq!(
        (e.rc, e.message.as_str()),
        (111, "variable nosuchvar not found")
    );

    let e = h.run("rename nosuchvar other").expect_err("r(111)");
    assert_eq!(
        (e.rc, e.message.as_str()),
        (111, "variable nosuchvar not found")
    );
}

#[test]
fn drop_and_keep_are_complements() {
    let mut h = TestHost::new(auto5());
    h.run("drop mpg").expect("drop");
    assert_eq!(h.frames().current().n_vars(), 2);
    assert!(h.frames().current().index_of("mpg").is_none());

    let mut h = TestHost::new(auto5());
    h.run("keep make price").expect("keep");
    assert_eq!(h.frames().current().n_vars(), 2);
    assert!(h.frames().current().index_of("mpg").is_none());
}

#[test]
fn drop_if_deletes_observations_and_says_how_many() {
    // GOLDEN core_surface.log: `. drop if price > 10000` → "(10 observations
    // deleted)".
    let mut h = TestHost::new(auto5());
    h.run("drop if price > 4800").expect("drop if");
    assert_eq!(h.text(), "(2 observations deleted)\n");
    assert_eq!(h.frames().current().n_obs(), 3);
}

#[test]
fn rename_keeps_the_variable_identity() {
    // The plan's rename bullet, at the layer this directory owns: the column
    // keeps its VarId, so a downstream block keyed on identity is unaffected
    // while one keyed on the NAME goes Broken.
    let mut h = TestHost::new(auto5());
    let id_before = h.frames().current().var(VarIdx(2)).expect("mpg").id;
    h.run("rename mpg mileage").expect("rename");
    let frame = h.frames().current();
    assert!(frame.index_of("mpg").is_none());
    let idx = frame.index_of("mileage").expect("mileage exists");
    assert_eq!(frame.var(idx).expect("mileage").id, id_before);
}

// ---------------------------------------------------------------------------
// Sorting
// ---------------------------------------------------------------------------

#[test]
fn sort_orders_ascending_and_records_the_keys() {
    let mut h = TestHost::new(auto5());
    h.run("sort price").expect("sort");
    let frame = h.frames().current();
    let col = frame.col(VarIdx(1)).expect("price");
    assert_eq!(col.get_f64(0), Some(3799.0));
    assert_eq!(col.get_f64(4), Some(7827.0));
    assert!(frame.sort_state().valid);

    h.run("describe").expect("describe");
    assert!(h.text().contains("Sorted by: price"), "{}", h.text());
}

#[test]
fn gsort_takes_a_leading_minus_for_descending() {
    let mut h = TestHost::new(auto5());
    h.run("gsort -price").expect("gsort");
    let frame = h.frames().current();
    assert_eq!(
        frame.col(VarIdx(1)).expect("price").get_f64(0),
        Some(7827.0)
    );
    assert!(
        !frame.sort_state().valid,
        "a descending order is not a Stata `sort`, so `sorted by` must not claim it"
    );
}

// ---------------------------------------------------------------------------
// io
// ---------------------------------------------------------------------------

/// A rendered path with the host's directory separator folded to `/`.
///
/// `cmd::io::resolve` anchors a relative filename with `Utf8Path::join`, and
/// `join` spells the separator the HOST's way: `/w/out.dta` on Unix,
/// `/w\out.dta` on Windows. The `Ask` and `cwd` assertions below compare
/// `Utf8PathBuf` values, which camino compares component-wise, so they need no
/// help. These are the ones that genuinely have to compare STRINGS — they
/// assert rendered classic output and r(601)/r(602) tokens, byte for byte —
/// so the separator is normalised at the boundary instead. The fold is
/// lossless for these fixtures: every path here is built from `/w`,
/// `/elsewhere` and plain ASCII filenames, so a `\` in the rendered text can
/// only have come from `join` and never from a filename of its own.
fn slashes(s: &str) -> String {
    s.replace('\\', "/")
}

#[test]
fn the_separator_fold_is_what_makes_a_windows_render_comparable() {
    // The mechanism the four tests below trip over, exercised on ANY host:
    // Windows renders the same resolved path with `\`, and only the spelling
    // differs. Reproducing it here means macOS still describes what Windows
    // does, rather than passing by accident because `join` picked `/`.
    assert_eq!(
        slashes("file /w\\out.dta already exists"), // as Windows renders it
        "file /w/out.dta already exists"
    );
    assert_eq!(
        slashes("/w/out.dta"),
        "/w/out.dta",
        "a Unix render is untouched"
    );
}

#[test]
fn use_announces_the_dataset_label_and_routes_through_the_host() {
    let mut h = TestHost::new(auto5());
    h.files.push(Utf8PathBuf::from("/w/auto.dta"));
    h.run("use auto.dta, clear").expect("use");
    // GOLDEN core_surface.log: `. sysuse auto, clear` → "(1978 automobile data)".
    assert_eq!(h.text(), "(1978 automobile data)\n");
    assert_eq!(
        h.asks,
        vec![Ask::LoadDta(Utf8PathBuf::from("/w/auto.dta"), true)],
        "a relative path is resolved against the RECORDED cwd, never std::env"
    );
}

#[test]
fn use_of_a_missing_file_is_r601_with_the_path_as_the_token() {
    // GOLDEN errors.log: `use /no/such/file.dta, clear` → r(601).
    let mut h = TestHost::new(auto5());
    let e = h.run("use /no/such/file.dta, clear").expect_err("r(601)");
    assert_eq!(
        (e.rc, e.message.as_str()),
        (601, "file /no/such/file.dta not found")
    );
    assert_eq!(e.offending_token.as_deref(), Some("/no/such/file.dta"));
}

#[test]
fn save_refuses_to_overwrite_without_replace() {
    let mut h = TestHost::new(auto5());
    h.files.push(Utf8PathBuf::from("/w/out.dta"));
    let e = h.run("save out.dta").expect_err("r(602)");
    // `file_exists` matched despite the separator — it compares `Utf8Path`
    // values. Only the RENDERED message and token are string-shaped, so those
    // go through `slashes`.
    let msg = slashes(e.message.as_str());
    assert_eq!(
        (e.rc, msg.as_str()),
        (602, "file /w/out.dta already exists")
    );
    assert_eq!(
        e.offending_token.as_deref().map(slashes).as_deref(),
        Some("/w/out.dta")
    );

    h.run("save out.dta, replace").expect("replace given");
    assert_eq!(slashes(&h.text()), "file /w/out.dta saved as .dta format\n");
}

#[test]
fn save_with_replace_and_no_file_says_so_first() {
    // GOLDEN core_surface.log:
    //     (file /var/…/St62681.000003 not found)
    //     file /var/…/St62681.000003 saved as .dta format
    let mut h = TestHost::new(auto5());
    h.run("save fresh.dta, replace").expect("save");
    // Rendered classic output, so a string compare; `/w\fresh.dta` on Windows
    // is the same resolved path, folded at the boundary.
    assert_eq!(
        slashes(&h.text()),
        "(file /w/fresh.dta not found)\nfile /w/fresh.dta saved as .dta format\n"
    );
}

/// A `CommandAst` carrying `text` in its REST slot.
///
/// This helper was written because `data/commands.ron` had no row for `cd`,
/// `pwd`, `erase` or `creturn`, which made those four unreachable from a
/// do-file: the parser produced an unknown command whose argument sat in a
/// slot `cmd::slots` does not read, so `cd /somewhere` ran as a bare `cd`.
/// The rows have since landed and
/// [`every_implemented_command_is_reachable_through_the_parser`] holds the
/// line. The helper stays because building one AST beats parsing a line in
/// tests that are about a command's behaviour rather than its grammar.
fn rest_ast(text: &str) -> CommandAst {
    let (ast, _diags) = parse_command(&format!("display {text}"), ParseMode::Execute);
    ast
}

/// Every command this crate implements must be one the PARSER knows.
///
/// A command absent from `data/commands.ron` still dispatches — `builtin` is
/// keyed by name — but it parses to an unknown command, and an unknown
/// command's argument text lands where [`cmd::slots`] does not look. The
/// command then runs as though it had been given no argument: `cd /somewhere`
/// reported the directory it had not changed to, and `cd` to a directory that
/// does not exist answered rc 0. Four commands were in exactly that state.
///
/// Every other test in this file builds its AST with [`rest_ast`], which is
/// why none of them could see it. This one goes through the real parser.
#[test]
fn every_implemented_command_is_reachable_through_the_parser() {
    let table = stratum_parse::table();
    for name in IMPLEMENTED {
        assert!(
            table.canonical_id(name).is_some(),
            "`{name}` is implemented but the parser's table does not contain it, \
             so its arguments are unreachable and it runs as a bare command. \
             Add a row to crates/stratum-parse/data/commands.ron."
        );
    }
}

#[test]
fn cd_and_pwd_report_the_recorded_directory() {
    let mut h = TestHost::new(auto5());
    cmd::io::pwd(&mut h, &rest_ast("")).expect("pwd");
    assert_eq!(h.text(), "/w\n");

    h.out.clear();
    cmd::io::cd(&mut h, &rest_ast("/elsewhere")).expect("cd");
    assert_eq!(slashes(&h.text()), "/elsewhere\n");
    assert_eq!(h.cwd, Utf8PathBuf::from("/elsewhere"));
    assert_eq!(
        h.asks,
        vec![Ask::SetCwd(Utf8PathBuf::from("/elsewhere"))],
        "cd goes through the recorded door, never std::env::set_current_dir"
    );

    // A relative `cd` composes with the recorded cwd. `pwd` renders whatever
    // `join` produced, which is `/elsewhere\sub` on Windows — the same
    // directory, spelled with the host's separator, so fold before comparing.
    h.out.clear();
    cmd::io::cd(&mut h, &rest_ast("sub")).expect("cd sub");
    assert_eq!(slashes(&h.text()), "/elsewhere/sub\n");
    assert_eq!(h.cwd, Utf8PathBuf::from("/elsewhere/sub"));

    // A bare `cd` is `pwd`.
    h.out.clear();
    cmd::io::cd(&mut h, &rest_ast("")).expect("bare cd");
    assert_eq!(slashes(&h.text()), "/elsewhere/sub\n");
}

#[test]
fn erase_is_silent_on_success_and_r601_on_a_missing_file() {
    let mut h = TestHost::new(auto5());
    h.files.push(Utf8PathBuf::from("/w/gone.dta"));
    cmd::io::erase(&mut h, &rest_ast("gone.dta")).expect("erase");
    assert_eq!(h.text(), "", "Stata says nothing when the erase succeeds");
    assert_eq!(h.asks, vec![Ask::Erase(Utf8PathBuf::from("/w/gone.dta"))]);

    let e = cmd::io::erase(&mut h, &rest_ast("nosuch.dta")).expect_err("r(601)");
    // The `Ask` above compares as a path, so it needed nothing; the message and
    // the token are strings the user reads, and `/w\nosuch.dta` is how Windows
    // spells the same anchoring.
    let msg = slashes(e.message.as_str());
    assert_eq!((e.rc, msg.as_str()), (601, "file /w/nosuch.dta not found"));
    assert_eq!(
        e.offending_token.as_deref().map(slashes).as_deref(),
        Some("/w/nosuch.dta")
    );
    assert!(
        e.span.is_some(),
        "the host has no span; the command attaches one"
    );
}

// ---------------------------------------------------------------------------
// The statistics seam
// ---------------------------------------------------------------------------

#[test]
fn summarize_hands_the_stats_crate_a_request_with_no_parsing_left_in_it() {
    let mut h = TestHost::new(auto5());
    h.run("summarize price mpg in 1/3").expect("summarize");
    assert_eq!(
        h.asks,
        vec![Ask::Stat("summarize vars=[1, 2] n=3 opts=[]".to_owned())]
    );
}

#[test]
fn summarize_with_no_varlist_takes_every_variable() {
    let mut h = TestHost::new(auto5());
    h.run("summarize").expect("summarize");
    assert_eq!(
        h.asks,
        vec![Ask::Stat("summarize vars=[0, 1, 2] n=5 opts=[]".to_owned())]
    );
}

#[test]
fn an_option_the_command_does_not_have_is_r198_as_typed() {
    // GOLDEN errors.log: `summarize price, detial` → "option detial not
    // allowed", NOT "option detail not allowed".
    let mut h = TestHost::new(auto5());
    let e = h.run("summarize price, detial").expect_err("r(198)");
    assert_eq!(
        (e.rc, e.message.as_str()),
        (198, "option detial not allowed")
    );
    assert_eq!(e.offending_token.as_deref(), Some("detial"));

    let e = h.run("summarize price, nosuchoption").expect_err("r(198)");
    assert_eq!(
        (e.rc, e.message.as_str()),
        (198, "option nosuchoption not allowed")
    );
}

#[test]
fn an_option_this_build_has_not_written_is_rc10_not_r198() {
    // "We are incomplete" must stay distinct from "you are wrong" (plan §W09).
    let mut h = TestHost::new(auto5());
    h.unimplemented_options.push("summarize:detail".to_owned());
    let e = h.run("summarize price, detail").expect_err("rc 10");
    assert_eq!(e.rc, 10);
    assert_eq!(
        e.message,
        "unsupported in this version: summarize, option detail"
    );
    assert_eq!(e.offending_token.as_deref(), Some("detail"));
    assert_eq!(cmd::to_diagnostic(&e).code, "STRATUM0010");
}

#[test]
fn ttest_without_by_or_a_comparison_is_r100() {
    // GOLDEN errors.log: `ttest price` → "by() option required", rc 100.
    let mut h = TestHost::new(auto5());
    let e = h.run("ttest price").expect_err("r(100)");
    assert_eq!((e.rc, e.message.as_str()), (100, "by() option required"));
    assert_eq!(e.offending_token.as_deref(), Some("by() option"));
    h.run("ttest price, by(mpg)").expect("by() given");
}

#[test]
fn summarize_of_an_unknown_name_is_r111_with_the_token() {
    // GOLDEN errors.log: the single most common error in the product.
    let mut h = TestHost::new(auto5());
    let e = h.run("summarize nosuchvar").expect_err("r(111)");
    assert_eq!(
        (e.rc, e.message.as_str()),
        (111, "variable nosuchvar not found")
    );
    assert_eq!(e.offending_token.as_deref(), Some("nosuchvar"));
}

#[test]
fn predict_with_no_option_assumes_xb_and_says_so() {
    // GOLDEN core_surface.log and errors.log both carry this line.
    let mut h = TestHost::new(auto5());
    h.run("predict pricehat").expect("predict");
    assert_eq!(h.text(), "(option xb assumed; fitted values)\n");

    // And naming ANY statistic suppresses it. GOLDEN core_surface.log:
    //     . predict resid, residuals
    //     (nothing)
    // and extended_surface.log adds `. predict stdp_hat, stdp` — also silent.
    // The note says "assumed", so it is wrong to print it when nothing was.
    for line in [
        "predict resid, residuals",
        "predict s, stdp",
        "predict x, xb",
    ] {
        let mut h = TestHost::new(auto5());
        h.run(line).unwrap_or_else(|e| panic!("{line}: {e:?}"));
        assert_eq!(h.text(), "", "{line} must be silent");
    }
}

// ---------------------------------------------------------------------------
// The offending-token invariant
// ---------------------------------------------------------------------------

#[test]
fn every_error_this_directory_raises_carries_its_offending_token() {
    // The plan calls a missing `offending_token` on an r(111)/r(198)/r(199) a
    // MERGE BLOCKER: it is the field spec §21 turns into "Did you mean
    // 'income'?". Driven through the commands rather than asserted on the
    // constructors, so a command that drops the token on the way out fails.
    let lines: &[&str] = &[
        "summarize nosuchvar",
        "count if nosuchvar > 1",
        "list nosuchvar",
        "describe nosuchvar",
        "gen price = 1",
        "replace nosuchvar = 1",
        "drop nosuchvar",
        "keep nosuchvar",
        "rename nosuchvar other",
        "sort nosuchvar",
        "correlate price nosuchvar",
        "tabulate nosuchvar",
        "summarize price, detial",
        "list in 0",
        "use /no/such/file.dta, clear",
        "confirm variable nosuchvar",
        "confirm numeric variable make",
    ];
    for line in lines {
        let mut h = TestHost::new(auto5());
        let e = h.run(line).expect_err(&format!("{line} must fail"));
        assert!(
            matches!(e.rc, 7 | 110 | 111 | 198 | 199 | 601),
            "{line}: unexpected rc {}",
            e.rc
        );
        assert!(
            e.offending_token.is_some(),
            "{line} raised r({}) `{}` with no offending_token — the plan calls this a merge blocker",
            e.rc,
            e.message
        );
        assert!(
            e.span.is_some(),
            "{line} raised r({}) with no span; the editor cannot underline it",
            e.rc
        );
    }
}

// ---------------------------------------------------------------------------
// Control flow — `control::run_block`
// ---------------------------------------------------------------------------
//
// `run_block` is reached from W06a's `dispatch.rs`, not from `builtin`, so it
// is driven here by constructing the `BlockCommand` the parser would have
// produced. What is asserted is the whole of this module's contract at that
// seam: which values the loop variable takes, in what order, and how many times
// the body is asked to run — plus the two return codes that are control flow
// rather than failure.

use stratum_parse::ast::command::{BlockCommand, ForeachSource, NumRange, RawArgs};
use stratum_runtime::cmd::control::{self, RC_BREAK, RC_CONTINUE};

/// A body span. `run_body` is the host's, so the extent never matters here —
/// only that the same one comes back on every pass.
const BODY: stratum_proto::Span = stratum_proto::Span { start: 0, end: 1 };

/// A varlist parsed from `src`.
///
/// `parse_varlist` takes the span as the SLICE of `src` to read, not merely as
/// an offset for error reporting, so the span has to cover the whole text —
/// a `BODY`-sized one silently parses the first character and nothing else.
fn varlist(src: &str) -> stratum_parse::ast::varlist::VarList {
    stratum_parse::varlist::parse_varlist(
        src,
        stratum_proto::Span {
            start: 0,
            end: src.len() as u32,
        },
    )
}

fn forvalues(from: f64, step: Option<f64>, to: f64) -> BlockCommand {
    BlockCommand::Forvalues {
        loopvar: "i".to_owned(),
        range: NumRange { from, step, to },
        body: BODY,
    }
}

/// The values a block wrote to its loop variable, in order.
fn loop_values(h: &TestHost) -> Vec<&str> {
    h.locals_log.iter().map(|(_, v)| v.as_str()).collect()
}

#[test]
fn forvalues_counts_its_passes_rather_than_accumulating_the_step() {
    let mut h = TestHost::new(auto5());
    control::run_block(&mut h, &forvalues(1.0, None, 3.0)).expect("forvalues 1/3");
    assert_eq!(loop_values(&h), ["1", "2", "3"]);
    assert_eq!(h.body_runs, 3);

    // The reason the count is computed rather than accumulated: `v += 0.1` ten
    // times lands on 0.9999999999999999 and runs the body ten times, not
    // eleven, and does so differently under different optimisation levels.
    let mut h = TestHost::new(auto5());
    control::run_block(&mut h, &forvalues(0.0, Some(0.1), 1.0)).expect("forvalues 0(0.1)1");
    assert_eq!(
        h.body_runs, 11,
        "0(0.1)1 is eleven passes on every platform"
    );
    assert_eq!(loop_values(&h).first().copied(), Some("0"));
    assert_eq!(loop_values(&h).last().copied(), Some("1"));
}

#[test]
fn a_backwards_forvalues_range_is_empty_and_not_one_pass() {
    // `forvalues i = 1/\`n'` with an empty `n` is how a do-file says "skip
    // this". Clamping the pass count to zero and then looping `0..=0` would run
    // the body once, with the loop variable at the START of the range — a
    // silent extra iteration that no error would ever surface.
    for (from, step, to) in [(5.0, None, 1.0), (1.0, Some(-1.0), 5.0), (1.0, None, 0.0)] {
        let mut h = TestHost::new(auto5());
        control::run_block(&mut h, &forvalues(from, step, to)).expect("empty range");
        assert_eq!(
            h.body_runs, 0,
            "forvalues i = {from}({step:?}){to} must not run"
        );
        assert!(h.locals_log.is_empty());
    }

    // A descending range that DOES run is unaffected.
    let mut h = TestHost::new(auto5());
    control::run_block(&mut h, &forvalues(10.0, Some(-2.0), 6.0)).expect("descending");
    assert_eq!(loop_values(&h), ["10", "8", "6"]);
}

#[test]
fn a_zero_step_is_r198_rather_than_a_hang() {
    let mut h = TestHost::new(auto5());
    let e = control::run_block(&mut h, &forvalues(1.0, Some(0.0), 10.0)).expect_err("r(198)");
    assert_eq!(e.rc, 198);
    assert_eq!(h.body_runs, 0);
}

#[test]
fn foreach_iterates_the_source_it_was_given() {
    // `in` is a whitespace split of the text as typed.
    let mut h = TestHost::new(auto5());
    control::run_block(
        &mut h,
        &BlockCommand::Foreach {
            loopvar: "v".to_owned(),
            source: ForeachSource::In(RawArgs {
                text: "  alpha   beta gamma ".to_owned(),
                span: BODY,
            }),
            body: BODY,
        },
    )
    .expect("foreach in");
    assert_eq!(loop_values(&h), ["alpha", "beta", "gamma"]);

    // `of varlist` resolves against the live frame, so it yields the frame's
    // spelling of each name and raises r(111) for one that is not there.
    let mut h = TestHost::new(auto5());
    control::run_block(
        &mut h,
        &BlockCommand::Foreach {
            loopvar: "v".to_owned(),
            source: ForeachSource::OfVarlist(varlist("price mpg")),
            body: BODY,
        },
    )
    .expect("foreach of varlist");
    assert_eq!(loop_values(&h), ["price", "mpg"]);

    // `of local` reads the macro through the host, because the local it wants
    // belongs to the CURRENT call frame.
    let mut h = TestHost::new(auto5());
    h.locals.insert("L".to_owned(), "one two".to_owned());
    control::run_block(
        &mut h,
        &BlockCommand::Foreach {
            loopvar: "v".to_owned(),
            source: ForeachSource::OfLocal("L".to_owned()),
            body: BODY,
        },
    )
    .expect("foreach of local");
    assert_eq!(loop_values(&h), ["one", "two"]);
}

#[test]
fn foreach_over_a_name_the_frame_does_not_have_is_r111() {
    let mut h = TestHost::new(auto5());
    let e = control::run_block(
        &mut h,
        &BlockCommand::Foreach {
            loopvar: "v".to_owned(),
            source: ForeachSource::OfVarlist(varlist("nosuchvar")),
            body: BODY,
        },
    )
    .expect_err("r(111)");
    assert_eq!(e.rc, 111);
    assert_eq!(e.offending_token.as_deref(), Some("nosuchvar"));
    assert_eq!(h.body_runs, 0);
}

#[test]
fn continue_skips_a_pass_and_continue_break_ends_the_loop() {
    // Both travel as return codes through `run_body`; there is no second error
    // channel, and the codes are above every real Stata one so a user's
    // r(198) can never be mistaken for either.
    let mut h = TestHost::new(auto5());
    h.body_rcs = [0, RC_CONTINUE, 0].into_iter().collect();
    control::run_block(&mut h, &forvalues(1.0, None, 3.0)).expect("continue is swallowed");
    assert_eq!(h.body_runs, 3, "a `continue` skips the rest of ONE pass");
    assert_eq!(loop_values(&h), ["1", "2", "3"]);

    let mut h = TestHost::new(auto5());
    h.body_rcs = [0, RC_BREAK].into_iter().collect();
    control::run_block(&mut h, &forvalues(1.0, None, 9.0)).expect("break ends the loop");
    assert_eq!(h.body_runs, 2, "a `break` abandons the whole loop");
}

#[test]
fn a_real_failure_inside_a_loop_propagates() {
    // The complement of the test above: only the two signals are swallowed.
    let mut h = TestHost::new(auto5());
    h.body_rcs = [0, 111].into_iter().collect();
    let e = control::run_block(&mut h, &forvalues(1.0, None, 9.0)).expect_err("r(111) escapes");
    assert_eq!(e.rc, 111);
    assert_eq!(h.body_runs, 2, "the loop stops at the failure");
}

#[test]
fn while_runs_until_its_condition_is_false() {
    // The condition is re-evaluated per pass, so a body that never changes it
    // would loop forever — which is exactly Stata, and exactly why the
    // cancellation ladder and not a pass cap is what stops a runaway loop.
    // Here the body raises `break` on the third pass to end it.
    let mut h = TestHost::new(auto5());
    h.body_rcs = [0, 0, RC_BREAK].into_iter().collect();
    control::run_block(
        &mut h,
        &BlockCommand::While {
            cond: Expr::Num(1.0, BODY),
            body: BODY,
        },
    )
    .expect("while");
    assert_eq!(h.body_runs, 3);

    // A condition that is false at entry runs the body zero times.
    let mut h = TestHost::new(auto5());
    control::run_block(
        &mut h,
        &BlockCommand::While {
            cond: Expr::Num(0.0, BODY),
            body: BODY,
        },
    )
    .expect("while 0");
    assert_eq!(h.body_runs, 0);
}

#[test]
fn if_else_takes_exactly_one_arm() {
    // Arms are tried in order and the first true one wins; a trailing `None`
    // condition is the `else`.
    let cases: &[(&[f64], bool, u32)] = &[
        (&[0.0, 1.0], false, 1),
        (&[1.0, 1.0], false, 1),
        (&[0.0, 0.0], false, 0),
        (&[0.0, 0.0], true, 1),
    ];
    for (conds, has_else, want) in cases {
        let mut arms: Vec<(Option<Expr>, stratum_proto::Span)> = conds
            .iter()
            .map(|c| (Some(Expr::Num(*c, BODY)), BODY))
            .collect();
        if *has_else {
            arms.push((None, BODY));
        }
        let mut h = TestHost::new(auto5());
        control::run_block(&mut h, &BlockCommand::IfElse { arms }).expect("if/else");
        assert_eq!(h.body_runs, *want, "conds={conds:?} else={has_else}");
    }
}

#[test]
fn capture_records_the_return_code_and_lets_the_signals_through() {
    // `capture summarize nosuchvar` leaves `_rc == 111` and no diagnostic;
    // `capture summarize price` leaves `_rc == 0` — GOLDEN errors.log.
    let mut h = TestHost::new(auto5());
    h.set_last_rc(7);
    control::capture_result(&mut h, h_ok()).expect("success resets _rc");
    assert_eq!(h.last_rc(), 0);

    let mut h = TestHost::new(auto5());
    control::capture_result(&mut h, Err(err::var_not_found("nosuchvar")))
        .expect("capture swallows the failure");
    assert_eq!(h.last_rc(), 111);

    // A `continue` inside a `capture`d command is control flow, not a failure:
    // eating it would strand the loop.
    let mut h = TestHost::new(auto5());
    let e = control::capture_result(&mut h, Err(StataError::new(RC_CONTINUE, "")))
        .expect_err("a signal is not captured");
    assert_eq!(e.rc, RC_CONTINUE);
    assert!(control::is_signal(e.rc));
    assert!(
        !control::is_signal(111),
        "a real return code is not a signal"
    );
}

fn h_ok() -> CmdResult {
    Ok(stratum_runtime::cmd::CmdOutcome::text_only())
}

#[test]
fn a_capture_block_records_the_body_s_return_code() {
    let mut h = TestHost::new(auto5());
    h.body_rcs = [601].into_iter().collect();
    control::run_block(&mut h, &BlockCommand::Capture { body: BODY }).expect("capture swallows");
    assert_eq!(h.last_rc(), 601);
    assert_eq!(h.body_runs, 1);
}

// ---------------------------------------------------------------------------
// ds — GOLDEN semantics.log
// ---------------------------------------------------------------------------

/// The twelve `auto.dta` variable names, in storage order.
const AUTO_NAMES: &[&str] = &[
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
];

fn auto_names_frame() -> Frame {
    let mut f = Frame::new("default");
    for n in AUTO_NAMES {
        f.add_column(n, Column::Byte(NumCol::from_slice(&[0i8])))
            .unwrap_or_else(|e| panic!("{n}: {e:?}"));
    }
    f.mark_saved();
    f
}

#[test]
fn ds_fills_down_each_column_before_moving_right() {
    // Column width is the longest name plus two, the grid is as wide as the
    // line allows, and the cells fill DOWN each column — so the first row is
    // not the first k names. At the pinned 80 columns that is three rows of
    // four.
    let mut h = TestHost::new(auto_names_frame());
    h.run("ds").expect("ds");
    assert_eq!(
        h.text(),
        "make          rep78         weight        displacement\n\
         price         headroom      length        gear_ratio\n\
         mpg           trunk         turn          foreign\n"
    );
    // No trailing blanks: the last cell in a row carries no pad, so a short
    // final row cannot end in the padding of a column it has no name in.
    for line in h.text().lines() {
        assert_eq!(line, line.trim_end(), "{line:?} has trailing blanks");
    }
}

#[test]
fn the_golden_ds_block_is_column_major_too() {
    // StataMP's own `ds` on `auto.dta`, captured at `set linesize 100` — a
    // width A16 rejects, so the shipped path cannot reproduce these bytes and
    // what is checked is the RULE they follow. Reading DOWN the golden's
    // columns must give storage order; reading across (the layout a naive
    // renderer produces) must not.
    let log = golden("semantics.log");
    let block: Vec<&str> = golden_block(&log, ". ds")
        .into_iter()
        .filter(|l| !l.trim().is_empty())
        .collect();
    let grid: Vec<Vec<&str>> = block
        .iter()
        .map(|l| l.split_whitespace().collect())
        .collect();
    let rows = grid.len();
    let ncols = grid[0].len();
    let mut down = Vec::new();
    for c in 0..ncols {
        for row in grid.iter().take(rows) {
            if let Some(n) = row.get(c) {
                down.push(*n);
            }
        }
    }
    assert_eq!(down, AUTO_NAMES, "the golden fills down, then right");

    // Same width rule: the longest name plus two, and as many columns as fit
    // in 100. `displacement` is 12, so the pitch is 14 and 100/14 is 7 —
    // which, balanced over two rows, is the six columns StataMP printed.
    let w = AUTO_NAMES.iter().map(|n| n.len()).max().expect("names") + 2;
    assert_eq!(w, 14);
    let max_cols = 100 / w;
    assert_eq!(rows, AUTO_NAMES.len().div_ceil(max_cols));
    assert_eq!(ncols, AUTO_NAMES.len().div_ceil(rows));
    // And the pitch is visible in the golden's own bytes.
    assert_eq!(block[0].find("mpg"), Some(w));
}

// ---------------------------------------------------------------------------
// label / format — the metadata commands, and the W02 seam
// ---------------------------------------------------------------------------
//
// `stratum_data::Frame` has no route to a variable's `label`, `format` or
// `value_label` (no `var_mut`), so these commands hand the edit to
// `CmdHost::edit_var_meta` and the host applies it. What this directory owns is
// the argument grammar, the error paths and the SHAPE of the edit — which is
// what these tests pin, so they keep passing unchanged when the accessor lands.
// Escalated in W06c's return.

/// The metadata edits the last command asked for.
fn meta_edits(h: &TestHost) -> Vec<(VarIdx, VarMetaEdit)> {
    h.asks
        .iter()
        .filter_map(|a| match a {
            Ask::MetaEdit(i, e) => Some((*i, e.clone())),
            _ => None,
        })
        .collect()
}

#[test]
fn label_variable_keeps_the_quoted_text_whole() {
    // GOLDEN core_surface.log:
    //     . label variable mileage "Miles per gallon"
    // The label is ONE argument with spaces in it, so the `rest` slot cannot be
    // `split_whitespace`d.
    let mut h = TestHost::new(auto5());
    h.run("label variable mpg \"Miles per gallon\"")
        .expect("label variable");
    assert_eq!(
        meta_edits(&h),
        vec![(VarIdx(2), VarMetaEdit::Label("Miles per gallon".to_owned()))]
    );
    assert_eq!(h.text(), "", "Stata says nothing");

    let e = h.run("label variable nosuchvar \"x\"").expect_err("r(111)");
    assert_eq!(
        (e.rc, e.message.as_str()),
        (111, "variable nosuchvar not found")
    );
    assert_eq!(e.offending_token.as_deref(), Some("nosuchvar"));
}

#[test]
fn label_values_may_name_a_table_that_does_not_exist_yet() {
    // GOLDEN errors.log:
    //     ----- label values price nosuchlabel
    //     rc = 0
    // The attachment is allowed to point at a table defined later, and `.`
    // detaches.
    let mut h = TestHost::new(auto5());
    h.run("label values price nosuchlabel").expect("rc = 0");
    assert_eq!(
        meta_edits(&h),
        vec![(
            VarIdx(1),
            VarMetaEdit::ValueLabel(Some("nosuchlabel".to_owned()))
        )]
    );

    let mut h = TestHost::new(auto5());
    h.run("label values price .").expect("detach");
    assert_eq!(
        meta_edits(&h),
        vec![(VarIdx(1), VarMetaEdit::ValueLabel(None))]
    );
}

#[test]
fn label_data_and_label_define_write_the_frame_directly() {
    // These two need no `var_mut`: the dataset label and the value-label set
    // are the frame's own, so they land now rather than through the shim.
    let mut h = TestHost::new(auto5());
    h.run("label data \"New label\"").expect("label data");
    assert_eq!(h.frames().current().label(), "New label");

    h.run("label define origin 0 \"Domestic\" 1 \"Foreign\"")
        .expect("label define");
    let frame = h.frames().current();
    let table = frame.labels().get("origin").expect("origin defined");
    assert_eq!(table.get(0.0), Some("Domestic"));
    assert_eq!(table.get(1.0), Some("Foreign"));

    h.run("label list origin").expect("label list");
    assert_eq!(
        h.text(),
        "origin:\n          0 Domestic\n          1 Foreign\n"
    );

    h.run("label drop origin").expect("label drop");
    assert!(h.frames().current().labels().get("origin").is_none());
}

#[test]
fn format_applies_to_every_name_it_was_given() {
    let mut h = TestHost::new(auto5());
    h.run("format price mpg %9.2f").expect("format");
    let want = stratum_core::fmt::StataFormat::parse("%9.2f").expect("a valid format");
    assert_eq!(
        meta_edits(&h),
        vec![
            (VarIdx(1), VarMetaEdit::Format(want)),
            (VarIdx(2), VarMetaEdit::Format(want)),
        ]
    );

    // Stata accepts the format on either side of the varlist.
    let mut h = TestHost::new(auto5());
    h.run("format %9.2f price").expect("format first");
    assert_eq!(meta_edits(&h), vec![(VarIdx(1), VarMetaEdit::Format(want))]);

    let mut h = TestHost::new(auto5());
    let e = h.run("format nosuchvar %9.2f").expect_err("r(111)");
    assert_eq!(
        (e.rc, e.message.as_str()),
        (111, "variable nosuchvar not found")
    );
    assert_eq!(e.offending_token.as_deref(), Some("nosuchvar"));
    assert!(meta_edits(&h).is_empty(), "a rejected format edits nothing");

    let mut h = TestHost::new(auto5());
    let e = h.run("format price %nonsense").expect_err("r(198)");
    assert_eq!(e.rc, 198);
    assert_eq!(e.offending_token.as_deref(), Some("%nonsense"));
}
