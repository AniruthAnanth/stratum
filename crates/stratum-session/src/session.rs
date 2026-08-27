//! [`Session`] — the engine-side session, one owning OS thread, `!Sync`.
//!
//! The type is a *product of its sixteen namespaces* and nothing else. That is
//! the whole design: `#[derive(PartialEq)]` on this struct is what makes the
//! clean-state test total, because a seventeenth namespace added next year
//! enters the comparison the moment it is added as a field. A hand-written `eq`
//! that listed fifteen of sixteen would be the exact failure mode ARCHITECTURE
//! §7.7 wrote "constructs a new one" to remove.
//!
//! There is deliberately **no `reset`, no `clear_all`, no `Session::reuse`**.
//! The only way to obtain a clean session is [`Session::fresh`], in
//! [`crate::fresh`], and the only way to dirty one is through the mutators here.

use std::sync::atomic::{AtomicU32, Ordering as AtomicOrdering};

use camino::Utf8PathBuf;
use indexmap::IndexMap;
use rustc_hash::FxHashMap;
use stratum_core::Value;
use stratum_parse::MacroEnv;
use stratum_proto::{ExecutionId, SessionEpoch, SessionId, Taint};

use crate::ado::AdoState;
use crate::config::{RngKind, SessionConfig, SettingId, SettingValue, SettingsSnapshot};
use crate::frames::Frames;

/// Item 6. Draw position, not the generator itself: `stratum-runtime` owns the
/// Mersenne Twister; what a session owns is *where it is*, because that is what
/// makes a block that drew random numbers stale when the seed moves.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RngState {
    /// Which generator.
    pub kind: RngKind,
    /// The execution that last ran `set seed`. `ExecutionId(0)` means "the
    /// session default", which is what lint R002 keys on: a file that never set
    /// a seed is not reproducible even though every run of it agrees.
    pub seed_origin: ExecutionId,
    /// The seed value in force.
    pub seed_value: u64,
    /// Draws consumed since that seed. Zero in a fresh session, and it must be
    /// zero rather than merely "the same as last time" — a resumed draw
    /// position is a clean run that produces different numbers.
    pub draws: u64,
    /// `set sortseed`, which decides how `sort` breaks ties.
    pub sortseed: u64,
}

impl RngState {
    /// Item 6's constructor.
    #[must_use]
    pub fn fresh(cfg: &SessionConfig) -> Self {
        Self {
            kind: cfg.rng_kind,
            seed_origin: ExecutionId(0),
            seed_value: cfg.default_seed,
            draws: 0,
            sortseed: cfg.sortseed,
        }
    }

    /// True when the seed came from the session default rather than from a
    /// `set seed` in the file. Lint R002 reports it; the reproducibility report
    /// prints "seed: session default (123456789)".
    #[must_use]
    pub fn seed_is_default(&self) -> bool {
        self.seed_origin == ExecutionId(0)
    }

    /// Item 6's audit.
    #[must_use]
    pub fn is_clean(&self, cfg: &SessionConfig) -> bool {
        *self == RngState::fresh(cfg)
    }
}

/// Item 4. Scalars and matrices, including the estimation-owned ones.
///
/// `_b`, `_se` and `r(table)` are named in the checklist because they are the
/// three a user never created and therefore never thinks to `drop`; they are
/// stored here rather than inside the estimates namespace because `matrix list
/// _b` reaches them by name like any other matrix.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct ScalarStore {
    /// `scalar` and `tempname` scalars. Not iterated where the order can reach
    /// output — `scalar dir` sorts.
    map: FxHashMap<String, Value>,
}

impl ScalarStore {
    /// Look one up.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Value> {
        self.map.get(name)
    }

    /// `scalar name = value`.
    pub fn set(&mut self, name: &str, v: Value) {
        self.map.insert(name.to_owned(), v);
    }

    /// `scalar drop`.
    pub fn drop_scalar(&mut self, name: &str) -> bool {
        self.map.remove(name).is_some()
    }

    /// `scalar dir`, sorted — the iteration order of the map must never reach
    /// output.
    #[must_use]
    pub fn names(&self) -> Vec<&str> {
        let mut v: Vec<&str> = self.map.keys().map(String::as_str).collect();
        v.sort_unstable();
        v
    }

    /// How many.
    #[must_use]
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// Item 4's audit, half of it.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

/// A matrix, stored row-major.
#[derive(Clone, PartialEq, Debug)]
pub struct Matrix {
    /// Row count.
    pub rows: usize,
    /// Column count.
    pub cols: usize,
    /// `rows * cols` values, row-major.
    pub data: Vec<f64>,
    /// `matrix rownames`.
    pub rownames: Vec<String>,
    /// `matrix colnames`.
    pub colnames: Vec<String>,
}

/// Item 4's other half.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct MatrixStore {
    map: FxHashMap<String, Matrix>,
}

impl MatrixStore {
    /// Look one up.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Matrix> {
        self.map.get(name)
    }

    /// `matrix name = ...`.
    pub fn set(&mut self, name: &str, m: Matrix) {
        self.map.insert(name.to_owned(), m);
    }

    /// `matrix drop`.
    pub fn drop_matrix(&mut self, name: &str) -> bool {
        self.map.remove(name).is_some()
    }

    /// `matrix dir`, sorted.
    #[must_use]
    pub fn names(&self) -> Vec<&str> {
        let mut v: Vec<&str> = self.map.keys().map(String::as_str).collect();
        v.sort_unstable();
        v
    }

    /// How many.
    #[must_use]
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// Item 4's audit, the other half.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

/// Item 5. `estimates store`, plus the three result singletons.
///
/// The singletons are `IndexMap`s because `return list` prints them in insertion
/// order, and `stratum-stats`' `ResultSet` is insertion-ordered for the same
/// reason. Sorting them here would disagree with the classic text.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct EstimateStore {
    /// `estimates store <name>`, in store order.
    pub stored: IndexMap<String, StoredEstimate>,
    /// `e()` — the active estimation results.
    pub e: IndexMap<String, Value>,
    /// `r()` — the last r-class command's results.
    pub r: IndexMap<String, Value>,
    /// `s()` — the last s-class command's results.
    pub s: IndexMap<String, Value>,
}

impl EstimateStore {
    /// Item 5's audit: `estimates clear`, and all three singletons empty.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.stored.is_empty() && self.e.is_empty() && self.r.is_empty() && self.s.is_empty()
    }
}

/// One `estimates store` entry.
#[derive(Clone, PartialEq, Debug)]
pub struct StoredEstimate {
    /// `e(cmd)`.
    pub cmd: String,
    /// The whole `e()` set as it was at store time.
    pub e: IndexMap<String, Value>,
    /// Which execution produced it — spec §19 "Compare models" shows it.
    pub from: ExecutionId,
}

/// Item 11. `set trace`, `capture` nesting, and the `preserve` stack.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct ControlState {
    /// `set trace off`.
    pub trace: bool,
    /// How many `capture` prefixes are currently in force. A clean session is at
    /// zero; a session that inherited a non-zero depth would swallow the first
    /// error of the run.
    pub capture_depth: u32,
    /// The `preserve` stack. Each entry names its spill file, because a
    /// `preserve` that spilled to disk and was never `restore`d leaves the file
    /// behind — item 11 says the stack is emptied *and* the spill files are
    /// deleted, and one without the other is a leak.
    pub preserve: Vec<PreserveEntry>,
}

impl ControlState {
    /// Item 11's audit.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        *self == ControlState::default()
    }
}

/// One `preserve` frame.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PreserveEntry {
    /// Where the snapshot spilled, if it did.
    pub spill: Option<Utf8PathBuf>,
    /// The frame it was taken from.
    pub frame: String,
}

/// Item 12. Named graphs and the scheme in force.
#[derive(Clone, PartialEq, Debug)]
pub struct GraphStore {
    /// `graph dir`, in creation order.
    pub graphs: IndexMap<String, GraphHandle>,
    /// `set scheme`. Reset to the configured default by a fresh session; the
    /// name is what `stratum_tokens::SCHEMES` is keyed by.
    pub scheme: String,
}

/// The default scheme name. Compiled in, never read from disk (A14).
pub const DEFAULT_SCHEME: &str = "stratum";

impl Default for GraphStore {
    fn default() -> Self {
        Self {
            graphs: IndexMap::new(),
            scheme: DEFAULT_SCHEME.to_owned(),
        }
    }
}

impl GraphStore {
    /// Item 12's audit: `graph drop _all` and the default scheme.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.graphs.is_empty() && self.scheme == DEFAULT_SCHEME
    }
}

/// One live graph.
#[derive(Clone, PartialEq, Debug)]
pub struct GraphHandle {
    /// The rendered SVG, or the path it was exported to.
    pub svg: String,
    /// Which execution drew it.
    pub from: ExecutionId,
}

/// Item 13. `file open` and `postfile` handles.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct FileHandles {
    /// `file open <handle> using <path>`, in open order.
    pub files: IndexMap<String, OpenFile>,
    /// `postfile <handle> ... using <path>`, in open order. A `postfile` that
    /// was never `postclose`d has a temp file behind it, which item 13 removes
    /// rather than leaks.
    pub postfiles: IndexMap<String, OpenFile>,
}

impl FileHandles {
    /// Item 13's audit.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.files.is_empty() && self.postfiles.is_empty()
    }
}

/// One open handle.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct OpenFile {
    /// The path behind the handle.
    pub path: Utf8PathBuf,
    /// True for `read`, false for `write`/`append`.
    pub read: bool,
    /// True when the path is a temporary this session created and must delete.
    pub temporary: bool,
}

/// Item 15. Every ambient read, recorded.
///
/// The checklist does not say environment reads are *forbidden* — `c(username)`
/// still answers — it says every one is **recorded** and sets a [`Taint`] bit.
/// That is the difference between a reproducibility report that can say "this
/// file reads `$HOME`, so I cannot promise it runs elsewhere" and one that
/// silently ticks a box.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct EnvTaint {
    /// Every read, in order, with duplicates — the count is evidence.
    pub reads: Vec<EnvRead>,
    /// The union of the bits those reads set.
    pub taint: Taint,
}

impl EnvTaint {
    /// Record a read of `what`, setting the bit its kind implies.
    pub fn record(&mut self, what: EnvSource, name: &str) {
        self.taint |= what.taint();
        self.reads.push(EnvRead {
            source: what,
            name: name.to_owned(),
        });
    }

    /// Item 15's audit: nothing read yet, no bits set.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.reads.is_empty() && self.taint.is_empty()
    }
}

/// One recorded ambient read.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct EnvRead {
    /// What kind of ambient value it was.
    pub source: EnvSource,
    /// The specific name — the variable for `getenv`, the `c()` name otherwise.
    pub name: String,
}

/// The ambient surfaces a session can read.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EnvSource {
    /// `getenv`, `$S_something`, an environment variable by any route.
    Env,
    /// `c(username)`, `c(hostname)`, `c(machine_type)`, `c(os)`.
    Machine,
    /// `c(current_date)`, `c(current_time)`, and anything else that reads a
    /// clock. Its own bit because a clock read makes a run non-reproducible in a
    /// different way from a hostname read: it changes on the *same* machine.
    Clock,
}

impl EnvSource {
    /// The bit this source sets.
    #[must_use]
    pub fn taint(self) -> Taint {
        match self {
            EnvSource::Env | EnvSource::Machine => Taint::ENVIRONMENT,
            EnvSource::Clock => Taint::CLOCK,
        }
    }
}

/// Allocates the `SessionId`s. Process-wide and monotone, so two sessions in one
/// engine never collide and a recycled id cannot make two different sessions
/// look like one in a log.
static NEXT_SESSION_ID: AtomicU32 = AtomicU32::new(1);

/// The engine-side session.
///
/// One session is one owning OS thread (`03` §9.1); nothing here is `Sync` by
/// intent, and the type is deliberately not `Clone` — a cloned session is two
/// sessions that both believe they own the same `SessionId`.
#[derive(PartialEq, Debug)]
pub struct Session {
    /// The configuration this session was constructed from. Read-only after
    /// construction; the mutable shadow of it is [`Session::settings`].
    pub(crate) config: SessionConfig,
    /// Identity. Excluded from nothing — two sessions with different ids are
    /// different sessions, and the clean-state test compares two sessions built
    /// from the same id on purpose (see [`crate::fresh`]).
    pub(crate) id: SessionId,
    /// Bumps on every clean run and every `clear all`.
    pub(crate) epoch: SessionEpoch,

    /// Items 1 and 2.
    pub(crate) frames: Frames,
    /// Item 3, and item 14 — `MacroEnv` owns the `__000000` counter that
    /// `tempvar`, `tempfile` and `tempname` all draw from, which is exactly one
    /// counter, as Stata has.
    pub(crate) macros: MacroEnv,
    /// Item 4.
    pub(crate) scalars: ScalarStore,
    /// Item 4.
    pub(crate) matrices: MatrixStore,
    /// Item 5.
    pub(crate) estimates: EstimateStore,
    /// Item 6.
    pub(crate) rng: RngState,
    /// Item 7. The session's own working directory, never the process's — a
    /// second session in the same engine process runs a different file from a
    /// different directory at the same time.
    pub(crate) cwd: Utf8PathBuf,
    /// Item 8.
    pub(crate) settings: SettingsSnapshot,
    /// Item 9.
    pub(crate) ado: AdoState,
    /// Item 10.
    pub(crate) version: crate::config::StataVersion,
    /// Item 11.
    pub(crate) control: ControlState,
    /// Item 12.
    pub(crate) graphs: GraphStore,
    /// Item 13.
    pub(crate) files: FileHandles,
    /// Item 15.
    pub(crate) env: EnvTaint,
    /// Item 16. One variant today; a field rather than a constant so that
    /// changing it is a change to session state that the equality catches.
    pub(crate) locale: crate::config::LocaleMode,
    /// Open documents. Not one of the sixteen — a document is not session
    /// *state* in the staleness sense, it is what the session is asked about —
    /// but it is a field, so it is in the derived equality, and a fresh session
    /// has none.
    pub(crate) docs: crate::document::Documents,
    /// The `BlockId` allocator CONTRACTS §2 step 3 calls "the session counter".
    /// Never reused, never reset by anything but constructing a session.
    pub(crate) next_block: u64,
}

impl Session {
    /// Take the next process-wide session id.
    pub(crate) fn next_id() -> SessionId {
        SessionId(NEXT_SESSION_ID.fetch_add(1, AtomicOrdering::Relaxed))
    }

    /// The configuration this session was built from.
    #[must_use]
    pub fn config(&self) -> &SessionConfig {
        &self.config
    }

    /// Identity.
    #[must_use]
    pub fn id(&self) -> SessionId {
        self.id
    }

    /// The epoch, which bumps on every clean run.
    #[must_use]
    pub fn epoch(&self) -> SessionEpoch {
        self.epoch
    }

    /// Items 1 and 2.
    #[must_use]
    pub fn frames(&self) -> &Frames {
        &self.frames
    }

    /// Items 1 and 2, mutably.
    pub fn frames_mut(&mut self) -> &mut Frames {
        &mut self.frames
    }

    /// Item 3 (and the counter behind item 14).
    #[must_use]
    pub fn macros(&self) -> &MacroEnv {
        &self.macros
    }

    /// Item 3, mutably.
    pub fn macros_mut(&mut self) -> &mut MacroEnv {
        &mut self.macros
    }

    /// Item 4.
    #[must_use]
    pub fn scalars(&self) -> &ScalarStore {
        &self.scalars
    }

    /// Item 4, mutably.
    pub fn scalars_mut(&mut self) -> &mut ScalarStore {
        &mut self.scalars
    }

    /// Item 4.
    #[must_use]
    pub fn matrices(&self) -> &MatrixStore {
        &self.matrices
    }

    /// Item 4, mutably.
    pub fn matrices_mut(&mut self) -> &mut MatrixStore {
        &mut self.matrices
    }

    /// Item 5.
    #[must_use]
    pub fn estimates(&self) -> &EstimateStore {
        &self.estimates
    }

    /// Item 5, mutably.
    pub fn estimates_mut(&mut self) -> &mut EstimateStore {
        &mut self.estimates
    }

    /// Item 6.
    #[must_use]
    pub fn rng(&self) -> &RngState {
        &self.rng
    }

    /// Item 6, mutably. `set seed` goes through here.
    pub fn rng_mut(&mut self) -> &mut RngState {
        &mut self.rng
    }

    /// Item 7.
    #[must_use]
    pub fn cwd(&self) -> &Utf8PathBuf {
        &self.cwd
    }

    /// Item 7. `cd`.
    pub fn set_cwd(&mut self, dir: impl Into<Utf8PathBuf>) {
        self.cwd = dir.into();
    }

    /// Item 8.
    #[must_use]
    pub fn settings(&self) -> &SettingsSnapshot {
        &self.settings
    }

    /// Item 8. `c(linesize)` — always [`crate::config::LINESIZE`], in every code
    /// path, which is C44/A16's other half.
    #[must_use]
    pub fn linesize(&self) -> u16 {
        crate::config::LINESIZE
    }

    /// Item 8, one setting at a time.
    #[must_use]
    pub fn setting(&self, id: SettingId) -> Option<&SettingValue> {
        self.settings.get(id)
    }

    /// `set <id> <value>`.
    ///
    /// `set linesize` is refused here rather than at the parser, because
    /// `c(linesize)` must report 80 in *every* code path (C44/A16) and the only
    /// way to guarantee that is for the value never to change. W06 turns the
    /// `false` into `rc = 10` with `STRATUM0010`.
    pub fn set_setting(&mut self, id: SettingId, value: SettingValue) -> bool {
        if id == SettingId::Linesize {
            return false;
        }
        self.settings.set(id, value)
    }

    /// Item 9.
    #[must_use]
    pub fn ado(&self) -> &AdoState {
        &self.ado
    }

    /// Item 9, mutably.
    pub fn ado_mut(&mut self) -> &mut AdoState {
        &mut self.ado
    }

    /// Item 10.
    #[must_use]
    pub fn version(&self) -> crate::config::StataVersion {
        self.version
    }

    /// Item 10. A `version 16` statement in the file.
    pub fn set_version(&mut self, v: crate::config::StataVersion) {
        self.version = v;
    }

    /// Item 11.
    #[must_use]
    pub fn control(&self) -> &ControlState {
        &self.control
    }

    /// Item 11, mutably.
    pub fn control_mut(&mut self) -> &mut ControlState {
        &mut self.control
    }

    /// Item 12.
    #[must_use]
    pub fn graphs(&self) -> &GraphStore {
        &self.graphs
    }

    /// Item 12, mutably.
    pub fn graphs_mut(&mut self) -> &mut GraphStore {
        &mut self.graphs
    }

    /// Item 13.
    #[must_use]
    pub fn files(&self) -> &FileHandles {
        &self.files
    }

    /// Item 13, mutably.
    pub fn files_mut(&mut self) -> &mut FileHandles {
        &mut self.files
    }

    /// Item 15.
    #[must_use]
    pub fn env(&self) -> &EnvTaint {
        &self.env
    }

    /// Item 15, mutably — every ambient read goes through
    /// [`EnvTaint::record`].
    pub fn env_mut(&mut self) -> &mut EnvTaint {
        &mut self.env
    }

    /// Item 16.
    #[must_use]
    pub fn locale(&self) -> crate::config::LocaleMode {
        self.locale
    }

    /// Item 16, as the comparison itself.
    #[must_use]
    pub fn collate(&self, a: &str, b: &str) -> std::cmp::Ordering {
        crate::frames::collate(a, b)
    }

    /// Item 14. How many temporary names this session has issued.
    #[must_use]
    pub fn tempnames_issued(&self) -> u32 {
        self.macros.temps_issued()
    }

    /// Item 14. The next `tempvar`/`tempfile`/`tempname`.
    pub fn alloc_tempname(&mut self) -> String {
        self.macros.alloc_temp()
    }

    /// Allocate the next `BlockId` — CONTRACTS §2 rule 3's "session counter".
    ///
    /// Never reused: a retired block's id must not come back on a later
    /// reconcile, or its `ExecutionRecord`s in the append-only ledger would
    /// attach to a block the user never ran.
    pub fn alloc_block_id(&mut self) -> stratum_proto::BlockId {
        // Starts at 1: `BlockId(0)` is `EPHEMERAL`. `BlockId::NONE` is
        // `u64::MAX` and is unreachable at any plausible edit count.
        let id = stratum_proto::BlockId(self.next_block);
        self.next_block += 1;
        id
    }

    /// The highest id this session has issued — `BlockId(0)` when none.
    ///
    /// The counter lives here as a plain `u64` rather than as a
    /// `stratum_runtime::doc::BlockIdAlloc` for one reason: `Session` derives
    /// `PartialEq` and the derive is what makes the clean-state test total, so
    /// every field has to be comparable. `document.rs` builds an allocator that
    /// *resumes after* this and writes the high-water mark back, so there is
    /// still exactly one counter.
    #[must_use]
    pub fn high_block_id(&self) -> stratum_proto::BlockId {
        stratum_proto::BlockId(self.next_block - 1)
    }

    /// Advance the counter past `high`. Never moves it backwards: an allocator
    /// that consumed nothing must not un-issue ids.
    pub(crate) fn set_high_block_id(&mut self, high: stratum_proto::BlockId) {
        self.next_block = self.next_block.max(high.0 + 1);
    }

    /// How many block ids have been handed out. The counter, not a name — the
    /// same reason `TempAlloc::issued` exists.
    #[must_use]
    pub fn blocks_issued(&self) -> u64 {
        self.next_block - 1
    }
}
