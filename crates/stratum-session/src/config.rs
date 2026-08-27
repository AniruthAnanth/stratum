//! [`SessionConfig`] — design `03` §8's declaration, transcribed, plus the
//! vocabulary types it names.
//!
//! Everything here is a *constant* of the build or a value the caller supplies
//! once. Nothing in this module reads the environment, the clock or the
//! filesystem: a `SessionConfig` that differed between two machines would make
//! the clean-run byte comparison (spec §31, §38-E) compare two different
//! experiments.
//!
//! # The five vocabulary types, and why they are declared here
//!
//! `RngKind`, `LocaleMode`, `StataVersion`, `SettingId` and [`SettingsSnapshot`]
//! are named by design `03` §8's `SessionConfig` and are declared in no landed
//! crate. `03` §4.6 puts `RngFingerprint` — which *contains* a `RngKind` — in
//! `StateFingerprint`, which ARCHITECTURE §5 assigns to `stratum-runtime`, and
//! runtime sits *below* this crate, so it cannot reach up here for the name.
//! They are declared here provisionally because `SessionConfig` cannot compile
//! without them, and W08b's return escalates the placement: the architect's call
//! is whether they belong in `stratum-core` (which both crates already reach) or
//! whether runtime re-declares `RngFingerprint` over its own copy.

use std::fmt;

use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};
use stratum_proto::SessionConfigWire;

use crate::ado::AdoEntry;

/// Stata's documented default seed, and ARCHITECTURE §7.7's item 6.
///
/// Not "some seed": `set seed 123456789` is what Stata itself starts from, so a
/// clean run of a file that never calls `set seed` draws the same numbers we
/// would draw. Lint R002 keys on `RngState::seed_is_default`.
pub const DEFAULT_SEED: u64 = 123_456_789;

/// The ONE accepted `linesize` in v1 (C44/A16).
///
/// Forcing it is not cosmetic: it is what makes classic text output
/// byte-identical across machines, which is what lets CI diff clean-run outputs
/// at all. W06 owns rejecting `set linesize n != 80` with `rc = 10`; this
/// constant is what it rejects against.
pub const LINESIZE: u16 = 80;

/// The default confidence level, `set level 95`.
pub const DEFAULT_LEVEL: f64 = 95.0;

/// Which pseudo-random generator a session draws from.
///
/// `Mt64` is Stata 14+'s default and the only one v1 constructs; the other two
/// exist because `set rng` names them and a session that cannot represent the
/// setting cannot record that a file changed it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RngKind {
    /// 64-bit Mersenne Twister. Stata's default since 14.
    #[default]
    Mt64,
    /// `mt64s` — the stream variant.
    Mt64s,
    /// The pre-14 generator, reachable through `set rng kiss32`.
    Kiss32,
}

/// Collation and decimal-point policy.
///
/// One variant, deliberately. ARCHITECTURE §7.7 item 16 accepts a documented
/// divergence from Stata under a non-C locale in exchange for being identical on
/// all three platforms; an enum with a second arm would be an invitation to make
/// that divergence configurable, and then the difftest corpus would depend on
/// which machine ran it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocaleMode {
    /// UTF-8 everywhere, byte-wise string ordering, `.` as the decimal point.
    #[default]
    Utf8Cnumeric,
}

/// The language-compatibility level a session starts at.
///
/// A `version 16` statement inside the file lowers it when reached, and the
/// reproducibility report records that it did (checklist item 10).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct StataVersion(pub u16);

impl StataVersion {
    /// The version a clean session starts at when the caller says nothing.
    pub const DEFAULT: StataVersion = StataVersion(18);
}

impl Default for StataVersion {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl fmt::Display for StataVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A `set`-able setting, as a closed enum rather than a string.
///
/// `03` §4.6's `settings: imbl::HashMap<SettingId, u64>` is a version *per
/// setting*, so a block that read `c(level)` is not made stale by a `set
/// varabbrev`. That only works if `SettingId` is an id; a `String` key would put
/// a heap allocation on the read barrier, which is on the execution path.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettingId {
    /// `set more` — forced off in a clean session.
    More,
    /// `set rmsg` — forced off.
    Rmsg,
    /// `set linesize` — forced to [`LINESIZE`].
    Linesize,
    /// `set pagesize` — forced to 0, i.e. never page.
    Pagesize,
    /// `set dp` — forced to `period`.
    Dp,
    /// `set varabbrev` — forced on, which is Stata's default.
    Varabbrev,
    /// `set type` — forced to `float`, which is Stata's default `generate` type.
    Type,
    /// `set level` — forced to [`DEFAULT_LEVEL`].
    Level,
    /// `set sortseed` — forced to `SessionConfig::sortseed`.
    Sortseed,
    /// `set seed`. Not forced by the settings table: the RNG namespace owns it,
    /// and it appears here so a block that read `c(seed)` has something to key
    /// a version on.
    Seed,
    /// `set trace` — forced off. Item 11 rather than item 8, but one id space.
    Trace,
    /// `set obs`. Never forced; a clean dataset has no observations at all.
    Obs,
}

impl SettingId {
    /// Every setting a clean session forces, in the order ARCHITECTURE §7.7
    /// item 8 lists them.
    pub const FORCED: [SettingId; 10] = [
        SettingId::More,
        SettingId::Rmsg,
        SettingId::Linesize,
        SettingId::Pagesize,
        SettingId::Dp,
        SettingId::Varabbrev,
        SettingId::Type,
        SettingId::Level,
        SettingId::Sortseed,
        SettingId::Trace,
    ];

    /// The `c()` name, for diagnostics and for `creturn list`.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            SettingId::More => "more",
            SettingId::Rmsg => "rmsg",
            SettingId::Linesize => "linesize",
            SettingId::Pagesize => "pagesize",
            SettingId::Dp => "dp",
            SettingId::Varabbrev => "varabbrev",
            SettingId::Type => "type",
            SettingId::Level => "level",
            SettingId::Sortseed => "sortseed",
            SettingId::Seed => "seed",
            SettingId::Trace => "trace",
            SettingId::Obs => "obs",
        }
    }
}

/// The value a setting holds. Three shapes cover every `set` v1 accepts.
///
/// Deliberately NOT `Serialize`/`Deserialize`. The wire form of session
/// configuration is `SessionConfigWire` (CONTRACTS §9.1), which carries the four
/// settings a window can change and nothing else; a second serialisable settings
/// type would be a second wire contract for the same state, and the `&'static
/// str` that keeps `SettingsSnapshot::v1` allocation-free cannot round-trip
/// through `Deserialize` anyway.
#[derive(Clone, PartialEq, Debug)]
pub enum SettingValue {
    /// `set more off`.
    OnOff(bool),
    /// `set linesize 80`, `set level 95`.
    Num(f64),
    /// `set type float`, `set dp period`.
    Word(&'static str),
}

/// The full `c()` defaults table a clean session starts from.
///
/// A *versioned constant*, in design `03` §8's words: [`SettingsSnapshot::v1`]
/// is the only constructor, so there is no way to build a session whose settings
/// came from somewhere else — an environment variable, a preferences file, the
/// last interactive session — which is the failure mode item 8 exists to
/// prevent.
#[derive(Clone, PartialEq, Debug)]
pub struct SettingsSnapshot {
    /// Parallel to [`SettingId::FORCED`] plus the settings a clean session
    /// leaves at their defaults. Sorted by `SettingId`, so equality is
    /// order-independent without a map.
    entries: Vec<(SettingId, SettingValue)>,
}

impl SettingsSnapshot {
    /// The v1 constant table, with `sortseed` filled from the config.
    #[must_use]
    pub fn v1(sortseed: u64) -> Self {
        let mut entries = vec![
            (SettingId::More, SettingValue::OnOff(false)),
            (SettingId::Rmsg, SettingValue::OnOff(false)),
            (SettingId::Linesize, SettingValue::Num(f64::from(LINESIZE))),
            (SettingId::Pagesize, SettingValue::Num(0.0)),
            (SettingId::Dp, SettingValue::Word("period")),
            (SettingId::Varabbrev, SettingValue::OnOff(true)),
            (SettingId::Type, SettingValue::Word("float")),
            (SettingId::Level, SettingValue::Num(DEFAULT_LEVEL)),
            #[allow(clippy::cast_precision_loss)]
            (SettingId::Sortseed, SettingValue::Num(sortseed as f64)),
            (SettingId::Trace, SettingValue::OnOff(false)),
            (SettingId::Obs, SettingValue::Num(0.0)),
        ];
        entries.sort_by_key(|(id, _)| *id);
        Self { entries }
    }

    /// Look a setting up.
    #[must_use]
    pub fn get(&self, id: SettingId) -> Option<&SettingValue> {
        self.entries
            .binary_search_by_key(&id, |(k, _)| *k)
            .ok()
            .map(|i| &self.entries[i].1)
    }

    /// Every entry, sorted by [`SettingId`].
    #[must_use]
    pub fn entries(&self) -> &[(SettingId, SettingValue)] {
        &self.entries
    }

    /// `set <id> <value>`. Returns false for a setting this table has no slot
    /// for, which is how an unknown `set` reaches `rc = 10` rather than growing
    /// the table at runtime — a settings table that could grow is a settings
    /// table `SettingsSnapshot::v1` no longer describes.
    pub fn set(&mut self, id: SettingId, value: SettingValue) -> bool {
        match self.entries.binary_search_by_key(&id, |(k, _)| *k) {
            Ok(i) => {
                self.entries[i].1 = value;
                true
            }
            Err(_) => false,
        }
    }
}

/// What refused to build a [`SessionConfig`].
#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum ConfigError {
    /// C44/A16: v1 accepts `set linesize 80` and nothing else, and it refuses at
    /// construction rather than accepting the value and then emitting
    /// 80-column tables anyway.
    #[error("unsupported in this version: `set linesize` other than {LINESIZE} (got {0})")]
    Linesize(u16),
    /// The entry `.do` file has no parent directory, so item 7 has nothing to
    /// `cd` to.
    #[error("entry path {0} has no directory to run from")]
    NoEntryDirectory(Utf8PathBuf),
}

impl ConfigError {
    /// The Stata return code this maps to. `10` is spec §30's "unsupported
    /// feature", deliberately distinct from `1` ("we are wrong").
    #[must_use]
    pub fn rc(&self) -> u16 {
        match self {
            ConfigError::Linesize(_) => 10,
            ConfigError::NoEntryDirectory(_) => 601,
        }
    }
}

/// Design `03` §8, transcribed.
///
/// Every field is either a caller decision made once (`cwd`, `ado_path`,
/// `max_memory`) or a constant this build pins (`settings`, `linesize`,
/// `locale`). `Session::fresh` reads it and never writes it.
#[derive(Clone, PartialEq, Debug)]
pub struct SessionConfig {
    /// Item 7: the directory containing the entry `.do` file. Not the app's
    /// cwd, not the last-used directory — that is what makes a relative path in
    /// a shared repository work on a colleague's machine.
    pub cwd: Utf8PathBuf,
    /// Item 6.
    pub rng_kind: RngKind,
    /// Item 6. [`DEFAULT_SEED`] unless the caller overrides it.
    pub default_seed: u64,
    /// Item 6. Set explicitly, so `sort` ties are reproducible rather than
    /// merely usually-the-same.
    pub sortseed: u64,
    /// Item 8.
    pub settings: SettingsSnapshot,
    /// Item 10.
    pub version: StataVersion,
    /// Item 9. Every directory the caller resolved, **tagged with the
    /// `sysdir` slot it came from**.
    ///
    /// Tagged rather than a bare `Vec<Utf8PathBuf>` (design `03` §8's
    /// declaration) because item 9's content is the *exclusion*, and a path
    /// list with no slots cannot express it: `Session::fresh` would have to
    /// trust that whoever built the vector had already filtered `PERSONAL` and
    /// `PLUS` out, which puts the checklist item ARCHITECTURE §7.7 places in
    /// this crate outside this crate. With slots, `fresh` does the filtering,
    /// and passing the user's `PERSONAL` directory in here is harmless rather
    /// than silent.
    pub ado_path: Vec<AdoEntry>,
    /// Item 8. Always [`LINESIZE`]; [`SessionConfig::new`] refuses anything
    /// else.
    pub linesize: u16,
    /// Item 16.
    pub locale: LocaleMode,
    /// `None` means "whatever the machine has". Not part of the clean-state
    /// checklist: it changes whether a `use` is refused, never what a
    /// successful run computes.
    pub max_memory: Option<u64>,
    /// `03` §9.3. Always true in v1.
    pub deterministic_reductions: bool,
    /// Item 9's documented override. `false` for every clean run; `true` only
    /// when the user has explicitly asked for their `PERSONAL`/`PLUS` ado to be
    /// on the path, and the reproducibility report says so.
    pub ado_personal: bool,
    /// `Some(scratch)` redirects every write verb into that directory
    /// (`stratum run --clean --sandbox`). See [`crate::isolate::WriteSandbox`].
    pub write_sandbox: Option<Utf8PathBuf>,
}

impl SessionConfig {
    /// A configuration rooted at `cwd`, with every clean-state default.
    ///
    /// # Errors
    ///
    /// Never today — the signature is `Result` because [`SessionConfig::with_linesize`]
    /// and [`SessionConfig::from_wire`] can refuse, and a caller that starts
    /// from `new` should not have to change shape when it adds one.
    pub fn new(cwd: impl Into<Utf8PathBuf>) -> Result<Self, ConfigError> {
        let sortseed = 0;
        Ok(Self {
            cwd: cwd.into(),
            rng_kind: RngKind::Mt64,
            default_seed: DEFAULT_SEED,
            sortseed,
            settings: SettingsSnapshot::v1(sortseed),
            version: StataVersion::DEFAULT,
            ado_path: Vec::new(),
            linesize: LINESIZE,
            locale: LocaleMode::Utf8Cnumeric,
            max_memory: None,
            deterministic_reductions: true,
            ado_personal: false,
            write_sandbox: None,
        })
    }

    /// The configuration for running `entry` from clean state: item 7's cwd is
    /// the file's own directory.
    ///
    /// # Errors
    ///
    /// [`ConfigError::NoEntryDirectory`] when `entry` has no parent — a bare
    /// file name reaching here means the caller never resolved it, and
    /// defaulting to the process cwd is exactly the silent behaviour item 7
    /// exists to remove.
    pub fn for_entry(entry: &Utf8Path) -> Result<Self, ConfigError> {
        let dir = entry
            .parent()
            .filter(|p| !p.as_str().is_empty())
            .ok_or_else(|| ConfigError::NoEntryDirectory(entry.to_owned()))?;
        Self::new(dir)
    }

    /// Reject any `linesize` but [`LINESIZE`].
    ///
    /// # Errors
    ///
    /// [`ConfigError::Linesize`], carrying `rc = 10`.
    pub fn with_linesize(mut self, n: u16) -> Result<Self, ConfigError> {
        if n != LINESIZE {
            return Err(ConfigError::Linesize(n));
        }
        self.linesize = n;
        Ok(self)
    }

    /// Apply the wire subset a window sent.
    ///
    /// # Errors
    ///
    /// [`ConfigError::Linesize`] — the wire type carries a `u16`, so this is the
    /// boundary where C44 is enforced.
    pub fn from_wire(wire: &SessionConfigWire, base: &Utf8Path) -> Result<Self, ConfigError> {
        let cwd = wire.cwd.clone().unwrap_or_else(|| base.to_owned());
        let mut cfg = Self::new(cwd)?.with_linesize(wire.linesize)?;
        if let Some(seed) = wire.seed {
            cfg.default_seed = seed;
        }
        cfg.max_memory = wire.max_memory_bytes;
        cfg.ado_personal = wire.ado_personal;
        cfg.write_sandbox = wire.write_sandbox.clone();
        cfg.settings
            .entries
            .iter_mut()
            .for_each(|(id, v)| match (*id, v) {
                (SettingId::Varabbrev, SettingValue::OnOff(b)) => *b = wire.varabbrev,
                (SettingId::More, SettingValue::OnOff(b)) => *b = wire.more,
                (SettingId::Level, SettingValue::Num(n)) => *n = wire.level,
                _ => {}
            });
        Ok(cfg)
    }

    /// The subset that crosses the wire (CONTRACTS §9.1).
    #[must_use]
    pub fn to_wire(&self) -> SessionConfigWire {
        SessionConfigWire {
            cwd: Some(self.cwd.clone()),
            seed: Some(self.default_seed),
            linesize: self.linesize,
            level: match self.settings.get(SettingId::Level) {
                Some(SettingValue::Num(n)) => *n,
                _ => DEFAULT_LEVEL,
            },
            varabbrev: matches!(
                self.settings.get(SettingId::Varabbrev),
                Some(SettingValue::OnOff(true))
            ),
            more: matches!(
                self.settings.get(SettingId::More),
                Some(SettingValue::OnOff(true))
            ),
            max_memory_bytes: self.max_memory,
            ado_personal: self.ado_personal,
            write_sandbox: self.write_sandbox.clone(),
        }
    }
}
