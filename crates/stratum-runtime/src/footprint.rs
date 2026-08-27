//! The dynamic dependency footprint — `03` §4.7, and the read/write barriers
//! that record it.
//!
//! [`DepFootprint`] is the *precise* record of what one execution actually read,
//! and it is what condition C6 of the staleness rule consults. C6 is allowed to
//! be exact — rather than conservative like the document-order clause C7 —
//! precisely because this list is complete: the command ABI hands an
//! implementation `&mut ExecCtx` and no ambient access to `std::env`,
//! `SystemTime`, or the filesystem, so there is no input a command can observe
//! that does not pass through a barrier here (`03` §6.3). The escape hatches
//! (`shell`, `python`, `java`, plugins) set `Taint::EXTERNAL` and are demoted to
//! `CurrentUnverifiable` instead of being lied about.
//!
//! # Recording is O(columns touched), not O(rows)
//!
//! [`FootprintBuilder::note_read`] and [`FootprintBuilder::note_write`] set one
//! bit with a relaxed `fetch_or` — ~1 ns, safe to call from a rayon worker
//! through `&self`, and called **once per column per command**, never per
//! element. Resolving those bits to versions happens once, at commit, against
//! the fingerprint captured at block entry. So the hot loop costs one atomic OR
//! per column and the commit costs O(columns touched).
//!
//! # Self-reads, and the one place the normative rule needs spelling out
//!
//! `03` §4.7: *"`note_read(v)` is a no-op if `v` is **already in** this
//! execution's `vars_written ∪ vars_created`."* The emphasis is load-bearing —
//! the test is against the set as it stands **at the moment of the read**:
//!
//! * `gen z = x + 1` then `replace z = z*2` — the read of `z` happens after `z`
//!   was created, so the block depends on `x` and not on itself. Without this,
//!   every multi-statement block would depend on its own output and could never
//!   be Current.
//! * `replace x = x + 1` — the read of `x` happens *before* the write, so `x`
//!   stays in the footprint. It has to: if an upstream `gen x` is edited, C7
//!   intersects that block's writes with this block's reads, and dropping `x`
//!   would leave this block showing ✓ Current against an `x` that no longer
//!   exists in the form it consumed. That is under-marking, which is the one
//!   direction INV-1 does not tolerate.
//!
//! A variable that is both read and written is therefore recorded at its
//! **post-commit** version (see [`FootprintBuilder::finish`]). Recording the
//! entry version would make `replace x = x+1` report itself stale the instant it
//! finished, since C6 would compare the version it read against the version it
//! had just written. `row_membership` is treated the same way for the same
//! reason.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use stratum_proto::{DepKey, FrameId, Taint, VarId};

use crate::state::dataset::DatasetFingerprint;
use crate::state::fingerprint::{FileStamp, Ns, PathKey, RngFingerprint, StateFingerprint};
use crate::state::versions::VarVersion;

/// What one execution read — `03` §4.7.
///
/// `vars` is sorted by `VarId` so two footprints compare without a sort.
///
/// `Default` is hand-written because `FrameId` has none and should not: a
/// default frame id is a plausible-looking wrong answer, and `FrameId(0)` is
/// meaningful only as "the frame this empty record does not describe".
#[derive(Clone, Debug, PartialEq)]
pub struct DepFootprint {
    /// The frame the execution ran against.
    pub frame: FrameId,
    /// Columns read, at the version read. Sorted by `VarId`.
    pub vars: Vec<VarVersion>,
    /// The row-membership counter, when the execution's answer depends on which
    /// rows exist.
    pub row_membership: Option<u64>,
    /// The row-order counter — `Some` only for an order-sensitive block
    /// (`03` §4.8). This is the second-largest source of avoided over-marking.
    pub row_order: Option<u64>,
    /// The column-layout counter — `Some` only for a metadata-sensitive block.
    pub var_layout: Option<u64>,
    /// Macros read, at the version read.
    pub macros: Vec<(Box<str>, u64)>,
    /// Scalars read.
    pub scalars: Vec<(Box<str>, u64)>,
    /// Matrices read.
    pub matrices: Vec<(Box<str>, u64)>,
    /// Programs called.
    pub programs: Vec<(Box<str>, u64)>,
    /// `e()` and the stored-estimates table.
    pub estimates: Option<u64>,
    /// `r()`.
    pub rclass: Option<u64>,
    /// `s()`.
    pub sclass: Option<u64>,
    /// Characteristics.
    pub chars: Option<u64>,
    /// The random-number stream at block entry. `draws` is included on purpose:
    /// an upstream block that consumes a different number of draws genuinely
    /// changes this block's numbers (`03` §6.3).
    pub rng: Option<RngFingerprint>,
    /// `set`/`c()` values read.
    pub settings: Vec<(Box<str>, u64)>,
    /// The working directory version, when a relative path was resolved.
    pub cwd: Option<u64>,
    /// External inputs, at the stamp read.
    pub files: Vec<(PathKey, FileStamp)>,
    /// Why this record is weaker than exact.
    pub taint: Taint,
}

impl Default for DepFootprint {
    fn default() -> Self {
        Self {
            frame: FrameId(0),
            vars: Vec::new(),
            row_membership: None,
            row_order: None,
            var_layout: None,
            macros: Vec::new(),
            scalars: Vec::new(),
            matrices: Vec::new(),
            programs: Vec::new(),
            estimates: None,
            rclass: None,
            sclass: None,
            chars: None,
            rng: None,
            settings: Vec::new(),
            cwd: None,
            files: Vec::new(),
            taint: Taint::empty(),
        }
    }
}

impl Default for WriteFootprint {
    fn default() -> Self {
        Self {
            frame: FrameId(0),
            vars_written: Vec::new(),
            vars_created: Vec::new(),
            vars_dropped: Vec::new(),
            renamed: Vec::new(),
            changed_membership: false,
            changed_order: false,
            changed_layout: false,
            macros_set: Vec::new(),
            scalars_set: Vec::new(),
            matrices_set: Vec::new(),
            programs_set: Vec::new(),
            settings_set: Vec::new(),
            set_estimates: false,
            set_rclass: false,
            set_sclass: false,
            set_chars: false,
            files_written: Vec::new(),
            changed_cwd: false,
            changed_rng: false,
            taint: Taint::empty(),
        }
    }
}

impl DepFootprint {
    /// Every dependency as the `DepKey` the stale banner renders, in a stable
    /// order. `frame_name` is passed in because a `FrameId` has no name here.
    #[must_use]
    pub fn keys(&self, frame_name: &str, name_of: &dyn Fn(VarId) -> Option<String>) -> Vec<DepKey> {
        let f = || frame_name.to_owned();
        let mut out = Vec::new();
        for v in &self.vars {
            if let Some(name) = name_of(v.var) {
                out.push(DepKey::Var { frame: f(), name });
            }
        }
        if self.row_membership.is_some() {
            out.push(DepKey::RowMembership { frame: f() });
        }
        if self.row_order.is_some() {
            out.push(DepKey::RowOrder { frame: f() });
        }
        if self.var_layout.is_some() {
            out.push(DepKey::VarLayout { frame: f() });
        }
        let named = [
            (&self.macros, 0u8),
            (&self.scalars, 1),
            (&self.matrices, 2),
            (&self.programs, 3),
            (&self.settings, 4),
        ];
        for (list, kind) in named {
            for (name, _) in list.iter() {
                let name = name.to_string();
                out.push(match kind {
                    0 => DepKey::Macro { name },
                    1 => DepKey::Scalar { name },
                    2 => DepKey::Matrix { name },
                    3 => DepKey::Program { name },
                    _ => DepKey::Setting { name },
                });
            }
        }
        if self.estimates.is_some() {
            out.push(DepKey::Estimates);
        }
        if self.rclass.is_some() {
            out.push(DepKey::RClass);
        }
        if self.sclass.is_some() {
            out.push(DepKey::SClass);
        }
        if self.rng.is_some() {
            out.push(DepKey::Rng);
        }
        if self.cwd.is_some() {
            out.push(DepKey::Cwd);
        }
        for (p, _) in &self.files {
            out.push(DepKey::File { path: p.0.clone() });
        }
        out
    }

    /// True when nothing was read.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self
            == DepFootprint {
                frame: self.frame,
                ..DepFootprint::default()
            }
    }
}

/// What one execution wrote — `03` §4.7. Drives C7's `Writes_pending`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WriteFootprint {
    /// The frame written.
    pub frame: FrameId,
    /// Existing columns whose values moved.
    pub vars_written: Vec<VarId>,
    /// Columns created.
    pub vars_created: Vec<VarId>,
    /// Columns dropped, with the name they had.
    pub vars_dropped: Vec<(VarId, Box<str>)>,
    /// `(id, from, to)`. Identity survives a rename, so this is not a drop plus
    /// a create.
    pub renamed: Vec<(VarId, Box<str>, Box<str>)>,
    /// Which rows exist changed.
    pub changed_membership: bool,
    /// The order of rows changed.
    pub changed_order: bool,
    /// Column layout or variable metadata changed.
    pub changed_layout: bool,
    /// Macros assigned.
    pub macros_set: Vec<Box<str>>,
    /// Scalars assigned.
    pub scalars_set: Vec<Box<str>>,
    /// Matrices assigned.
    pub matrices_set: Vec<Box<str>>,
    /// Programs defined or dropped.
    pub programs_set: Vec<Box<str>>,
    /// `e()` was replaced.
    pub set_estimates: bool,
    /// `r()` was replaced.
    pub set_rclass: bool,
    /// `s()` was replaced.
    pub set_sclass: bool,
    /// Characteristics changed.
    pub set_chars: bool,
    /// Settings written.
    pub settings_set: Vec<Box<str>>,
    /// Files written.
    pub files_written: Vec<PathKey>,
    /// `cd`.
    pub changed_cwd: bool,
    /// The RNG stream advanced or was reseeded.
    pub changed_rng: bool,
    /// Unknown-write taint from static analysis, carried through.
    pub taint: Taint,
}

impl WriteFootprint {
    /// True when this execution changed nothing an observer could see.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self
            == WriteFootprint {
                frame: self.frame,
                ..WriteFootprint::default()
            }
    }
}

/// A lock-free bitset over `VarId` slots, with a mutexed overflow list.
///
/// Sized at block entry from the session's variable counter. The overflow list
/// exists because a command may create columns whose ids sit above that mark,
/// and a dependency dropped for being out of range is a soundness bug, not a
/// missed optimisation. Overflow is taken at most once per *new* column, so it
/// is never on a hot path.
#[derive(Debug)]
struct SlotBits {
    words: Vec<AtomicU64>,
    overflow: Mutex<Vec<VarId>>,
}

impl SlotBits {
    fn with_slots(n: usize) -> Self {
        Self {
            words: (0..n.div_ceil(64)).map(|_| AtomicU64::new(0)).collect(),
            overflow: Mutex::new(Vec::new()),
        }
    }

    /// Set the bit for `var`; returns true if it was not already set.
    fn set(&self, var: VarId) -> bool {
        let i = var.0 as usize;
        match self.words.get(i / 64) {
            Some(w) => {
                let bit = 1u64 << (i % 64);
                (w.fetch_or(bit, Ordering::Relaxed) & bit) == 0
            }
            None => {
                let mut o = self.overflow.lock().expect("footprint overflow list");
                if o.contains(&var) {
                    false
                } else {
                    o.push(var);
                    true
                }
            }
        }
    }

    fn contains(&self, var: VarId) -> bool {
        let i = var.0 as usize;
        match self.words.get(i / 64) {
            Some(w) => (w.load(Ordering::Relaxed) & (1u64 << (i % 64))) != 0,
            None => self
                .overflow
                .lock()
                .expect("footprint overflow list")
                .contains(&var),
        }
    }

    /// Every set slot, ascending.
    fn iter(&self) -> Vec<VarId> {
        let mut out = Vec::new();
        for (wi, w) in self.words.iter().enumerate() {
            let mut bits = w.load(Ordering::Relaxed);
            while bits != 0 {
                let b = bits.trailing_zeros() as usize;
                out.push(VarId((wi * 64 + b) as u32));
                bits &= bits - 1;
            }
        }
        let mut o = self
            .overflow
            .lock()
            .expect("footprint overflow list")
            .clone();
        o.sort_unstable();
        out.extend(o);
        out
    }

    fn is_empty(&self) -> bool {
        self.words.iter().all(|w| w.load(Ordering::Relaxed) == 0)
            && self
                .overflow
                .lock()
                .expect("footprint overflow list")
                .is_empty()
    }
}

/// The named-namespace side channel. One entry per name per command.
#[derive(Debug, Default)]
struct Named {
    macro_reads: Vec<Box<str>>,
    scalar_reads: Vec<Box<str>>,
    matrix_reads: Vec<Box<str>>,
    program_reads: Vec<Box<str>>,
    setting_reads: Vec<Box<str>>,
    file_reads: Vec<(PathKey, FileStamp)>,

    macro_writes: Vec<Box<str>>,
    scalar_writes: Vec<Box<str>>,
    matrix_writes: Vec<Box<str>>,
    program_writes: Vec<Box<str>>,
    setting_writes: Vec<Box<str>>,
    file_writes: Vec<PathKey>,

    dropped: Vec<(VarId, Box<str>)>,
    renamed: Vec<(VarId, Box<str>, Box<str>)>,
}

fn push_unique(v: &mut Vec<Box<str>>, name: &str) -> bool {
    if v.iter().any(|n| n.as_ref() == name) {
        false
    } else {
        v.push(name.into());
        true
    }
}

/// Records one execution's reads and writes. One per command.
#[derive(Debug)]
pub struct FootprintBuilder {
    frame: FrameId,
    reads: SlotBits,
    writes: SlotBits,
    creates: SlotBits,
    named: Mutex<Named>,
    flags: Flags,
    entry: Entry,
}

#[derive(Debug, Default)]
struct Flags {
    read_membership: AtomicU64,
    read_order: AtomicU64,
    read_layout: AtomicU64,
    read_estimates: AtomicU64,
    read_rclass: AtomicU64,
    read_sclass: AtomicU64,
    read_chars: AtomicU64,
    read_rng: AtomicU64,
    read_cwd: AtomicU64,

    wrote_membership: AtomicU64,
    wrote_order: AtomicU64,
    wrote_layout: AtomicU64,
    wrote_estimates: AtomicU64,
    wrote_rclass: AtomicU64,
    wrote_sclass: AtomicU64,
    wrote_chars: AtomicU64,
    wrote_rng: AtomicU64,
    wrote_cwd: AtomicU64,

    taint: AtomicU64,
    order_sensitive: AtomicU64,
}

fn flag(a: &AtomicU64) {
    a.store(1, Ordering::Relaxed);
}

fn is_set(a: &AtomicU64) -> bool {
    a.load(Ordering::Relaxed) != 0
}

/// The versions in force when the block started. Reads resolve against this.
#[derive(Clone, Debug)]
struct Entry {
    row_membership: u64,
    row_order: u64,
    var_layout: u64,
    gens: Vec<(VarId, u32)>,
    state: Box<StateFingerprint>,
}

impl FootprintBuilder {
    /// Start recording. `slots` is the session's next `VarId`, which sizes the
    /// lock-free part of the bitsets.
    #[must_use]
    pub fn begin(state: &StateFingerprint, ds: &DatasetFingerprint, slots: u32) -> Self {
        Self {
            frame: ds.frame,
            reads: SlotBits::with_slots(slots as usize + 1),
            writes: SlotBits::with_slots(slots as usize + 1),
            creates: SlotBits::with_slots(slots as usize + 1),
            named: Mutex::new(Named::default()),
            flags: Flags::default(),
            entry: Entry {
                row_membership: ds.row_membership,
                row_order: ds.row_order,
                var_layout: ds.var_layout,
                gens: ds.vars.iter().map(|v| (v.var, v.gen)).collect(),
                state: Box::new(state.clone()),
            },
        }
    }

    /// The frame being recorded.
    #[must_use]
    pub fn frame(&self) -> FrameId {
        self.frame
    }

    // -----------------------------------------------------------------------
    // Reads. All `&self`: safe to call from a rayon worker.
    // -----------------------------------------------------------------------

    /// Record a column read. No-op if this execution has already written or
    /// created `var` — see the module header.
    ///
    /// Also flags the row-membership dependency, because `03` §4.2 defines the
    /// effective version as `eff(v, S) = (var, gen, row_membership)`: reading a
    /// column means reading it *over the rows that currently exist*, and
    /// `drop if` must restale everything that read anything. Leaving the two
    /// apart would let one counter bump ("O(1), not O(#vars)") invalidate
    /// nothing at all.
    pub fn note_read(&self, var: VarId) {
        if self.writes.contains(var) || self.creates.contains(var) {
            return;
        }
        self.reads.set(var);
        flag(&self.flags.read_membership);
    }

    /// Record that the answer depends on which rows exist.
    pub fn note_read_membership(&self) {
        flag(&self.flags.read_membership);
    }

    /// Record that the answer depends on the order of rows. Called only for a
    /// structurally order-sensitive block (`03` §4.8).
    pub fn note_read_order(&self) {
        flag(&self.flags.read_order);
    }

    /// Record that the answer depends on variable metadata.
    pub fn note_read_layout(&self) {
        flag(&self.flags.read_layout);
    }

    /// Record a read of a named namespace entry.
    pub fn note_read_named(&self, ns: Ns, name: &str) {
        let mut n = self.named.lock().expect("footprint side channel");
        let v = match ns {
            Ns::Global | Ns::Local => &mut n.macro_reads,
            Ns::Scalar => &mut n.scalar_reads,
            Ns::Matrix => &mut n.matrix_reads,
            Ns::Program => &mut n.program_reads,
            Ns::Setting => &mut n.setting_reads,
            other => {
                drop(n);
                self.note_read_ns(other);
                return;
            }
        };
        push_unique(v, &qualify(ns, name));
    }

    /// Record a read of a namespace with no names of its own.
    pub fn note_read_ns(&self, ns: Ns) {
        match ns {
            Ns::Estimates => flag(&self.flags.read_estimates),
            Ns::RClass => flag(&self.flags.read_rclass),
            Ns::SClass => flag(&self.flags.read_sclass),
            Ns::Chars => flag(&self.flags.read_chars),
            Ns::Rng => flag(&self.flags.read_rng),
            Ns::Cwd => flag(&self.flags.read_cwd),
            _ => {}
        }
    }

    /// Record an external input and the stamp it was read at.
    pub fn note_read_file(&self, path: PathKey, stamp: FileStamp) {
        let mut n = self.named.lock().expect("footprint side channel");
        if !n.file_reads.iter().any(|(p, _)| *p == path) {
            n.file_reads.push((path, stamp));
        }
    }

    // -----------------------------------------------------------------------
    // Writes.
    // -----------------------------------------------------------------------

    /// Record a write to an existing column. Idempotent — this is what makes
    /// `replace x = x+1` over 10 M rows one recorded write and, at commit, one
    /// version bump.
    pub fn note_write(&self, var: VarId) {
        self.writes.set(var);
    }

    /// Record a column creation.
    pub fn note_create(&self, var: VarId) {
        self.creates.set(var);
    }

    /// Record a column drop.
    pub fn note_drop(&self, var: VarId, name: &str) {
        let mut n = self.named.lock().expect("footprint side channel");
        n.dropped.push((var, name.into()));
    }

    /// Record a rename. Identity and `gen` survive it (`03` §4.3).
    pub fn note_rename(&self, var: VarId, from: &str, to: &str) {
        let mut n = self.named.lock().expect("footprint side channel");
        n.renamed.push((var, from.into(), to.into()));
    }

    /// Record that the set of rows changed.
    pub fn note_write_membership(&self) {
        flag(&self.flags.wrote_membership);
    }

    /// Record that the order of rows changed.
    pub fn note_write_order(&self) {
        flag(&self.flags.wrote_order);
    }

    /// Record that column layout or variable metadata changed.
    pub fn note_write_layout(&self) {
        flag(&self.flags.wrote_layout);
    }

    /// Record a write to a named namespace entry.
    pub fn note_write_named(&self, ns: Ns, name: &str) {
        let mut n = self.named.lock().expect("footprint side channel");
        let v = match ns {
            Ns::Global | Ns::Local => &mut n.macro_writes,
            Ns::Scalar => &mut n.scalar_writes,
            Ns::Matrix => &mut n.matrix_writes,
            Ns::Program => &mut n.program_writes,
            Ns::Setting => &mut n.setting_writes,
            other => {
                drop(n);
                self.note_write_ns(other);
                return;
            }
        };
        push_unique(v, &qualify(ns, name));
    }

    /// Record a write to a namespace with no names of its own.
    pub fn note_write_ns(&self, ns: Ns) {
        match ns {
            Ns::Estimates => flag(&self.flags.wrote_estimates),
            Ns::RClass => flag(&self.flags.wrote_rclass),
            Ns::SClass => flag(&self.flags.wrote_sclass),
            Ns::Chars => flag(&self.flags.wrote_chars),
            Ns::Rng => flag(&self.flags.wrote_rng),
            Ns::Cwd => flag(&self.flags.wrote_cwd),
            _ => {}
        }
    }

    /// Record an external output.
    pub fn note_write_file(&self, path: PathKey) {
        let mut n = self.named.lock().expect("footprint side channel");
        if !n.file_writes.contains(&path) {
            n.file_writes.push(path);
        }
    }

    /// Absorb the interpreter's raw [`crate::ctx::AccessLog`].
    ///
    /// `ExecCtx` records in the interpreter's own coordinates — `VarIdx`,
    /// owned names, one `Vec::insert` on a cold path — and W06a's note on that
    /// type says this module resolves it into `VarId`s and versions at command
    /// end. This is that resolution, and it is the only place the two
    /// coordinate systems meet.
    ///
    /// `VarIdx` is a *position*, so it must be resolved against the frame the
    /// command ran on and never stored: `order` renumbers every one of them and
    /// touches no data (`03` §4.3). A position that no longer resolves is
    /// dropped rather than guessed at.
    pub fn absorb(&self, log: &crate::ctx::AccessLog, frame: &stratum_data::Frame) {
        let id = |idx: stratum_proto::VarIdx| frame.var(idx).map(|v| v.id);
        // Writes and creates first: `note_read` consults them, and the log has
        // already applied the same exclusion in its own coordinates.
        for v in log.vars_written.iter().filter_map(|i| id(*i)) {
            self.note_write(v);
        }
        for v in log.vars_created.iter().filter_map(|i| id(*i)) {
            self.note_create(v);
        }
        for v in log.vars_read.iter().filter_map(|i| id(*i)) {
            self.note_read(v);
        }
        for (ns, name) in &log.named_reads {
            self.note_read_named(from_ctx_ns(*ns, name), name);
        }
        for (ns, name) in &log.named_writes {
            self.note_write_named(from_ctx_ns(*ns, name), name);
        }
        if log.read_row_membership {
            self.note_read_membership();
        }
        if log.read_row_order {
            self.note_read_order();
        }
        if log.read_var_layout {
            self.note_read_layout();
        }
        if log.read_ambient {
            // `03` §6.3's residual escape hatch: recorded, never denied. C8
            // demotes the block to `CurrentUnverifiable` rather than showing a
            // ✓ we cannot stand behind.
            self.note_taint(Taint::ENVIRONMENT);
        }
    }

    /// Union in a taint bit. Never subtractive.
    pub fn note_taint(&self, t: Taint) {
        self.flags
            .taint
            .fetch_or(u64::from(t.bits()), Ordering::Relaxed);
    }

    /// Declare the block structurally order-sensitive (`03` §4.8). Any taint at
    /// all also forces it, at [`Self::finish`].
    pub fn set_order_sensitive(&self, yes: bool) {
        if yes {
            flag(&self.flags.order_sensitive);
        }
    }

    /// The taint accumulated so far.
    #[must_use]
    pub fn taint(&self) -> Taint {
        Taint::from_bits_truncate(self.flags.taint.load(Ordering::Relaxed) as u16)
    }

    /// Columns this execution has written, in ascending order.
    #[must_use]
    pub fn written(&self) -> Vec<VarId> {
        self.writes.iter()
    }

    /// True when no column was written or created.
    #[must_use]
    pub fn wrote_nothing(&self) -> bool {
        self.writes.is_empty() && self.creates.is_empty()
    }

    /// The write side of the record, without closing it.
    ///
    /// The commit path needs this *before* [`Self::finish`] can run: applying
    /// the `03` §4.3 table is what produces the exit fingerprint, and `finish`
    /// takes the exit fingerprint. Reading the write side twice is cheap — it is
    /// a handful of set bits and a short side-channel list.
    #[must_use]
    pub fn writes(&self) -> WriteFootprint {
        let named = self.named.lock().expect("footprint side channel");
        let strip_all = |v: &[Box<str>]| -> Vec<Box<str>> {
            let mut v: Vec<Box<str>> = v.iter().map(|n| strip(n).into()).collect();
            v.sort();
            v.dedup();
            v
        };
        WriteFootprint {
            frame: self.frame,
            vars_written: self.writes.iter(),
            vars_created: self.creates.iter(),
            vars_dropped: named.dropped.clone(),
            renamed: named.renamed.clone(),
            changed_membership: is_set(&self.flags.wrote_membership),
            changed_order: is_set(&self.flags.wrote_order),
            changed_layout: is_set(&self.flags.wrote_layout),
            macros_set: strip_all(&named.macro_writes),
            scalars_set: strip_all(&named.scalar_writes),
            matrices_set: strip_all(&named.matrix_writes),
            programs_set: strip_all(&named.program_writes),
            settings_set: strip_all(&named.setting_writes),
            set_estimates: is_set(&self.flags.wrote_estimates),
            set_rclass: is_set(&self.flags.wrote_rclass),
            set_sclass: is_set(&self.flags.wrote_sclass),
            set_chars: is_set(&self.flags.wrote_chars),
            files_written: named.file_writes.clone(),
            changed_cwd: is_set(&self.flags.wrote_cwd),
            changed_rng: is_set(&self.flags.wrote_rng),
            taint: self.taint(),
        }
    }

    /// Close the record.
    ///
    /// `exit` is the dataset fingerprint **after** commit. A column that was
    /// both read and written is recorded at its exit version: recording the
    /// entry version would make `replace x = x+1` compare the version it read
    /// against the version it had just written, and report itself stale the
    /// instant it finished.
    #[must_use]
    pub fn finish(self, exit: &DatasetFingerprint) -> (DepFootprint, WriteFootprint) {
        let writes = self.writes();
        let taint = self.taint();
        let order_sensitive = is_set(&self.flags.order_sensitive) || !taint.is_empty();

        let read_ids = self.reads.iter();
        let entry_gen: rustc_hash::FxHashMap<VarId, u32> =
            self.entry.gens.iter().copied().collect();

        let mut vars = Vec::with_capacity(read_ids.len());
        for var in &read_ids {
            // Exit version for a read-modify-write column, entry version
            // otherwise. `origin` comes from the exit record either way — it is
            // provenance, not a comparison key.
            let v = if self.writes.contains(*var) {
                exit.version_of(*var)
            } else {
                entry_gen.get(var).map(|gen| VarVersion {
                    var: *var,
                    gen: *gen,
                    origin: exit
                        .version_of(*var)
                        .map_or(stratum_proto::ExecutionId(0), |e| e.origin),
                })
            };
            if let Some(v) = v {
                vars.push(v);
            }
        }

        let named = self.named.lock().expect("footprint side channel");
        let st = &self.entry.state;
        let resolve = |ns: Ns, names: Vec<Box<str>>| -> Vec<(Box<str>, u64)> {
            let mut v: Vec<(Box<str>, u64)> = names
                .into_iter()
                .map(|n| {
                    let version = st.named(unqualify_ns(ns, &n), strip(&n));
                    (n, version)
                })
                .collect();
            v.sort_by(|a, b| a.0.cmp(&b.0));
            v
        };

        // Same post-commit rule as a read-then-written column, and for the same
        // reason: a `drop if` that also read membership must not report itself
        // stale the instant it finishes.
        let membership = if is_set(&self.flags.wrote_membership) {
            exit.row_membership
        } else {
            self.entry.row_membership
        };
        let deps = DepFootprint {
            frame: self.frame,
            vars,
            row_membership: is_set(&self.flags.read_membership).then_some(membership),
            row_order: (order_sensitive && is_set(&self.flags.read_order)).then(|| {
                if is_set(&self.flags.wrote_order) {
                    exit.row_order
                } else {
                    self.entry.row_order
                }
            }),
            var_layout: is_set(&self.flags.read_layout).then(|| {
                if is_set(&self.flags.wrote_layout) {
                    exit.var_layout
                } else {
                    self.entry.var_layout
                }
            }),
            macros: resolve(Ns::Local, named.macro_reads.clone()),
            scalars: resolve(Ns::Scalar, named.scalar_reads.clone()),
            matrices: resolve(Ns::Matrix, named.matrix_reads.clone()),
            programs: resolve(Ns::Program, named.program_reads.clone()),
            settings: resolve(Ns::Setting, named.setting_reads.clone()),
            estimates: is_set(&self.flags.read_estimates).then_some(st.estimates),
            rclass: is_set(&self.flags.read_rclass).then_some(st.rclass),
            sclass: is_set(&self.flags.read_sclass).then_some(st.sclass),
            chars: is_set(&self.flags.read_chars).then_some(st.chars),
            rng: is_set(&self.flags.read_rng).then_some(st.rng),
            cwd: is_set(&self.flags.read_cwd).then_some(st.cwd),
            files: {
                let mut f = named.file_reads.clone();
                f.sort_by(|a, b| a.0.cmp(&b.0));
                f
            },
            taint,
        };

        (deps, writes)
    }
}

/// Macros live in two scopes that share a `DepKey`, so the recorded name carries
/// its sigil: `` `x' `` is not `$x`.
fn qualify(ns: Ns, name: &str) -> String {
    match ns {
        Ns::Global => format!("${name}"),
        Ns::Local => format!("`{name}"),
        _ => name.to_owned(),
    }
}

fn strip(name: &str) -> &str {
    name.strip_prefix('$')
        .or_else(|| name.strip_prefix('`'))
        .unwrap_or(name)
}

/// `ctx::Ns` is the interpreter's five-way split; [`Ns`] is the fingerprint's
/// fifteen-way one. Macros are the only lossy direction — `ctx::Ns::Macro`
/// covers both scopes — so the sigil the interpreter kept on the name decides.
fn from_ctx_ns(ns: crate::ctx::Ns, name: &str) -> Ns {
    match ns {
        crate::ctx::Ns::Macro => {
            if name.starts_with('$') {
                Ns::Global
            } else {
                Ns::Local
            }
        }
        crate::ctx::Ns::Scalar => Ns::Scalar,
        crate::ctx::Ns::Matrix => Ns::Matrix,
        crate::ctx::Ns::Program => Ns::Program,
        crate::ctx::Ns::Setting => Ns::Setting,
    }
}

fn unqualify_ns(ns: Ns, name: &str) -> Ns {
    match ns {
        Ns::Local | Ns::Global => {
            if name.starts_with('$') {
                Ns::Global
            } else {
                Ns::Local
            }
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::dataset::Carry;
    use stratum_proto::{ExecutionId, SessionEpoch};

    const E: ExecutionId = ExecutionId(7);

    fn setup() -> (StateFingerprint, DatasetFingerprint) {
        let mut ds = DatasetFingerprint::empty(FrameId(0), Carry::default());
        for (i, n) in [(1u32, "x"), (2, "y"), (3, "z")] {
            ds.create(VarId(i), n, E);
        }
        ds.change_membership(74);
        let mut st = StateFingerprint::fresh(SessionEpoch(1), FrameId(0));
        st.set_frame(FrameId(0), ds.clone());
        (st, ds)
    }

    #[test]
    fn a_read_after_this_blocks_own_write_is_not_a_dependency() {
        // `gen z = x + 1` then `replace z = z*2` depends on x, not on itself.
        let (st, ds) = setup();
        let b = FootprintBuilder::begin(&st, &ds, 8);
        b.note_read(VarId(1));
        b.note_create(VarId(4));
        b.note_read(VarId(4)); // the self-read
        let (deps, writes) = b.finish(&ds);
        assert_eq!(
            deps.vars.iter().map(|v| v.var).collect::<Vec<_>>(),
            vec![VarId(1)]
        );
        assert_eq!(writes.vars_created, vec![VarId(4)]);
    }

    #[test]
    fn a_read_before_this_blocks_own_write_stays_a_dependency_at_the_exit_version() {
        // `replace x = x + 1` reads x before writing it. Dropping the dependency
        // would leave the block ✓ Current when an upstream `gen x` is edited;
        // recording the ENTRY version would make it stale the instant it ran.
        let (st, mut ds) = setup();
        let b = FootprintBuilder::begin(&st, &ds, 8);
        b.note_read(VarId(1));
        b.note_write(VarId(1));
        let entry_gen = ds.version_of(VarId(1)).unwrap().gen;
        ds.bump_value(VarId(1), E);
        let (deps, writes) = b.finish(&ds);
        assert_eq!(deps.vars.len(), 1);
        assert_eq!(deps.vars[0].var, VarId(1));
        assert_eq!(deps.vars[0].gen, entry_gen + 1, "exit version, not entry");
        assert_eq!(writes.vars_written, vec![VarId(1)]);
    }

    #[test]
    fn recording_a_column_twice_records_it_once() {
        let (st, ds) = setup();
        let b = FootprintBuilder::begin(&st, &ds, 8);
        for _ in 0..10_000 {
            b.note_write(VarId(1));
            b.note_read(VarId(2));
        }
        let (deps, writes) = b.finish(&ds);
        assert_eq!(writes.vars_written, vec![VarId(1)]);
        assert_eq!(deps.vars.len(), 1);
    }

    #[test]
    fn a_column_id_above_the_preallocated_bitset_is_still_recorded() {
        // A dependency dropped for being out of range is a soundness bug.
        let (st, ds) = setup();
        let b = FootprintBuilder::begin(&st, &ds, 4);
        b.note_write(VarId(9_000));
        b.note_write(VarId(9_000));
        assert_eq!(b.written(), vec![VarId(9_000)]);
    }

    #[test]
    fn any_taint_forces_order_sensitivity() {
        let (st, ds) = setup();
        let b = FootprintBuilder::begin(&st, &ds, 8);
        b.note_read_order();
        b.set_order_sensitive(false);
        assert!(b.finish(&ds).0.row_order.is_none());

        let b = FootprintBuilder::begin(&st, &ds, 8);
        b.note_read_order();
        b.set_order_sensitive(false);
        b.note_taint(Taint::UNKNOWN_COMMAND);
        let (deps, _) = b.finish(&ds);
        assert!(
            deps.row_order.is_some(),
            "`03` §4.8: any taint at all ⇒ true"
        );
        assert!(deps.taint.contains(Taint::UNKNOWN_COMMAND));
    }

    #[test]
    fn a_local_and_a_global_of_the_same_name_are_two_dependencies() {
        let (st, ds) = setup();
        let b = FootprintBuilder::begin(&st, &ds, 8);
        b.note_read_named(Ns::Local, "path");
        b.note_read_named(Ns::Global, "path");
        let (deps, _) = b.finish(&ds);
        assert_eq!(deps.macros.len(), 2);
    }
}
