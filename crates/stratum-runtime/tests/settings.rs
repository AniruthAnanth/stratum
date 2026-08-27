//! W06c acceptance — `set` and the `c()` class, and A16 in particular.
//!
//! > **`set linesize` with `n != 80` returns `rc = 10` and `STRATUM0010`**
//! > (A16); `c(linesize)` returns 80 in every code path. Owned by agent 6c.
//!
//! That bullet has two halves and they are tested differently. The rejection is
//! behaviour, so it is driven through the command. "80 in every code path" is a
//! STRUCTURAL claim — [`Settings`] has no `linesize` field, so there is no
//! value to return but 80 — and the tests below try every route to a different
//! answer: a successful `set linesize 80`, a rejected `set linesize 100`, a
//! session whose every other setting has been changed, `c(linesize)` through
//! the evaluator's door, and `creturn list`'s rendering of it.
//!
//! Accepting the setting and then emitting 78-column tables anyway is a silent
//! semantic divergence in a differentially-tested product, and it is the first
//! thing a 20-year Stata user notices on a window resize (spec Scenario C).

use camino::{Utf8Path, Utf8PathBuf};
use stratum_core::Value;
use stratum_data::column::NumCol;
use stratum_data::{Column, Frame, FrameSet, StorageType};
use stratum_parse::ast::expr::{Expr, StoredClass};
use stratum_parse::{parse_command, ParseMode, StataError};
use stratum_proto::{ScalarValue, StyledRun, VarIdx};
use stratum_runtime::cmd::{
    self, builtin,
    settings::{creturn, Settings, C_NAMES, DEFAULT_LEVEL, DEFAULT_SEED, LINESIZE},
    CmdHost, CmdResult, EvalType, LoadReport, StatRequest, VarMetaEdit,
};

// ---------------------------------------------------------------------------
// A host with nothing in it but a frame and the settings
// ---------------------------------------------------------------------------

/// `set` reaches for exactly three things: the settings, the frame (`set obs`)
/// and the output sink. Every other door on [`CmdHost`] is unreachable from
/// this file, and says so rather than pretending to work.
struct SettingsHost {
    frames: FrameSet,
    settings: Settings,
    out: Vec<StyledRun>,
    rc: u32,
}

impl SettingsHost {
    fn new() -> Self {
        let mut frames = FrameSet::new();
        let mut f = Frame::new("default");
        f.add_column("x", Column::Byte(NumCol::from_slice(&[1i8, 2, 3, 4, 5])))
            .expect("x");
        f.mark_saved();
        *frames.current_mut() = f;
        Self {
            frames,
            settings: Settings::default(),
            out: Vec::new(),
            rc: 0,
        }
    }

    fn run(&mut self, line: &str) -> CmdResult {
        let (ast, _diags) = parse_command(line, ParseMode::Execute);
        let name = match &ast.cmd {
            stratum_parse::ast::command::Command::Known(k) => {
                stratum_parse::cmdtable::command(k.id).canonical.to_owned()
            }
            _ => line.split_whitespace().next().unwrap_or("").to_owned(),
        };
        let f = builtin(&name).unwrap_or_else(|| panic!("no builtin for {name:?}"));
        self.out.clear();
        f(self, &ast)
    }

    /// `creturn list` through an equivalent REST-slot AST.
    ///
    /// **ESCALATION.** `crates/stratum-parse/data/commands.ron` (W04's file)
    /// has no row for `creturn`, so `parse_command("creturn list")` cannot
    /// produce a `Command::Known` and the command is unreachable from a
    /// do-file until it does. `settings::creturn_list` ignores its AST, so
    /// this drives exactly the shipped path. Reported in W06c's return.
    fn creturn_list(&mut self) {
        let (ast, _diags) = parse_command("display", ParseMode::Execute);
        self.out.clear();
        cmd::settings::creturn_list(self, &ast).expect("creturn list");
    }

    fn text(&self) -> String {
        stratum_proto::styled::to_plain(&self.out)
    }

    /// `c(name)` through the same door `eval.rs` uses.
    fn c(&self, name: &str) -> Option<ScalarValue> {
        creturn(self, name)
    }

    fn c_num(&self, name: &str) -> f64 {
        match self.c(name) {
            Some(ScalarValue::Num { value, .. }) => value,
            other => panic!("c({name}) is {other:?}, not a number"),
        }
    }

    fn c_str(&self, name: &str) -> String {
        match self.c(name) {
            Some(ScalarValue::Str { value }) => value,
            other => panic!("c({name}) is {other:?}, not a string"),
        }
    }
}

impl CmdHost for SettingsHost {
    fn frames(&self) -> &FrameSet {
        &self.frames
    }
    fn frames_mut(&mut self) -> &mut FrameSet {
        &mut self.frames
    }
    fn edit_var_meta(&mut self, _idx: VarIdx, _edit: VarMetaEdit) -> Result<(), StataError> {
        unreachable!("`set` edits no variable metadata")
    }
    fn data_source(&self) -> Option<&str> {
        None
    }
    fn clear_data_source(&mut self) {
        unreachable!("`set` loads and clears no dataset")
    }
    fn data_timestamp(&self) -> Option<&str> {
        None
    }
    fn settings(&self) -> &Settings {
        &self.settings
    }
    fn settings_mut(&mut self) -> &mut Settings {
        &mut self.settings
    }
    fn expr_type(&mut self, _e: &Expr) -> Result<EvalType, StataError> {
        unreachable!("`set` evaluates no expression")
    }
    fn eval_scalar(&mut self, _e: &Expr) -> Result<Value, StataError> {
        unreachable!("`set` evaluates no expression")
    }
    fn eval_num_rows(
        &mut self,
        _e: &Expr,
        _row0: u64,
        _len: usize,
        _out: &mut Vec<f64>,
    ) -> Result<(), StataError> {
        unreachable!("`set` evaluates no expression")
    }
    fn eval_str_rows(
        &mut self,
        _e: &Expr,
        _row0: u64,
        _len: usize,
        _out: &mut Vec<String>,
    ) -> Result<(), StataError> {
        unreachable!("`set` evaluates no expression")
    }
    fn emit(&mut self, runs: &[StyledRun]) {
        self.out.extend_from_slice(runs);
    }
    fn quiet(&self) -> bool {
        false
    }
    fn clear_r(&mut self) {}
    fn set_r(&mut self, _name: &str, _v: ScalarValue) {}
    fn stored(&self, class: StoredClass, name: &str) -> Option<ScalarValue> {
        if class == StoredClass::C {
            return creturn(self, name);
        }
        None
    }
    fn stored_names(&self, _class: StoredClass) -> Vec<String> {
        Vec::new()
    }
    fn set_local(&mut self, _name: &str, _value: &str) {}
    fn set_global(&mut self, _name: &str, _value: &str) {}
    fn get_macro(&self, _global: bool, _name: &str) -> Option<String> {
        None
    }
    fn run_body(&mut self, _body: stratum_proto::Span) -> Result<(), StataError> {
        unreachable!("`set` runs no body")
    }
    fn last_rc(&self) -> u32 {
        self.rc
    }
    fn set_last_rc(&mut self, rc: u32) {
        self.rc = rc;
    }
    fn load_dta(&mut self, _p: &Utf8Path, _clear: bool) -> Result<LoadReport, StataError> {
        unreachable!("`set` loads nothing")
    }
    fn save_dta(&mut self, _p: &Utf8Path, _replace: bool) -> Result<(), StataError> {
        unreachable!("`set` saves nothing")
    }
    fn sysuse_path(&mut self, _name: &str) -> Result<Utf8PathBuf, StataError> {
        unreachable!("`set` resolves no dataset")
    }
    fn erase_file(&mut self, _p: &Utf8Path) -> Result<(), StataError> {
        unreachable!("`set` erases nothing")
    }
    fn cwd(&self) -> &Utf8Path {
        Utf8Path::new("/w")
    }
    fn set_cwd(&mut self, _p: &Utf8Path) -> Result<(), StataError> {
        unreachable!("`set` does not chdir")
    }
    fn file_exists(&mut self, _p: &Utf8Path) -> bool {
        false
    }
    fn run_stat(&mut self, _req: &StatRequest) -> CmdResult {
        unreachable!("`set` runs no statistic")
    }
    fn implements(&self, name: &str) -> bool {
        cmd::IMPLEMENTED.contains(&name)
    }
    fn implements_option(&self, _cmd: &str, _opt: &str) -> bool {
        true
    }
}

// ---------------------------------------------------------------------------
// A16 — the rejection
// ---------------------------------------------------------------------------

#[test]
fn set_linesize_80_is_the_one_accepted_value() {
    let mut h = SettingsHost::new();
    h.run("set linesize 80")
        .expect("80 is the width v1 renders at");
    assert_eq!(h.text(), "", "an accepted `set` says nothing");
    assert_eq!(h.settings.linesize(), LINESIZE);
}

#[test]
fn every_other_linesize_is_rc10_and_stratum0010() {
    for n in ["100", "79", "81", "132", "0", "255"] {
        let mut h = SettingsHost::new();
        let e = h
            .run(&format!("set linesize {n}"))
            .expect_err("only 80 is accepted");
        assert_eq!(e.rc, 10, "set linesize {n}");
        assert_eq!(
            e.message,
            "unsupported in this version: set linesize other than 80"
        );
        // The token is the VALUE the user typed, not the phrase: that field is
        // what spec §21 turns into "Did you mean 80?".
        assert_eq!(e.offending_token.as_deref(), Some(n));
        assert!(
            e.span.is_some(),
            "the editor has to be able to underline it"
        );
        let d = cmd::to_diagnostic(&e);
        assert_eq!(d.code, "STRATUM0010", "rc 10 is OURS, not a Stata code");
        // And the width the renderers use has not moved.
        assert_eq!(h.settings.linesize(), 80);
    }
}

#[test]
fn a_non_numeric_linesize_is_r198() {
    let mut h = SettingsHost::new();
    let e = h.run("set linesize wide").expect_err("not a number");
    assert_eq!(e.rc, 198);
    assert_eq!(e.offending_token.as_deref(), Some("wide"));
}

// ---------------------------------------------------------------------------
// A16 — "80 in every code path"
// ---------------------------------------------------------------------------

#[test]
fn c_linesize_is_80_however_the_session_was_driven() {
    // Fresh.
    let mut h = SettingsHost::new();
    assert_eq!(h.c_num("linesize"), 80.0);

    // After the one accepted `set`.
    h.run("set linesize 80").expect("accepted");
    assert_eq!(h.c_num("linesize"), 80.0);

    // After a REJECTED `set` — the failure must not have half-applied.
    h.run("set linesize 132").expect_err("rejected");
    assert_eq!(h.c_num("linesize"), 80.0);
    assert_eq!(h.settings.linesize(), 80);

    // After every other setting has been changed.
    h.run("set level 90").expect("level");
    h.run("set more on").expect("more");
    h.run("set varabbrev off").expect("varabbrev");
    h.run("set type double").expect("type");
    h.run("set seed 12345").expect("seed");
    assert_eq!(h.c_num("linesize"), 80.0);

    // And after the settings struct has been rewritten wholesale, which is the
    // strongest form of the claim: there is no field to write.
    h.settings = Settings {
        level: 10.0,
        more: true,
        varabbrev: false,
        gen_type: StorageType::Double,
        seed: 1,
        seed_epoch: 99,
    };
    assert_eq!(h.settings.linesize(), 80);
    assert_eq!(h.c_num("linesize"), 80.0);
}

#[test]
fn creturn_list_prints_linesize_as_80() {
    let mut h = SettingsHost::new();
    h.run("set linesize 100").expect_err("rejected");
    h.creturn_list();
    let text = h.text();
    assert!(
        text.contains("             c(linesize) = 80\n"),
        "creturn list must agree with c(linesize): {text}"
    );
}

// ---------------------------------------------------------------------------
// The rest of the `set` surface
// ---------------------------------------------------------------------------

#[test]
fn set_level_takes_stata_s_range() {
    let mut h = SettingsHost::new();
    assert_eq!(h.settings.level, DEFAULT_LEVEL);
    h.run("set level 90").expect("90 is in range");
    assert_eq!(h.settings.level, 90.0);
    assert_eq!(h.c_num("level"), 90.0);
    for bad in ["9", "100", "abc"] {
        let e = h
            .run(&format!("set level {bad}"))
            .expect_err("out of range");
        assert_eq!(e.rc, 198, "set level {bad}");
        assert_eq!(e.offending_token.as_deref(), Some(bad));
    }
    assert_eq!(h.settings.level, 90.0, "a rejected `set` changes nothing");
}

#[test]
fn set_type_takes_float_and_double_only() {
    let mut h = SettingsHost::new();
    assert_eq!(h.settings.gen_type, StorageType::Float);
    h.run("set type double").expect("double");
    assert_eq!(h.settings.gen_type, StorageType::Double);
    assert_eq!(h.c_str("type"), "double");
    h.run("set type float").expect("float");
    assert_eq!(h.c_str("type"), "float");
    let e = h.run("set type long").expect_err("Stata 18 takes neither");
    assert_eq!(e.rc, 198);
    assert_eq!(e.offending_token.as_deref(), Some("long"));
}

#[test]
fn the_on_off_settings_round_trip_through_c() {
    let mut h = SettingsHost::new();
    assert_eq!(h.c_str("varabbrev"), "on");
    h.run("set varabbrev off").expect("off");
    assert!(!h.settings.varabbrev);
    assert_eq!(h.c_str("varabbrev"), "off");
    h.run("set varabbrev on").expect("on");
    assert!(h.settings.varabbrev);

    assert_eq!(h.c_str("more"), "off", "a clean session forces `more` off");
    h.run("set more on").expect("on");
    assert_eq!(h.c_str("more"), "on");

    let e = h.run("set more maybe").expect_err("on|off only");
    assert_eq!(e.rc, 198);
    assert_eq!(e.offending_token.as_deref(), Some("maybe"));
}

#[test]
fn set_seed_bumps_the_epoch_every_time_even_for_the_same_value() {
    // GOLDEN core_surface.log:
    //     set seed 12345 / gen u = runiform() / set seed 12345 /
    //     gen u2 = runiform() / assert u == u2
    // The second `set seed 12345` must restart the stream, so the session
    // cannot key its reseed on the VALUE changing.
    let mut h = SettingsHost::new();
    assert_eq!(h.settings.seed, DEFAULT_SEED);
    assert_eq!(h.settings.seed_epoch, 0);
    h.run("set seed 12345").expect("seed");
    assert_eq!(h.settings.seed, 12345);
    assert_eq!(h.settings.seed_epoch, 1);
    h.run("set seed 12345").expect("the same seed again");
    assert_eq!(
        h.settings.seed_epoch, 2,
        "the epoch moves, so the RNG restarts"
    );
    assert_eq!(h.c_str("seed"), "12345");

    let e = h.run("set seed nope").expect_err("not a number");
    assert_eq!(e.rc, 198);
}

#[test]
fn set_obs_grows_the_dataset_and_refuses_to_shrink_it() {
    let mut h = SettingsHost::new();
    h.run("set obs 10").expect("grow");
    assert_eq!(h.frames.current().n_obs(), 10);
    assert_eq!(h.text(), "Number of observations (_N) was 5, now 10.\n");
    assert_eq!(h.c_num("N"), 10.0);
    assert_eq!(h.c_num("obs"), 10.0);

    let e = h.run("set obs 3").expect_err("may not shrink");
    assert_eq!(
        (e.rc, e.message.as_str()),
        (198, "may not decrease the number of observations")
    );
    assert_eq!(h.frames.current().n_obs(), 10);

    // `set obs` to the same count is a no-op that still reports.
    h.run("set obs 10").expect("same count");
    assert_eq!(h.text(), "Number of observations (_N) was 10, now 10.\n");
}

#[test]
fn set_obs_groups_thousands_the_way_stata_does() {
    let mut h = SettingsHost::new();
    h.run("set obs 1000").expect("grow");
    assert_eq!(h.text(), "Number of observations (_N) was 5, now 1,000.\n");
}

#[test]
fn an_unknown_setting_is_rc10_not_r198() {
    // "We have not written it" is not "you are wrong": a `set` this build has
    // never heard of is exit 10, and the token is the setting name so §21 can
    // suggest one.
    let mut h = SettingsHost::new();
    let e = h.run("set nosuchsetting 1").expect_err("rc 10");
    assert_eq!(e.rc, 10);
    assert_eq!(e.message, "unsupported in this version: set nosuchsetting");
    assert_eq!(e.offending_token.as_deref(), Some("nosuchsetting"));
    assert_eq!(cmd::to_diagnostic(&e).code, "STRATUM0010");

    let e = h
        .run("set")
        .expect_err("bare `set` lists everything in Stata");
    assert_eq!(e.rc, 10);
}

#[test]
fn permanently_is_accepted_and_ignored() {
    // There is no profile.do in v1. Silently not persisting is the honest
    // failure; claiming persistence and losing it is worse.
    let mut h = SettingsHost::new();
    h.run("set level 90, permanently").expect("accepted");
    assert_eq!(h.settings.level, 90.0);
}

// ---------------------------------------------------------------------------
// c()
// ---------------------------------------------------------------------------

#[test]
fn creturn_answers_every_name_it_lists_and_nothing_else() {
    let h = SettingsHost::new();
    for name in C_NAMES {
        assert!(
            h.c(name).is_some(),
            "c({name}) is in C_NAMES but `creturn` does not answer it"
        );
    }
    // Clock- and environment-derived names are deliberately absent: reading
    // them is a Taint::EXTERNAL event that ExecCtx has to record.
    for name in ["current_date", "current_time", "username", "pwd", "os"] {
        assert!(h.c(name).is_none(), "c({name}) must not be answered here");
    }
    assert!(h.c("nosuchcvalue").is_none());
}

#[test]
fn creturn_reports_the_live_dataset_and_the_last_return_code() {
    let mut h = SettingsHost::new();
    assert_eq!(h.c_num("N"), 5.0);
    assert_eq!(h.c_num("k"), 1.0);
    assert_eq!(h.c_num("changed"), 0.0);
    assert_eq!(h.c_num("version"), 18.5);
    assert_eq!(h.c_num("pi"), core::f64::consts::PI);
    assert_eq!(h.c_num("rc"), 0.0);
    h.set_last_rc(111);
    assert_eq!(h.c_num("rc"), 111.0);
}

#[test]
fn creturn_list_renders_names_right_aligned_in_25_columns() {
    let mut h = SettingsHost::new();
    h.creturn_list();
    for line in h.text().lines() {
        let (key, _) = line.split_once(" = ").expect(line);
        assert_eq!(key.len(), 25, "{line:?}");
        assert!(key.trim_start().starts_with("c("), "{line:?}");
    }
    assert_eq!(
        h.text().lines().count(),
        C_NAMES.len(),
        "every listed name is printed"
    );
}
