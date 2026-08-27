//! `EffectSet`, `Atomicity` and the `EffectTable` trait — design 03 §5.1–5.2,
//! CONTRACTS §13.

use smallvec::SmallVec;
use stratum_parse::CommandAst;
use stratum_proto::{Confidence, Taint, Tri};

use crate::ctx::StaticCtx;
use crate::varset::{FileSet, Name, NameSet, VarSet};

/// What a command does to the frame it runs in.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub enum FrameEffect {
    /// No frame-level effect.
    #[default]
    None,
    /// Modifies the current frame's contents.
    Modify,
    /// Replaces the current frame wholesale (`use`, `clear`).
    ReplaceCurrent,
    /// Creates a named frame.
    Create(Name),
    /// Switches the current frame.
    SwitchTo(Name),
    /// Cannot be determined statically.
    Unknown,
}

/// Read/write effect on a whole namespace that has no names of its own —
/// `e()` estimates and `r()` stored results.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub enum RwEffect {
    /// Untouched.
    #[default]
    None,
    /// Read only.
    Read,
    /// Overwritten.
    Write,
    /// Both.
    ReadWrite,
    /// Cannot be determined statically.
    Unknown,
}

/// What a command does to the random-number stream.
///
/// `Seeds { literal: false }` — a seed built from a macro or from the clock — is
/// what makes a run non-reproducible, and is why this is not a bool.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub enum RngEffect {
    /// Does not touch the stream.
    #[default]
    None,
    /// Advances the stream.
    Consumes,
    /// Sets the seed.
    Seeds {
        /// The seed is a literal in the source, so the run is reproducible.
        literal: bool,
    },
    /// Cannot be determined statically.
    Unknown,
}

/// What a command does to the working directory.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub enum CwdEffect {
    /// Nothing.
    #[default]
    None,
    /// `cd` — changes it, so every later relative path resolves differently.
    Changes,
    /// Resolves a relative path against it.
    Reads,
}

/// Whether a command can be rolled back — ARCHITECTURE §7.6.
///
/// > **INV-2.** A command with `Atomicity::Rollbackable` either completes or
/// > leaves dataset and session state exactly as at entry.
///
/// `External` does NOT mean "cannot be rolled back": engine state still rolls
/// back. It means the effect OUTSIDE the engine — a written `.dta`, an exported
/// graph, a shell command — does not, so the execution records
/// `rolled_back: false` and invalidates the file stamp for every written path.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub enum Atomicity {
    /// All effects are engine state and the undo journal restores them.
    #[default]
    Rollbackable,
    /// Has an effect outside the engine.
    External,
}

/// The static over-approximation of one command's or one block's effects.
///
/// Every field defaults to "no effect", so building one up is additive and a
/// forgotten field is a *narrow* answer — which is why [`EffectSet::unknown_all`]
/// exists and why the extractor reaches for it on every uncertainty rather than
/// leaving fields at their defaults.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct EffectSet {
    /// Variables read.
    pub reads: VarSet,
    /// Existing variables written.
    pub writes: VarSet,
    /// Variables created.
    pub creates: VarSet,
    /// Variables dropped.
    pub drops: VarSet,
    /// `(from, to)` renames. Identity survives a rename, so this is not
    /// `drops` + `creates`.
    pub renames: SmallVec<[(Name, Name); 2]>,

    /// Effect on the frame.
    pub frame: FrameEffect,
    /// Changes which observations exist.
    pub row_membership: Tri,
    /// Changes the order of observations.
    pub row_order: Tri,
    /// Reads variable metadata (labels, formats) — so it depends on
    /// `var_layout`. `tabulate` and `summarize` do; `regress` does not.
    pub reads_metadata: bool,
    /// The answer depends on the current sort order (`_n`, `in`, `by`).
    pub order_sensitive: bool,

    /// Macros read.
    pub macro_reads: NameSet,
    /// Macros written.
    pub macro_writes: NameSet,
    /// Scalars read.
    pub scalar_reads: NameSet,
    /// Scalars written.
    pub scalar_writes: NameSet,
    /// Matrices read.
    pub matrix_reads: NameSet,
    /// Matrices written.
    pub matrix_writes: NameSet,
    /// Programs read (called).
    pub program_reads: NameSet,
    /// Programs written (defined).
    pub program_writes: NameSet,

    /// Effect on `e()`.
    pub estimates: RwEffect,
    /// Effect on `r()`.
    pub rclass: RwEffect,
    /// Effect on the random-number stream.
    pub rng: RngEffect,
    /// Settings read, by the name `DepKey::Setting` uses.
    pub settings_read: SmallVec<[Name; 4]>,
    /// Settings written.
    pub settings_write: SmallVec<[Name; 4]>,
    /// Effect on the working directory.
    pub cwd: CwdEffect,
    /// Files read.
    pub file_reads: FileSet,
    /// Files written.
    pub file_writes: FileSet,
    /// Rollback behaviour.
    pub atomicity: Atomicity,
    /// Why this analysis is weaker than exact.
    pub taint: Taint,
    /// `Exact` only when every narrowing was justified by a literal in the
    /// source.
    pub confidence: Confidence,
}

/// Hand-written because `stratum_proto::Tri` has no `Default` — and it should
/// not have one: `Unknown` and `No` are both plausible defaults for a tri-state
/// and picking the wrong one silently is exactly the class of bug this
/// subsystem cannot afford. Here `No` is correct, because an `EffectSet` is
/// built up additively from nothing.
impl Default for EffectSet {
    fn default() -> Self {
        Self {
            reads: VarSet::new(),
            writes: VarSet::new(),
            creates: VarSet::new(),
            drops: VarSet::new(),
            renames: SmallVec::new(),
            frame: FrameEffect::None,
            row_membership: Tri::No,
            row_order: Tri::No,
            reads_metadata: false,
            order_sensitive: false,
            macro_reads: NameSet::new(),
            macro_writes: NameSet::new(),
            scalar_reads: NameSet::new(),
            scalar_writes: NameSet::new(),
            matrix_reads: NameSet::new(),
            matrix_writes: NameSet::new(),
            program_reads: NameSet::new(),
            program_writes: NameSet::new(),
            estimates: RwEffect::None,
            rclass: RwEffect::None,
            rng: RngEffect::None,
            settings_read: SmallVec::new(),
            settings_write: SmallVec::new(),
            cwd: CwdEffect::None,
            file_reads: FileSet::new(),
            file_writes: FileSet::new(),
            atomicity: Atomicity::Rollbackable,
            taint: Taint::empty(),
            confidence: Confidence::Exact,
        }
    }
}

impl EffectSet {
    /// The empty effect set — reads nothing, writes nothing.
    pub fn new() -> Self {
        Self::default()
    }

    /// The maximally conservative answer, from design 03 §5.3 rule 7.
    ///
    /// Reached whenever the command name is not in the table, the command name
    /// is itself macro-derived, or the command escapes the engine (`shell`,
    /// `python`, `plugin call`, `ssc`, `net`). There is no path in a correct
    /// extractor that narrows a set on a guess, so this is the floor every
    /// uncertainty falls to.
    pub fn unknown_all() -> Self {
        Self {
            reads: VarSet::unknown(),
            writes: VarSet::unknown(),
            creates: VarSet::unknown(),
            drops: VarSet::unknown(),
            renames: SmallVec::new(),
            frame: FrameEffect::Unknown,
            row_membership: Tri::Unknown,
            row_order: Tri::Unknown,
            reads_metadata: true,
            order_sensitive: true,
            macro_reads: NameSet::unknown(),
            macro_writes: NameSet::unknown(),
            scalar_reads: NameSet::unknown(),
            scalar_writes: NameSet::unknown(),
            matrix_reads: NameSet::unknown(),
            matrix_writes: NameSet::unknown(),
            program_reads: NameSet::unknown(),
            program_writes: NameSet::unknown(),
            estimates: RwEffect::Unknown,
            rclass: RwEffect::Unknown,
            rng: RngEffect::Unknown,
            settings_read: SmallVec::new(),
            settings_write: SmallVec::new(),
            cwd: CwdEffect::Reads,
            file_reads: FileSet::unknown(),
            file_writes: FileSet::unknown(),
            atomicity: Atomicity::External,
            taint: Taint::UNKNOWN_COMMAND,
            confidence: Confidence::Speculative,
        }
    }

    /// Union in document order — the driver of design 03 §5.3.
    ///
    /// Both branches of an `if`/`else` and the body of a `while` are unioned in,
    /// because every write here is a MAY-write and that is exactly what
    /// staleness needs.
    pub fn union(&mut self, other: &EffectSet) {
        self.reads.union(&other.reads);
        self.writes.union(&other.writes);
        self.creates.union(&other.creates);
        self.drops.union(&other.drops);
        self.renames.extend(other.renames.iter().cloned());

        self.frame = join_frame(&self.frame, &other.frame);
        self.row_membership = join_tri(self.row_membership, other.row_membership);
        self.row_order = join_tri(self.row_order, other.row_order);
        self.reads_metadata |= other.reads_metadata;
        self.order_sensitive |= other.order_sensitive;

        self.macro_reads.union(&other.macro_reads);
        self.macro_writes.union(&other.macro_writes);
        self.scalar_reads.union(&other.scalar_reads);
        self.scalar_writes.union(&other.scalar_writes);
        self.matrix_reads.union(&other.matrix_reads);
        self.matrix_writes.union(&other.matrix_writes);
        self.program_reads.union(&other.program_reads);
        self.program_writes.union(&other.program_writes);

        self.estimates = join_rw(self.estimates, other.estimates);
        self.rclass = join_rw(self.rclass, other.rclass);
        self.rng = join_rng(self.rng, other.rng);
        for s in &other.settings_read {
            if !self.settings_read.contains(s) {
                self.settings_read.push(s.clone());
            }
        }
        for s in &other.settings_write {
            if !self.settings_write.contains(s) {
                self.settings_write.push(s.clone());
            }
        }
        self.cwd = join_cwd(self.cwd, other.cwd);
        self.file_reads.union(&other.file_reads);
        self.file_writes.union(&other.file_writes);
        if other.atomicity == Atomicity::External {
            self.atomicity = Atomicity::External;
        }
        self.taint |= other.taint;
        self.confidence = join_confidence(self.confidence, other.confidence);
    }

    /// True when this set can be proven not to touch anything the other reads.
    /// Answering `true` on an uncertainty would be the soundness bug; every
    /// component check is a may-intersect.
    pub fn writes_nothing_read_by(&self, other: &EffectSet) -> bool {
        !self.writes.may_intersect(&other.reads)
            && !self.creates.may_intersect(&other.reads)
            && !self.drops.may_intersect(&other.reads)
            && !self.macro_writes.may_intersect(&other.macro_reads)
            && !self.scalar_writes.may_intersect(&other.scalar_reads)
            && !self.matrix_writes.may_intersect(&other.matrix_reads)
            && !self.program_writes.may_intersect(&other.program_reads)
            && self.row_membership == Tri::No
            && self.row_order == Tri::No
            && !self.file_writes.unknown
            && self.estimates != RwEffect::Unknown
            && self.rng == RngEffect::None
    }
}

/// The weaker of two confidences.
///
/// Design 03 §5.1 spells this enum `{ Exact, Conservative }`; CONTRACTS §4 — the
/// frozen one, shared with every diagnostic in the product — spells it
/// `{ Exact, Probable, Speculative }`. `Conservative` maps onto `Speculative`:
/// both mean "this answer was widened because something could not be resolved".
fn join_confidence(a: Confidence, b: Confidence) -> Confidence {
    let rank = |c: Confidence| match c {
        Confidence::Exact => 0u8,
        Confidence::Probable => 1,
        Confidence::Speculative => 2,
    };
    if rank(a) >= rank(b) {
        a
    } else {
        b
    }
}

fn join_tri(a: Tri, b: Tri) -> Tri {
    match (a, b) {
        (Tri::Unknown, _) | (_, Tri::Unknown) => Tri::Unknown,
        (Tri::Yes, _) | (_, Tri::Yes) => Tri::Yes,
        _ => Tri::No,
    }
}

fn join_rw(a: RwEffect, b: RwEffect) -> RwEffect {
    use RwEffect::{None, Read, ReadWrite, Unknown, Write};
    match (a, b) {
        (Unknown, _) | (_, Unknown) => Unknown,
        (None, x) | (x, None) => x,
        (Read, Read) => Read,
        (Write, Write) => Write,
        _ => ReadWrite,
    }
}

fn join_rng(a: RngEffect, b: RngEffect) -> RngEffect {
    use RngEffect::{Consumes, None, Seeds, Unknown};
    match (a, b) {
        (Unknown, _) | (_, Unknown) => Unknown,
        (None, x) | (x, None) => x,
        // A later literal seed does not undo an earlier non-literal one: the
        // run is still not reproducible from the source alone.
        (Seeds { literal: x }, Seeds { literal: y }) => Seeds { literal: x && y },
        (Seeds { literal }, Consumes) | (Consumes, Seeds { literal }) => Seeds { literal },
        (Consumes, Consumes) => Consumes,
    }
}

fn join_cwd(a: CwdEffect, b: CwdEffect) -> CwdEffect {
    match (a, b) {
        (CwdEffect::Changes, _) | (_, CwdEffect::Changes) => CwdEffect::Changes,
        (CwdEffect::Reads, _) | (_, CwdEffect::Reads) => CwdEffect::Reads,
        _ => CwdEffect::None,
    }
}

fn join_frame(a: &FrameEffect, b: &FrameEffect) -> FrameEffect {
    match (a, b) {
        (FrameEffect::Unknown, _) | (_, FrameEffect::Unknown) => FrameEffect::Unknown,
        (FrameEffect::None, x) | (x, FrameEffect::None) => x.clone(),
        (x, y) if x == y => x.clone(),
        // Two different non-trivial frame effects in one block: the analyser
        // cannot say which frame is current afterwards.
        _ => FrameEffect::Unknown,
    }
}

/// The static effect table — CONTRACTS §13, implemented by `stratum-runtime`
/// (built-ins) and `stratum-stats` (its own rows), consumed by `stratum-exec`
/// and `stratum-intel`.
///
/// There is deliberately **no default implementation** of either method, so a
/// command cannot be added to this engine without someone writing down what it
/// does.
pub trait EffectTable: Send + Sync {
    /// MUST return a conservative OVER-approximation. Returning too small a read
    /// or write set is a soundness bug against INV-1.
    fn effects(&self, cmd: &CommandAst, ctx: &StaticCtx<'_>) -> EffectSet;

    /// Is this command name in the table at all? A `false` here is what sends
    /// the extractor to [`EffectSet::unknown_all`].
    fn is_known_command(&self, name: &str) -> bool;
}
