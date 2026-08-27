//! [`Session::fresh`] and the sixteen-item clean-state checklist.
//!
//! ARCHITECTURE §7.7 makes the list normative and spells out the reason it is a
//! *constructor*:
//!
//! > Clean execution does not *reset* a session — it **constructs a new one**
//! > (`Session::fresh(SessionConfig)`), so there is no cleanup path that can
//! > forget an item.
//!
//! Two mechanisms hold that in place, and neither is a comment:
//!
//! 1. [`Session`] is `#[derive(PartialEq)]` over its sixteen namespaces, and
//!    `fresh` builds the struct literal in one expression. A seventeenth
//!    namespace is a field, a field is in the derive, and the derive is what
//!    `tests/fresh_checklist.rs`'s construct-don't-reset test compares. Adding
//!    state without a clean-state answer for it does not compile past
//!    `Session::fresh`, because the struct literal names every field.
//! 2. [`CleanItem`] is the list itself, as data, with [`CleanItem::ALL`] holding
//!    exactly sixteen entries and [`audit`] answering every one. The test file
//!    asserts one named assertion per item; the audit is what a running engine
//!    calls when a `--clean` child wants to prove its own starting state before
//!    it executes a line.

use camino::Utf8PathBuf;
use indexmap::IndexMap;
use stratum_parse::MacroEnv;
use stratum_proto::{SessionEpoch, SessionId};

use crate::ado::AdoState;
use crate::config::{SessionConfig, SettingId, SettingValue, SettingsSnapshot, LINESIZE};
use crate::frames::Frames;
use crate::session::{
    ControlState, EnvTaint, EstimateStore, FileHandles, GraphStore, MatrixStore, RngState,
    ScalarStore, Session,
};

/// One line of ARCHITECTURE §7.7's checklist.
///
/// The order is §7.7's order, and `docs/design/03` §8's numbering, so a
/// violation reported as `CleanItem::Rng` is item 6 in both documents.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum CleanItem {
    /// 1. Every frame dropped; a single frame `default`, 0 obs, 0 vars,
    ///    current; no `frlink`s.
    Frames,
    /// 2. The dataset empty. `_N = 0`, no `sortedby`, no
    ///    `tsset`/`xtset`/`svyset`/`mi set`, dataset label, notes and
    ///    characteristics cleared.
    Dataset,
    /// 3. All globals dropped; the local scope stack emptied to depth 0 — which
    ///    is `MacroEnv`'s depth 1, one always-open `DoFile` scope with nothing
    ///    in it, because a scopeless environment makes every `local` a match on
    ///    `Option`.
    Macros,
    /// 4. Scalars and matrices dropped, including `_b`, `_se` and `r(table)`.
    ScalarsMatrices,
    /// 5. `estimates clear`; `e()`, `r()` and `s()` empty.
    Estimates,
    /// 6. `kind = Mt64`, seeded with `default_seed` (123456789), `draws = 0`,
    ///    `seed_origin = ExecutionId(0)`, `sortseed = cfg.sortseed`.
    Rng,
    /// 7. `cd` to the directory of the entry-point `.do` file — not the app's
    ///    cwd, not the last-used directory.
    Cwd,
    /// 8. The constant `c()` defaults table, then forced: `more off`, `rmsg
    ///    off`, `linesize 80`, `pagesize 0`, `dp period`, `varabbrev on`, `type
    ///    float`, `level 95`, `sortseed <cfg>`, `trace off`.
    Settings,
    /// 9. Session-defined programs dropped; the ado path reset to
    ///    `cfg.ado_path` with `PERSONAL`/`PLUS` excluded; the compiled-command
    ///    cache discarded.
    Ado,
    /// 10. `version` set to `cfg.version`.
    Version,
    /// 11. `set trace off`, `capture` nesting depth 0, the `preserve` stack
    ///     emptied and its spill files deleted.
    Control,
    /// 12. `graph drop _all`; the scheme reset to the configured default.
    Graphs,
    /// 13. Every `file open` handle closed; every `postfile` handle closed and
    ///     its temp file removed.
    FileHandles,
    /// 14. The `tempvar`/`tempfile`/`tempname` counter reset to 0. Temp names
    ///     appear in output, so without this two clean runs of one file produce
    ///     different text.
    Tempnames,
    /// 15. `getenv`/`c(username)`/`c(hostname)`/`c(current_date)` still
    ///     readable, but every read recorded into the footprint, setting
    ///     `Taint::ENVIRONMENT` or `Taint::CLOCK`.
    Environment,
    /// 16. UTF-8 everywhere; string comparison and `sort` on string variables
    ///     byte-wise, never the OS locale's collation.
    Collation,
}

impl CleanItem {
    /// The list, in §7.7 order. Exactly sixteen entries — the length is asserted
    /// by `tests/fresh_checklist.rs`, because "sixteen" is the number
    /// ARCHITECTURE §7.7 and `docs/design/03` §8 both commit to.
    pub const ALL: [CleanItem; 16] = [
        CleanItem::Frames,
        CleanItem::Dataset,
        CleanItem::Macros,
        CleanItem::ScalarsMatrices,
        CleanItem::Estimates,
        CleanItem::Rng,
        CleanItem::Cwd,
        CleanItem::Settings,
        CleanItem::Ado,
        CleanItem::Version,
        CleanItem::Control,
        CleanItem::Graphs,
        CleanItem::FileHandles,
        CleanItem::Tempnames,
        CleanItem::Environment,
        CleanItem::Collation,
    ];

    /// The 1-based number this item carries in `docs/design/03` §8.
    #[must_use]
    pub fn number(self) -> u8 {
        // `position` over ALL rather than a second literal table: two hand-kept
        // lists of sixteen is one list too many.
        u8::try_from(
            CleanItem::ALL
                .iter()
                .position(|i| *i == self)
                .expect("CleanItem::ALL contains every variant")
                + 1,
        )
        .expect("sixteen fits in a u8")
    }

    /// The short name a diagnostic prints.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            CleanItem::Frames => "frames",
            CleanItem::Dataset => "dataset",
            CleanItem::Macros => "macros",
            CleanItem::ScalarsMatrices => "scalars and matrices",
            CleanItem::Estimates => "estimates",
            CleanItem::Rng => "rng",
            CleanItem::Cwd => "working directory",
            CleanItem::Settings => "settings",
            CleanItem::Ado => "programs and ado",
            CleanItem::Version => "version",
            CleanItem::Control => "control state",
            CleanItem::Graphs => "graphs",
            CleanItem::FileHandles => "file handles",
            CleanItem::Tempnames => "temp names",
            CleanItem::Environment => "environment",
            CleanItem::Collation => "locale and collation",
        }
    }
}

impl std::fmt::Display for CleanItem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}. {}", self.number(), self.name())
    }
}

/// Every checklist item `session` violates, in §7.7 order.
///
/// Empty means the session is clean. This is a *verification* of the state, not
/// the construction of it: `Session::fresh` is the only thing that produces a
/// clean session, and this is what proves it did.
#[must_use]
pub fn audit(session: &Session) -> Vec<CleanItem> {
    let cfg = &session.config;
    let mut bad = Vec::new();
    let mut check = |ok: bool, item: CleanItem| {
        if !ok {
            bad.push(item);
        }
    };

    check(session.frames.is_clean_frames(), CleanItem::Frames);
    check(session.frames.is_clean_dataset(), CleanItem::Dataset);
    check(
        session.macros.global_names().is_empty()
            && session.macros.depth() == 1
            && session.macros.scope().names().is_empty(),
        CleanItem::Macros,
    );
    check(
        session.scalars.is_empty() && session.matrices.is_empty(),
        CleanItem::ScalarsMatrices,
    );
    check(session.estimates.is_clean(), CleanItem::Estimates);
    check(session.rng.is_clean(cfg), CleanItem::Rng);
    check(session.cwd == cfg.cwd, CleanItem::Cwd);
    // Against the FORCED table, never against `cfg.settings`. A session opened
    // from a window that asked for `set level 90` is a perfectly good session
    // and a bad clean state, and item 8 is the question "is this clean state?".
    check(
        session.settings == SettingsSnapshot::v1(cfg.sortseed)
            && session.linesize() == LINESIZE
            && matches!(
                session.settings.get(SettingId::Linesize),
                Some(SettingValue::Num(n)) if *n == f64::from(LINESIZE)
            ),
        CleanItem::Settings,
    );
    check(session.ado.is_clean(), CleanItem::Ado);
    check(session.version == cfg.version, CleanItem::Version);
    check(session.control.is_clean(), CleanItem::Control);
    check(session.graphs.is_clean(), CleanItem::Graphs);
    check(session.files.is_clean(), CleanItem::FileHandles);
    check(session.macros.temps_issued() == 0, CleanItem::Tempnames);
    check(session.env.is_clean(), CleanItem::Environment);
    check(session.locale == cfg.locale, CleanItem::Collation);

    bad
}

impl Session {
    /// A clean environment, item by item.
    ///
    /// Allocates a fresh [`SessionId`] and starts at epoch 0. A clean run
    /// *inside* an existing session — `Isolation::InProcess` — wants the same
    /// session identity with a bumped epoch instead, and calls
    /// [`Session::fresh_at`].
    #[must_use]
    pub fn fresh(cfg: SessionConfig) -> Session {
        Session::fresh_at(cfg, Session::next_id(), SessionEpoch(0))
    }

    /// [`Session::fresh`] with the identity supplied.
    ///
    /// This is the one constructor: `fresh` is a two-line call into it, so the
    /// checklist has exactly one implementation and the tests exercise the same
    /// code the engine runs.
    ///
    /// **Every field of the struct literal below is a checklist item, in §7.7
    /// order.** Nothing is copied from another session, nothing is cleared in
    /// place, and nothing reads the process environment: `cwd` comes from the
    /// config rather than from `std::env::current_dir`, which is item 7's whole
    /// point, and the settings table is a versioned constant rather than a
    /// preferences file, which is item 8's.
    #[must_use]
    pub fn fresh_at(cfg: SessionConfig, id: SessionId, epoch: SessionEpoch) -> Session {
        let cwd: Utf8PathBuf = cfg.cwd.clone();
        // The config's table, not a second literal one. `SettingsSnapshot::v1`
        // is its ONLY constructor and `SessionConfig::new` is the only thing
        // that calls it, so a config built for a clean run carries the forced
        // constant table by construction — item 8 holds without this line
        // hard-coding it a second time. What the line buys is that the three
        // settings a window may override (`level`, `more`, `varabbrev`, through
        // `SessionConfig::from_wire`) actually reach the session instead of
        // being silently discarded here, and `audit` still measures the result
        // against the forced table, so such a session is reported as *not* in
        // clean state rather than quietly accepted as one.
        let settings = cfg.settings.clone();
        let version = cfg.version;
        let locale = cfg.locale;
        let rng = RngState::fresh(&cfg);
        // Item 9's exclusion happens HERE, not in the caller. `AdoPath::clean`
        // filters by `AdoDir`, so a caller that hands us the user's `PERSONAL`
        // directory gets it dropped rather than trusted; `with_personal` is the
        // documented override, and it sets the flag the reproducibility report
        // prints. A `Vec<Utf8PathBuf>` config could not express either.
        let ado = AdoState::fresh(if cfg.ado_personal {
            crate::ado::AdoPath::with_personal(cfg.ado_path.iter().cloned())
        } else {
            crate::ado::AdoPath::clean(cfg.ado_path.iter().cloned())
        });

        Session {
            config: cfg,
            id,
            epoch,
            // 1, 2
            frames: Frames::fresh(),
            // 3, and the counter behind 14
            macros: MacroEnv::new(),
            // 4
            scalars: ScalarStore::default(),
            matrices: MatrixStore::default(),
            // 5
            estimates: EstimateStore {
                stored: IndexMap::new(),
                e: IndexMap::new(),
                r: IndexMap::new(),
                s: IndexMap::new(),
            },
            // 6
            rng,
            // 7
            cwd,
            // 8
            settings,
            // 9
            ado,
            // 10
            version,
            // 11
            control: ControlState::default(),
            // 12
            graphs: GraphStore::default(),
            // 13
            files: FileHandles::default(),
            // 15
            env: EnvTaint::default(),
            // 16
            locale,
            // Not checklist items. A fresh session has no documents open and
            // has issued no block ids; both are fields, so both are in the
            // derived equality the construct-don't-reset test compares on.
            docs: crate::document::Documents::new(),
            // Starts at 1 because 0 is `BlockId::EPHEMERAL`.
            next_block: 1,
        }
    }

    /// Every checklist item this session violates. Empty for a fresh one.
    #[must_use]
    pub fn audit_clean(&self) -> Vec<CleanItem> {
        audit(self)
    }

    /// True when [`Session::audit_clean`] is empty.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.audit_clean().is_empty()
    }
}
