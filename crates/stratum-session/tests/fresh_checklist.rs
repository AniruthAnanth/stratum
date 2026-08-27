//! ARCHITECTURE §7.7's sixteen-item clean-state checklist, one named assertion
//! per item.
//!
//! **Not one aggregate test.** The plan says so explicitly, and the reason is
//! that an aggregate `assert!(session.is_clean())` reports "the session is not
//! clean" when what a reader needs is "item 13, file handles". Sixteen tests
//! named after their items make the failing line the answer.
//!
//! Each test follows the same shape, and the shape is the point:
//!
//! 1. Build a session and **dirty the namespace that item owns**.
//! 2. Assert the audit now names that item — so the audit is not a stub that
//!    always says "clean".
//! 3. Build a fresh session from the same config and assert the item is clean.
//!
//! Step 2 is what makes step 3 mean anything. A checklist whose audit could not
//! detect a violation would pass all sixteen tests on an empty implementation.

use std::cmp::Ordering;

use camino::Utf8PathBuf;
use indexmap::IndexMap;
use pretty_assertions::assert_eq;
use stratum_core::Value;
use stratum_data::StorageType;
use stratum_proto::{ExecutionId, SessionEpoch, SessionId, Taint, VarIdx};
use stratum_session::ado::{AdoDir, AdoEntry, AdoPath, ProgramClass, ProgramDef};
use stratum_session::config::{
    SessionConfig, SettingId, SettingValue, SettingsSnapshot, DEFAULT_SEED, LINESIZE,
};
use stratum_session::frames::{MiStyle, TimeSeriesSet};
use stratum_session::session::{
    EnvSource, GraphHandle, Matrix, OpenFile, PreserveEntry, RngState, Session, StoredEstimate,
    DEFAULT_SCHEME,
};
use stratum_session::{CleanItem, StataVersion};

/// The session identity every test in this file uses, so that two sessions
/// differ in nothing incidental. `Session::fresh` allocates a fresh id; the
/// last test in this file proves the two constructors agree.
const ID: SessionId = SessionId(4242);
const EPOCH: SessionEpoch = SessionEpoch(7);

fn cfg() -> SessionConfig {
    // An absolute path that is not the process cwd, which is what item 7 is
    // about. Deliberately not `std::env::current_dir()`.
    SessionConfig::new("/projects/wage-study/analysis").expect("a plain cwd is accepted")
}

fn fresh() -> Session {
    Session::fresh_at(cfg(), ID, EPOCH)
}

/// Asserts that `item` is currently violated — the audit can see it — and that
/// a fresh session does not violate it.
#[track_caller]
fn violated_then_clean(dirty: &Session, item: CleanItem) {
    assert!(
        dirty.audit_clean().contains(&item),
        "the audit did not notice {item} was dirty; it reported {:?}",
        dirty.audit_clean()
    );
    assert!(
        !fresh().audit_clean().contains(&item),
        "a fresh session violates {item}"
    );
}

// ── the list itself ─────────────────────────────────────────────────────────

#[test]
fn item_00_the_checklist_has_exactly_sixteen_items() {
    // ARCHITECTURE §7.7 and docs/design/03 §8 both commit to sixteen. If this
    // number moves, both documents move with it.
    assert_eq!(CleanItem::ALL.len(), 16);
    let mut seen: Vec<u8> = CleanItem::ALL.iter().map(|i| i.number()).collect();
    seen.sort_unstable();
    assert_eq!(seen, (1u8..=16).collect::<Vec<_>>());
}

#[test]
fn item_00_a_fresh_session_violates_nothing() {
    assert_eq!(fresh().audit_clean(), Vec::<CleanItem>::new());
    assert!(fresh().is_clean());
}

// ── 1. frames ───────────────────────────────────────────────────────────────

#[test]
fn item_01_frames_one_empty_default_frame_current_and_no_frlinks() {
    let f = fresh();
    assert_eq!(f.frames().set().len(), 1);
    assert_eq!(&**f.frames().set().current_name(), "default");
    assert_eq!(f.frames().set().current().n_obs(), 0);
    assert_eq!(f.frames().set().current().n_vars(), 0);
    assert!(f.frames().links().is_empty());

    let mut dirty = fresh();
    dirty.frames_mut().create("aux").expect("a new frame name");
    dirty.frames_mut().change("aux").expect("aux exists");
    violated_then_clean(&dirty, CleanItem::Frames);
}

// ── 2. dataset ──────────────────────────────────────────────────────────────

#[test]
fn item_02_dataset_empty_unsorted_undeclared_and_unlabelled() {
    let mut dirty = fresh();
    {
        let frame = dirty.frames_mut().set_mut().current_mut();
        frame.set_n_obs(4);
        frame
            .add_var("price", StorageType::Double)
            .expect("a legal name");
        frame.set_label("1978 Automobile Data");
        frame.chars_mut().set("_dta", "note1", "from the manual");
        frame
            .sort_by(&[(VarIdx(0), stratum_proto::SortDir::Asc)])
            .expect("sorting one numeric column");
    }
    dirty.frames_mut().bindings_mut().tsset = Some(TimeSeriesSet {
        panelvar: None,
        timevar: Some("year".to_owned()),
        delta: 1.0,
        format: Some("%ty".to_owned()),
    });
    dirty.frames_mut().bindings_mut().mi = Some(MiStyle::Wide);
    violated_then_clean(&dirty, CleanItem::Dataset);

    let f = fresh();
    let frame = f.frames().set().current();
    assert_eq!(frame.n_obs(), 0, "_N = 0");
    assert!(frame.sort_state().keys.is_empty(), "no sortedby");
    assert!(frame.label().is_empty(), "no dataset label");
    assert!(frame.chars().is_empty(), "no notes or characteristics");
    assert!(f.frames().bindings().is_clean(), "no tsset/xtset/svyset/mi");
}

// ── 3. macros ───────────────────────────────────────────────────────────────

#[test]
fn item_03_macros_globals_dropped_and_the_local_stack_at_depth_one() {
    let mut dirty = fresh();
    dirty.macros_mut().set_global("S_ADO", "/home/me/ado");
    dirty
        .macros_mut()
        .push_scope(stratum_parse::macros::ScopeKind::DoFile);
    dirty.macros_mut().set_local("i", "17");
    violated_then_clean(&dirty, CleanItem::Macros);

    let f = fresh();
    assert!(f.macros().global_names().is_empty());
    // Depth 1, not 0: `MacroEnv` always holds one open `DoFile` scope, because
    // `local x 1` typed at the console has to go somewhere. §7.7's "depth 0"
    // means "no *nested* scopes", and this is that state.
    assert_eq!(f.macros().depth(), 1);
    assert!(f.macros().scope().names().is_empty());
}

// ── 4. scalars and matrices ─────────────────────────────────────────────────

#[test]
fn item_04_scalars_and_matrices_dropped_including_b_se_and_r_table() {
    let mut dirty = fresh();
    dirty.scalars_mut().set("rho", Value::Real(0.5));
    for name in ["_b", "_se", "r(table)"] {
        dirty.matrices_mut().set(
            name,
            Matrix {
                rows: 1,
                cols: 1,
                data: vec![1.0],
                rownames: vec!["y1".to_owned()],
                colnames: vec!["educ".to_owned()],
            },
        );
    }
    violated_then_clean(&dirty, CleanItem::ScalarsMatrices);

    let f = fresh();
    assert!(f.scalars().is_empty());
    assert!(f.matrices().is_empty());
    for name in ["_b", "_se", "r(table)"] {
        assert!(
            f.matrices().get(name).is_none(),
            "{name} survived into a fresh session"
        );
    }
}

// ── 5. estimates ────────────────────────────────────────────────────────────

#[test]
fn item_05_estimates_cleared_and_r_e_s_empty() {
    let mut dirty = fresh();
    dirty.estimates_mut().stored.insert(
        "m1".to_owned(),
        StoredEstimate {
            cmd: "regress".to_owned(),
            e: IndexMap::new(),
            from: ExecutionId(3),
        },
    );
    dirty
        .estimates_mut()
        .e
        .insert("N".to_owned(), Value::Real(74.0));
    dirty
        .estimates_mut()
        .r
        .insert("mean".to_owned(), Value::Real(6165.257));
    dirty
        .estimates_mut()
        .s
        .insert("cmd".to_owned(), Value::Str("summarize".to_owned()));
    violated_then_clean(&dirty, CleanItem::Estimates);

    let f = fresh();
    assert!(f.estimates().stored.is_empty(), "estimates clear");
    assert!(f.estimates().e.is_empty(), "e() empty");
    assert!(f.estimates().r.is_empty(), "r() empty");
    assert!(f.estimates().s.is_empty(), "s() empty");
}

// ── 6. RNG ──────────────────────────────────────────────────────────────────

#[test]
fn item_06_rng_mt64_seeded_from_the_default_with_zero_draws() {
    let f = fresh();
    assert_eq!(f.rng().kind, stratum_session::RngKind::Mt64);
    assert_eq!(f.rng().seed_value, DEFAULT_SEED);
    assert_eq!(DEFAULT_SEED, 123_456_789, "Stata's documented default");
    assert_eq!(f.rng().draws, 0);
    assert_eq!(f.rng().seed_origin, ExecutionId(0));
    assert_eq!(f.rng().sortseed, cfg().sortseed);
    assert!(
        f.rng().seed_is_default(),
        "lint R002 keys on this: the file never set a seed"
    );

    let mut dirty = fresh();
    *dirty.rng_mut() = RngState {
        kind: stratum_session::RngKind::Kiss32,
        seed_origin: ExecutionId(9),
        seed_value: 20_260_822,
        draws: 1_000_000,
        sortseed: 42,
    };
    violated_then_clean(&dirty, CleanItem::Rng);
}

// ── 7. working directory ────────────────────────────────────────────────────

#[test]
fn item_07_cwd_is_the_entry_files_directory_not_the_processs() {
    let entry = Utf8PathBuf::from("/projects/wage-study/analysis/wages.do");
    let cfg = SessionConfig::for_entry(&entry).expect("the entry has a directory");
    let f = Session::fresh_at(cfg, ID, EPOCH);
    assert_eq!(f.cwd(), "/projects/wage-study/analysis");

    // The property is that it did NOT come from the process. The process cwd is
    // this crate's directory under a cargo run, and it is never the fixture path
    // above — asserting they differ is what makes the equality above evidence.
    let process = Utf8PathBuf::from_path_buf(
        std::env::current_dir().expect("a process always has a working directory"),
    )
    .expect("the checkout path is UTF-8");
    assert_ne!(f.cwd(), &process);

    let mut dirty = fresh();
    dirty.set_cwd("/tmp/somewhere-else");
    violated_then_clean(&dirty, CleanItem::Cwd);
}

// ── 8. settings ─────────────────────────────────────────────────────────────

#[test]
fn item_08_settings_are_the_constant_table_then_forced_including_linesize_80() {
    let f = fresh();
    let expect = [
        (SettingId::More, SettingValue::OnOff(false)),
        (SettingId::Rmsg, SettingValue::OnOff(false)),
        (SettingId::Linesize, SettingValue::Num(80.0)),
        (SettingId::Pagesize, SettingValue::Num(0.0)),
        (SettingId::Dp, SettingValue::Word("period")),
        (SettingId::Varabbrev, SettingValue::OnOff(true)),
        (SettingId::Type, SettingValue::Word("float")),
        (SettingId::Level, SettingValue::Num(95.0)),
        (SettingId::Sortseed, SettingValue::Num(0.0)),
        (SettingId::Trace, SettingValue::OnOff(false)),
    ];
    for (id, want) in &expect {
        assert_eq!(
            f.setting(*id),
            Some(want),
            "set {} is not at its forced clean-state value",
            id.name()
        );
    }
    assert_eq!(SettingId::FORCED.len(), expect.len());

    // C44/A16: `c(linesize)` reports 80 in every code path, and the setter is
    // the code path that could break that.
    assert_eq!(f.linesize(), LINESIZE);
    let mut f = fresh();
    assert!(
        !f.set_setting(SettingId::Linesize, SettingValue::Num(132.0)),
        "`set linesize 132` must be refused, not accepted and ignored"
    );
    assert_eq!(f.linesize(), 80);
    assert_eq!(
        f.setting(SettingId::Linesize),
        Some(&SettingValue::Num(80.0))
    );

    let mut dirty = fresh();
    assert!(dirty.set_setting(SettingId::Level, SettingValue::Num(90.0)));
    assert!(dirty.set_setting(SettingId::More, SettingValue::OnOff(true)));
    violated_then_clean(&dirty, CleanItem::Settings);
}

// ── 9. programs and ado ─────────────────────────────────────────────────────

#[test]
fn item_09_ado_path_excludes_personal_and_plus_and_programs_are_dropped() {
    let entries = vec![
        AdoEntry {
            kind: AdoDir::Base,
            dir: "/opt/stratum/ado/base".into(),
        },
        AdoEntry {
            kind: AdoDir::Project,
            dir: "/projects/wage-study/ado".into(),
        },
        AdoEntry {
            kind: AdoDir::Personal,
            dir: "/home/me/Library/Stata/ado/personal".into(),
        },
        AdoEntry {
            kind: AdoDir::Plus,
            dir: "/home/me/Library/Stata/ado/plus".into(),
        },
        AdoEntry {
            kind: AdoDir::Oldplace,
            dir: "/home/me/ado".into(),
        },
    ];
    let clean = AdoPath::clean(entries.clone());
    let dirs: Vec<&str> = clean.dirs().map(camino::Utf8Path::as_str).collect();
    assert_eq!(dirs, ["/opt/stratum/ado/base", "/projects/wage-study/ado"]);
    assert!(!clean.personal_included());
    assert!(clean.is_clean());

    // The documented override puts them back AND says so — that flag is what
    // the reproducibility report prints.
    let overridden = AdoPath::with_personal(entries.clone());
    assert_eq!(overridden.dirs().count(), 5);
    assert!(overridden.personal_included());
    assert!(!overridden.is_clean());

    let f = fresh();
    assert!(f.ado().is_clean());
    assert!(f.ado().programs.is_empty(), "session programs dropped");
    assert!(
        f.ado().resolved.is_empty(),
        "the compiled-command cache is discarded: a PERSONAL ado resolved by an \
         earlier interactive run must not answer a clean run's lookup"
    );

    // The exclusion happens inside `Session::fresh`, not in whoever built the
    // config. Hand the constructor the user's PERSONAL and PLUS directories and
    // they do not reach the session — which is the property that keeps item 9 in
    // the crate ARCHITECTURE §7.7 puts it in.
    let mut with_user_dirs = cfg();
    with_user_dirs.ado_path = entries.clone();
    let filtered = Session::fresh_at(with_user_dirs.clone(), ID, EPOCH);
    assert_eq!(
        filtered
            .ado()
            .path
            .dirs()
            .map(camino::Utf8Path::as_str)
            .collect::<Vec<_>>(),
        ["/opt/stratum/ado/base", "/projects/wage-study/ado"],
        "Session::fresh trusted the caller to have filtered PERSONAL/PLUS"
    );
    assert!(filtered.audit_clean().is_empty());

    // The documented override is *recorded*: the session is constructible, and
    // the audit says out loud that it is not a clean one.
    let mut overridden_cfg = with_user_dirs;
    overridden_cfg.ado_personal = true;
    let with_personal = Session::fresh_at(overridden_cfg, ID, EPOCH);
    assert_eq!(with_personal.ado().path.dirs().count(), 5);
    assert!(with_personal.ado().path.personal_included());
    assert!(
        with_personal.audit_clean().contains(&CleanItem::Ado),
        "a session that put PERSONAL back is not in clean state, and the audit          is where that is said"
    );

    let mut dirty = fresh();
    dirty.ado_mut().programs.insert(
        "mypwd".to_owned(),
        ProgramDef {
            name: "mypwd".to_owned(),
            body: "display \"`c(pwd)'\"".to_owned(),
            class: ProgramClass::Nclass,
            sortpreserve: false,
            version: StataVersion::DEFAULT,
        },
    );
    dirty
        .ado_mut()
        .resolved
        .insert("mypwd".to_owned(), "/home/me/ado/mypwd.ado".into());
    violated_then_clean(&dirty, CleanItem::Ado);
}

// ── 10. version ─────────────────────────────────────────────────────────────

#[test]
fn item_10_version_is_the_configured_language_level() {
    let f = fresh();
    assert_eq!(f.version(), cfg().version);
    assert_eq!(f.version(), StataVersion::DEFAULT);

    let mut dirty = fresh();
    dirty.set_version(StataVersion(13));
    violated_then_clean(&dirty, CleanItem::Version);
}

// ── 11. control state ───────────────────────────────────────────────────────

#[test]
fn item_11_control_trace_off_capture_depth_zero_preserve_stack_empty() {
    let f = fresh();
    assert!(!f.control().trace);
    assert_eq!(f.control().capture_depth, 0);
    assert!(f.control().preserve.is_empty());

    let mut dirty = fresh();
    dirty.control_mut().trace = true;
    dirty.control_mut().capture_depth = 2;
    dirty.control_mut().preserve.push(PreserveEntry {
        spill: Some("/tmp/st_preserve_0001".into()),
        frame: "default".to_owned(),
    });
    violated_then_clean(&dirty, CleanItem::Control);
}

// ── 12. graphs ──────────────────────────────────────────────────────────────

#[test]
fn item_12_graphs_dropped_and_the_scheme_reset() {
    let f = fresh();
    assert!(f.graphs().graphs.is_empty());
    assert_eq!(f.graphs().scheme, DEFAULT_SCHEME);

    let mut dirty = fresh();
    dirty.graphs_mut().graphs.insert(
        "Graph".to_owned(),
        GraphHandle {
            svg: "<svg/>".to_owned(),
            from: ExecutionId(11),
        },
    );
    dirty.graphs_mut().scheme = "s2color".to_owned();
    violated_then_clean(&dirty, CleanItem::Graphs);
}

// ── 13. file handles ────────────────────────────────────────────────────────

#[test]
fn item_13_file_and_postfile_handles_all_closed() {
    let f = fresh();
    assert!(f.files().files.is_empty());
    assert!(f.files().postfiles.is_empty());

    let mut dirty = fresh();
    dirty.files_mut().files.insert(
        "fh".to_owned(),
        OpenFile {
            path: "/projects/out.txt".into(),
            read: false,
            temporary: false,
        },
    );
    dirty.files_mut().postfiles.insert(
        "sim".to_owned(),
        OpenFile {
            path: "/tmp/__000003.dta".into(),
            read: false,
            temporary: true,
        },
    );
    violated_then_clean(&dirty, CleanItem::FileHandles);
}

// ── 14. temp names ──────────────────────────────────────────────────────────

#[test]
fn item_14_the_tempname_counter_is_zero_so_two_clean_runs_print_the_same_names() {
    let f = fresh();
    assert_eq!(f.tempnames_issued(), 0);

    // The reason the counter matters: temp names reach output. Two clean
    // sessions must hand out the same first name, or two clean runs of one file
    // differ in text while computing identical numbers.
    let mut a = fresh();
    let mut b = fresh();
    for expect in ["__000000", "__000001", "__000002"] {
        let (from_a, from_b) = (a.alloc_tempname(), b.alloc_tempname());
        assert_eq!(from_a, expect, "Stata's six-digit zero-padded sequence");
        assert_eq!(from_a, from_b, "two clean sessions hand out the same names");
    }
    assert_eq!(a.tempnames_issued(), 3);

    violated_then_clean(&a, CleanItem::Tempnames);
}

// ── 15. environment ─────────────────────────────────────────────────────────

#[test]
fn item_15_environment_reads_stay_readable_but_are_recorded_and_taint() {
    let f = fresh();
    assert!(f.env().reads.is_empty());
    assert_eq!(f.env().taint, Taint::empty());

    let mut dirty = fresh();
    dirty.env_mut().record(EnvSource::Env, "HOME");
    dirty.env_mut().record(EnvSource::Machine, "username");
    assert_eq!(dirty.env().taint, Taint::ENVIRONMENT);
    dirty.env_mut().record(EnvSource::Clock, "current_date");
    assert_eq!(
        dirty.env().taint,
        Taint::ENVIRONMENT | Taint::CLOCK,
        "a clock read taints differently from a hostname read: it changes on the \
         SAME machine"
    );
    assert_eq!(dirty.env().reads.len(), 3, "the count is the evidence");
    violated_then_clean(&dirty, CleanItem::Environment);
}

// ── 16. locale and collation ────────────────────────────────────────────────

#[test]
fn item_16_string_comparison_is_byte_wise_utf8_not_os_collation() {
    let f = fresh();
    assert_eq!(f.locale(), stratum_session::LocaleMode::Utf8Cnumeric);

    // The case that separates the two policies. Byte-wise UTF-8 puts every
    // upper-case ASCII letter before every lower-case one (0x42 < 0x61); an
    // `en_US` locale collation puts "apple" before "Banana". docs/design/03 §8
    // item 16 accepts the divergence from a non-C-locale Stata in exchange for
    // being identical on macOS, Windows and Linux.
    assert_eq!(f.collate("Banana", "apple"), Ordering::Less);
    assert_eq!(f.collate("apple", "Banana"), Ordering::Greater);
    // Accents sort by code point, not by base letter.
    assert_eq!(f.collate("z", "\u{e9}"), Ordering::Less);
    // And "" is string missing, which sorts low.
    assert_eq!(f.collate("", "a"), Ordering::Less);
    assert_eq!(f.collate("same", "same"), Ordering::Equal);

    // Item 16 has one representable state, so it cannot be dirtied: the enum has
    // one variant on purpose (making the divergence configurable would make the
    // difftest corpus depend on which machine ran it). The audit still answers
    // for it, and this is the assertion that it does.
    assert!(!fresh().audit_clean().contains(&CleanItem::Collation));
}

// ── the construct-don't-reset property ──────────────────────────────────────

/// Dirty **every one of the sixteen namespaces** in one session.
///
/// Kept as one function so the aggregate test below cannot quietly skip a
/// namespace: it dirties all of them, and the equality it then asserts is over
/// the whole `Session`, field by derived field.
fn dirty_everything(s: &mut Session) {
    // 1 frames
    s.frames_mut().create("aux").expect("a new frame");
    s.frames_mut().change("aux").expect("aux exists");
    // 2 dataset
    {
        let frame = s.frames_mut().set_mut().current_mut();
        frame.set_n_obs(74);
        frame
            .add_var("mpg", StorageType::Int)
            .expect("a legal name");
        frame.set_label("1978 Automobile Data");
        frame.chars_mut().set("_dta", "note1", "a note");
    }
    s.frames_mut().bindings_mut().svyset = Some(stratum_session::SurveySet {
        psu: Some("_n".to_owned()),
        weight: Some("pw".to_owned()),
        strata: None,
    });
    // 3 macros
    s.macros_mut().set_global("root", "/data");
    s.macros_mut().set_local("i", "3");
    // 4 scalars and matrices
    s.scalars_mut().set("rho", Value::Real(0.5));
    s.matrices_mut().set(
        "_b",
        Matrix {
            rows: 1,
            cols: 1,
            data: vec![2.0],
            rownames: vec!["y1".to_owned()],
            colnames: vec!["x".to_owned()],
        },
    );
    // 5 estimates
    s.estimates_mut()
        .e
        .insert("N".to_owned(), Value::Real(74.0));
    s.estimates_mut().stored.insert(
        "m1".to_owned(),
        StoredEstimate {
            cmd: "regress".to_owned(),
            e: IndexMap::new(),
            from: ExecutionId(1),
        },
    );
    // 6 rng
    s.rng_mut().seed_value = 1;
    s.rng_mut().draws = 4096;
    s.rng_mut().seed_origin = ExecutionId(2);
    // 7 cwd
    s.set_cwd("/tmp/elsewhere");
    // 8 settings
    s.set_setting(SettingId::Level, SettingValue::Num(90.0));
    // 9 programs and ado
    s.ado_mut().programs.insert(
        "p".to_owned(),
        ProgramDef {
            name: "p".to_owned(),
            body: "di 1".to_owned(),
            class: ProgramClass::Rclass,
            sortpreserve: true,
            version: StataVersion::DEFAULT,
        },
    );
    s.ado_mut()
        .resolved
        .insert("p".to_owned(), "/x/p.ado".into());
    s.ado_mut().path = AdoPath::with_personal([AdoEntry {
        kind: AdoDir::Personal,
        dir: "/home/me/ado".into(),
    }]);
    // 10 version
    s.set_version(StataVersion(14));
    // 11 control
    s.control_mut().trace = true;
    s.control_mut().capture_depth = 1;
    s.control_mut().preserve.push(PreserveEntry {
        spill: None,
        frame: "aux".to_owned(),
    });
    // 12 graphs
    s.graphs_mut().scheme = "s2mono".to_owned();
    s.graphs_mut().graphs.insert(
        "g".to_owned(),
        GraphHandle {
            svg: String::new(),
            from: ExecutionId(1),
        },
    );
    // 13 file handles
    s.files_mut().files.insert(
        "fh".to_owned(),
        OpenFile {
            path: "/x".into(),
            read: true,
            temporary: false,
        },
    );
    s.files_mut().postfiles.insert(
        "pf".to_owned(),
        OpenFile {
            path: "/y".into(),
            read: false,
            temporary: true,
        },
    );
    // 14 temp names
    s.alloc_tempname();
    s.alloc_tempname();
    // 15 environment
    s.env_mut().record(EnvSource::Env, "HOME");
    s.env_mut().record(EnvSource::Clock, "current_time");
    // 16 collation — one representable state; see item_16 above.
    // Documents, which are not a checklist item but ARE a field, so the derived
    // equality below covers them too.
    s.apply_document_change(
        stratum_proto::DocumentId(1),
        "sysuse auto\nsummarize price\n".to_owned(),
        1,
    );
}

#[test]
fn fresh_constructs_it_does_not_reset() {
    let virgin = Session::fresh_at(cfg(), ID, EPOCH);

    let mut dirty = Session::fresh_at(cfg(), ID, EPOCH);
    dirty_everything(&mut dirty);
    assert_eq!(
        dirty.audit_clean().len(),
        // Item 16 has one representable state and item 2 is already implied by
        // item 1's extra frame; every other item is dirty.
        CleanItem::ALL.len() - 1,
        "dirty_everything left {:?} clean",
        CleanItem::ALL
            .iter()
            .filter(|i| !dirty.audit_clean().contains(i))
            .collect::<Vec<_>>()
    );

    // THE PROPERTY. Not "reset the dirty one and compare" — construct a new one
    // while the dirty one is still alive and holding all sixteen namespaces, and
    // assert it is indistinguishable from a session that never saw any of it.
    // `Session` derives `PartialEq`, so this compares every field, including the
    // ones a future author adds.
    let after = Session::fresh_at(cfg(), ID, EPOCH);
    assert_eq!(after, virgin);
    assert!(after.is_clean());
    assert!(!dirty.is_clean(), "the dirty session is still dirty");
}

#[test]
fn fresh_and_fresh_at_are_the_same_constructor() {
    // `Session::fresh` allocates identity and delegates. If it ever grew a
    // second body, the sixteen tests above — which all use `fresh_at` so that
    // two sessions differ in nothing incidental — would stop covering the
    // constructor the engine actually calls.
    let f = Session::fresh(cfg());
    let same = Session::fresh_at(cfg(), f.id(), f.epoch());
    assert_eq!(f, same);
    assert_eq!(f.epoch(), SessionEpoch(0));

    // And identity is not recycled: two sessions are two sessions.
    let g = Session::fresh(cfg());
    assert_ne!(f.id(), g.id());
}

#[test]
fn a_fresh_session_holds_no_documents_and_has_issued_no_block_ids() {
    let f = fresh();
    assert!(f.documents().is_empty());
    assert_eq!(f.blocks_issued(), 0);
}

#[test]
fn a_window_supplied_setting_reaches_the_session_and_is_reported_as_not_clean() {
    // `SessionConfig::from_wire` is the only way the three settings a window can
    // change reach a session. Discarding them in `fresh_at` would make
    // `from_wire` a no-op that looks like it works; honouring them without
    // saying so would make `audit` a rubber stamp. Both halves are asserted.
    let wire = stratum_proto::SessionConfigWire {
        cwd: Some("/projects/wage-study/analysis".into()),
        seed: None,
        linesize: LINESIZE,
        level: 90.0,
        varabbrev: true,
        more: true,
        max_memory_bytes: None,
        ado_personal: false,
        write_sandbox: None,
    };
    let cfg = SessionConfig::from_wire(&wire, camino::Utf8Path::new("/projects/wage-study"))
        .expect("linesize 80 is accepted");
    let s = Session::fresh_at(cfg, ID, EPOCH);

    assert_eq!(s.setting(SettingId::Level), Some(&SettingValue::Num(90.0)));
    assert_eq!(s.setting(SettingId::More), Some(&SettingValue::OnOff(true)));
    assert_eq!(
        s.audit_clean(),
        vec![CleanItem::Settings],
        "the session is fine; it is simply not in clean state, and item 8 is          the question `is this clean state?`"
    );
    // C44/A16 survives the wire regardless.
    assert_eq!(s.linesize(), LINESIZE);
    assert!(
        SessionConfig::from_wire(
            &stratum_proto::SessionConfigWire {
                linesize: 132,
                ..wire
            },
            camino::Utf8Path::new("/projects/wage-study"),
        )
        .is_err(),
        "`set linesize 132` must be refused at the wire boundary"
    );
}

#[test]
fn the_settings_table_is_a_versioned_constant_not_a_preferences_file() {
    // Item 8's real content: `SettingsSnapshot::v1` is the ONLY constructor, so
    // there is no way to build a session whose settings came from an
    // environment variable or from the last interactive session.
    assert_eq!(SettingsSnapshot::v1(0), SettingsSnapshot::v1(0));
    assert_ne!(SettingsSnapshot::v1(0), SettingsSnapshot::v1(999));
    assert_eq!(
        SettingsSnapshot::v1(999).get(SettingId::Sortseed),
        Some(&SettingValue::Num(999.0)),
        "set sortseed <cfg> — so `sort` ties are reproducible rather than merely \
         usually-the-same"
    );
}
