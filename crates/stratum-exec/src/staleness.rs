//! THE staleness algorithm — design 03 §6, ADR-008.
//!
//! One forward sweep in document order, clauses C0–C9, no fixpoint. C7 (the
//! document-order clause) depends only on blocks with strictly smaller ordinal,
//! so transitivity falls out of the sweep itself: if A is stale and B reads A's
//! writes then B is already resolved to non-Current by the time C is examined.
//!
//! # Why the exact clause is allowed to be exact
//!
//! C6 compares the **recorded dynamic footprint** — what the execution actually
//! read — against current versions. That is sound only because the footprint is
//! complete: `03` §6.3 enumerates every namespace a command implementation can
//! observe, and the command ABI gives implementations no ambient access to the
//! environment, the clock or the filesystem outside `ExecCtx` helpers that
//! record into the footprint. The residual escape hatches (`shell`, `python`,
//! `java`, plugins) set [`Taint::EXTERNAL`], and C8 downgrades those blocks to
//! `CurrentUnverifiable` rather than lying about them.
//!
//! # Where the precision comes from
//!
//! Static effect sets are used **only** where there is no observation — a block
//! that has never run, or whose text changed since it did. A block that ran and
//! has not been edited is compared against what it actually read. Conservatism
//! is the price of ignorance and we stop paying it the moment the block runs
//! (`03` §6.4 Example 4).

use std::sync::Arc;

use rustc_hash::FxHashMap;
use stratum_effects::{EffectSet, FileSet, NameSet, RngEffect, RwEffect, VarSet};
use stratum_proto::{
    BlockId, BlockMap, BlockStatus, BrokenReason, DepKey, DocumentId, ExecStatus, ExecutionId,
    RegionKind, SessionEpoch, StaleReason, Taint, Tri,
};

use crate::ledger::ExecutionLedger;

/// Field separator inside a [`slot_of`] key. Cannot appear in a Stata name, a
/// frame name, or a path we would ever index.
const SEP: char = '\u{1f}';

// ---------------------------------------------------------------------------
// Dependency slots
// ---------------------------------------------------------------------------

/// Write the canonical index key for `key` into `out`, which is cleared first.
///
/// [`DepKey`] is a frozen wire type (CONTRACTS §3) and derives neither `Hash`
/// nor `Ord`, so the engine cannot key a map on it directly. It keys on this
/// text encoding instead, which buys three things at once: an allocation-free
/// `&str` lookup against a `HashMap<String, _>`, a lexicographic order that is
/// identical on every platform and across sessions — so the witness key a stale
/// banner names is deterministic — and a single `DepKey` kept beside the entry
/// for the wire.
pub fn slot_into(key: &DepKey, out: &mut String) {
    out.clear();
    match key {
        DepKey::Var { frame, name } => {
            out.push('v');
            out.push(SEP);
            out.push_str(frame);
            out.push(SEP);
            out.push_str(name);
        }
        DepKey::RowMembership { frame } => {
            out.push('m');
            out.push(SEP);
            out.push_str(frame);
        }
        DepKey::RowOrder { frame } => {
            out.push('o');
            out.push(SEP);
            out.push_str(frame);
        }
        DepKey::VarLayout { frame } => {
            out.push('l');
            out.push(SEP);
            out.push_str(frame);
        }
        DepKey::Macro { name } => {
            out.push('M');
            out.push(SEP);
            out.push_str(name);
        }
        DepKey::Scalar { name } => {
            out.push('s');
            out.push(SEP);
            out.push_str(name);
        }
        DepKey::Matrix { name } => {
            out.push('x');
            out.push(SEP);
            out.push_str(name);
        }
        DepKey::Program { name } => {
            out.push('p');
            out.push(SEP);
            out.push_str(name);
        }
        DepKey::Estimates => out.push('e'),
        DepKey::RClass => out.push('r'),
        DepKey::SClass => out.push('c'),
        DepKey::Rng => out.push('g'),
        DepKey::Setting { name } => {
            out.push('t');
            out.push(SEP);
            out.push_str(name);
        }
        DepKey::Cwd => out.push('w'),
        DepKey::File { path } => {
            out.push('f');
            out.push(SEP);
            out.push_str(path.as_str());
        }
    }
}

/// Allocating form of [`slot_into`].
#[must_use]
pub fn slot_of(key: &DepKey) -> String {
    let mut s = String::new();
    slot_into(key, &mut s);
    s
}

// ---------------------------------------------------------------------------
// Versions — the projection of the runtime's VersionTable that C6 compares
// ---------------------------------------------------------------------------

/// One dependency slot's current version, and the execution that set it.
///
/// `at` is what turns a stale banner from "something changed" into "income was
/// modified at E44", which is the difference between a badge users read and a
/// badge users turn off.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Version {
    /// Monotone within a slot. Equality is the ONLY operation the sweep
    /// performs on it, which is what lets a `FileStamp` or an `RngFingerprint`
    /// be folded into one by the adapter that owns those types.
    pub value: u64,
    /// The execution that last set this slot, when one is known.
    pub at: Option<ExecutionId>,
}

#[derive(Clone, Debug)]
struct Slot {
    key: DepKey,
    version: Version,
}

/// Current version of every live dependency slot, plus the current frame.
///
/// This is the projection of `stratum_runtime::snapshot::VersionTable` the
/// sweep consumes. It doubles as the live-name index: a variable is in scope
/// exactly when its `DepKey::Var` slot is present, which is what makes C5's
/// name-resolution check a lookup rather than a second data structure that can
/// disagree with the first.
#[derive(Clone, Debug)]
pub struct Versions {
    frame: String,
    slots: FxHashMap<String, Slot>,
    scratch: String,
}

impl Versions {
    /// An empty state whose current frame is `frame` (`default` for a fresh
    /// session).
    #[must_use]
    pub fn new(frame: impl Into<String>) -> Self {
        Self {
            frame: frame.into(),
            slots: FxHashMap::default(),
            scratch: String::new(),
        }
    }

    /// The name of the current frame. Static variable reads resolve here.
    #[must_use]
    pub fn frame(&self) -> &str {
        &self.frame
    }

    /// Switch the current frame (`frame change`).
    pub fn set_frame(&mut self, frame: impl Into<String>) {
        self.frame = frame.into();
    }

    /// Set a slot's version outright.
    pub fn set(&mut self, key: DepKey, value: u64, at: Option<ExecutionId>) {
        let mut s = std::mem::take(&mut self.scratch);
        slot_into(&key, &mut s);
        self.slots.insert(
            s.clone(),
            Slot {
                key,
                version: Version { value, at },
            },
        );
        self.scratch = s;
    }

    /// Advance a slot by one — the `gen += 1` of `03` §4.3, and the only
    /// mutation a commit performs on a slot that already exists.
    pub fn bump(&mut self, key: DepKey, at: ExecutionId) {
        let next = self.get(&key).map_or(1, |v| v.value + 1);
        self.set(key, next, Some(at));
    }

    /// Remove a slot — `drop varlist`, and the reason a downstream reader of a
    /// dropped variable goes `Broken` rather than `Stale`.
    pub fn remove(&mut self, key: &DepKey) {
        let mut s = std::mem::take(&mut self.scratch);
        slot_into(key, &mut s);
        self.slots.remove(&s);
        self.scratch = s;
    }

    /// Current version of a slot, or `None` when the slot does not exist —
    /// which C6 treats as "changed", because it is.
    #[must_use]
    pub fn get(&self, key: &DepKey) -> Option<Version> {
        let mut s = String::new();
        slot_into(key, &mut s);
        self.get_slot(&s).map(|slot| slot.version)
    }

    /// The one place the table is indexed by a canonical slot key. Private
    /// because the slot encoding is an internal index key, not a wire type:
    /// callers hand over a [`DepKey`] and the encoding stays here.
    fn get_slot(&self, slot: &str) -> Option<&Slot> {
        self.slots.get(slot)
    }

    /// Does `name` resolve as a variable in the current frame right now?
    #[must_use]
    pub fn resolves_var(&self, name: &str) -> bool {
        self.get(&DepKey::Var {
            frame: self.frame.clone(),
            name: name.to_owned(),
        })
        .is_some()
    }

    /// The live variable name closest to `name`, for the `Broken` quick fix
    /// ("rename reference to `mpg_hwy`").
    ///
    /// Deterministic: candidates are ranked by edit distance and ties broken
    /// lexicographically, so the same rename always suggests the same target.
    /// This is a text computation over names the user already has; nothing here
    /// is generated, which is what `03` §10 means by "deterministic, never
    /// AI-generated".
    #[must_use]
    pub fn nearest_var(&self, name: &str) -> Option<String> {
        let budget = 2 + name.len() / 3;
        let mut best: Option<(usize, &str)> = None;
        for slot in self.slots.values() {
            let DepKey::Var {
                frame,
                name: candidate,
            } = &slot.key
            else {
                continue;
            };
            if frame != &self.frame {
                continue;
            }
            // A rename usually keeps a common stem, so a prefix relationship is
            // worth more than raw distance and is checked first.
            let d = if candidate.starts_with(name) || name.starts_with(candidate.as_str()) {
                0
            } else {
                edit_distance(name, candidate)
            };
            if d > budget {
                continue;
            }
            match best {
                Some((bd, bn)) if (bd, bn) <= (d, candidate.as_str()) => {}
                _ => best = Some((d, candidate)),
            }
        }
        best.map(|(_, n)| n.to_owned())
    }

    /// Number of live slots. Used by tests and by the bench.
    #[must_use]
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    /// True when no slot is live.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }
}

/// Levenshtein distance, capped implicitly by the caller's budget. Names are
/// ≤32 bytes in Stata, so the quadratic table is 1 KiB in the worst case and
/// this runs only on the `Broken` path.
fn edit_distance(a: &str, b: &str) -> usize {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            cur[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

// ---------------------------------------------------------------------------
// Recorded footprints — the projection of the runtime's DepFootprint
// ---------------------------------------------------------------------------

/// One observed dependency: the slot, and the version that was live when the
/// execution read it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Dep {
    /// Which slot.
    pub key: DepKey,
    /// The version observed at read time.
    pub version: u64,
}

/// What an execution ACTUALLY read.
///
/// Self-reads are excluded by the runtime's barrier (`03` §4.7): a block that
/// does `gen z = x + 1` then `replace z = z*2` depends on `x`, not on `z`.
/// Without that rule every multi-statement block would depend on itself and
/// could never be `Current`.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct RecordedReads {
    /// Sorted by [`slot_of`] and deduplicated by [`RecordedReads::normalize`].
    pub deps: Vec<Dep>,
    /// Why this record's INV-1 proof is weaker than exact.
    pub taint: Taint,
}

impl RecordedReads {
    /// Sort and deduplicate. Sorting is by the canonical slot key, so the order
    /// is identical on every platform — the sweep reports the first hit, and a
    /// stale banner that named a different variable on Windows would be a bug
    /// nobody could reproduce.
    pub fn normalize(&mut self) {
        let mut scratch = String::new();
        self.deps.sort_by_cached_key(|d| {
            slot_into(&d.key, &mut scratch);
            scratch.clone()
        });
        self.deps.dedup_by(|a, b| a.key == b.key);
    }

    /// True when this execution provably read nothing at all. Such a block
    /// cannot be affected by anything, including an entirely unknown upstream
    /// write — `display 1` stays `Current` behind a `shell` command.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.deps.is_empty()
    }
}

/// What an execution ACTUALLY wrote.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct RecordedWrites {
    /// Slots whose value changed.
    pub writes: Vec<DepKey>,
    /// Slots that came into existence (`generate`, `local`, a new frame).
    pub creates: Vec<DepKey>,
    /// Slots that ceased to exist (`drop`).
    pub drops: Vec<DepKey>,
    /// `(from, to)` variable renames. Identity survives a rename, so this is
    /// deliberately not `drops` + `creates`.
    pub renames: Vec<(String, String)>,
    /// The execution's writes could not be observed exhaustively — a failed or
    /// interrupted command, or one that escaped the engine.
    pub unknown: bool,
}

impl RecordedWrites {
    /// The maximally conservative write set.
    #[must_use]
    pub fn unknown() -> Self {
        Self {
            unknown: true,
            ..Self::default()
        }
    }

    /// Every slot this execution touched, in a deterministic order.
    pub fn keys(&self) -> impl Iterator<Item = &DepKey> {
        self.writes
            .iter()
            .chain(self.creates.iter())
            .chain(self.drops.iter())
    }

    /// Sort and deduplicate each list by the canonical slot key.
    pub fn normalize(&mut self) {
        let mut scratch = String::new();
        for list in [&mut self.writes, &mut self.creates, &mut self.drops] {
            list.sort_by_cached_key(|k| {
                slot_into(k, &mut scratch);
                scratch.clone()
            });
            list.dedup();
        }
    }

    /// True when nothing was written and the write set is exhaustive.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        !self.unknown
            && self.writes.is_empty()
            && self.creates.is_empty()
            && self.drops.is_empty()
            && self.renames.is_empty()
    }
}

// ---------------------------------------------------------------------------
// The analysed document
// ---------------------------------------------------------------------------

/// A reconciled [`BlockMap`] plus one static [`EffectSet`] per region.
///
/// The map comes from `stratum_session`'s `DocumentModel` (segment then
/// reconcile); the effect sets come from `stratum_runtime`'s extractor driving
/// the `EffectTable`. Neither is computed here — this crate consumes the
/// analysis and owns the sweep over it.
#[derive(Clone, Debug)]
pub struct AnalysedDoc {
    /// Identity assignment for this document.
    pub map: BlockMap,
    /// Parallel to `map.regions`. Shared because the sweep, the planner and the
    /// dep index all read the same set and none of them mutates it.
    pub effects: Vec<Arc<EffectSet>>,
}

impl AnalysedDoc {
    /// Build one, checking the parallel-vector invariant the whole crate leans
    /// on.
    ///
    /// # Panics
    /// If `effects` is not the same length as `map.regions`, or if `map.blocks`
    /// is not either.
    #[must_use]
    pub fn new(map: BlockMap, effects: Vec<Arc<EffectSet>>) -> Self {
        assert_eq!(
            map.regions.len(),
            effects.len(),
            "one EffectSet per region: the sweep indexes both with the same u32"
        );
        assert_eq!(
            map.regions.len(),
            map.blocks.len(),
            "BlockMap.blocks is parallel to BlockMap.regions (CONTRACTS §2)"
        );
        Self { map, effects }
    }

    /// Number of regions, executable or not.
    #[must_use]
    pub fn len(&self) -> usize {
        self.map.regions.len()
    }

    /// True when the document has no regions at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.map.regions.is_empty()
    }

    /// The block id at `idx`.
    #[must_use]
    pub fn block(&self, idx: usize) -> BlockId {
        self.map.blocks[idx]
    }

    /// Is this region a node in the staleness graph at all? `Trivia` regions
    /// carry [`BlockId::NONE`] and are skipped everywhere (A3).
    #[must_use]
    pub fn is_executable(&self, idx: usize) -> bool {
        self.map.blocks[idx].is_real()
            && !matches!(self.map.regions[idx].kind, RegionKind::Trivia { .. })
    }

    /// Index of a block id, if the document still contains it.
    #[must_use]
    pub fn index_of(&self, block: BlockId) -> Option<usize> {
        self.map.blocks.iter().position(|b| *b == block)
    }
}

// ---------------------------------------------------------------------------
// Status map
// ---------------------------------------------------------------------------

/// Statuses for one document, parallel to its [`AnalysedDoc`].
#[derive(Clone, Debug)]
pub struct StatusMap {
    /// Which document.
    pub doc: DocumentId,
    /// The map generation this was computed against; a status batch from an
    /// older generation is dropped rather than applied.
    pub generation: u64,
    statuses: Vec<Option<BlockStatus>>,
    blocks: Vec<BlockId>,
}

impl StatusMap {
    /// An all-`NeverRun` map for a document nothing has run in.
    #[must_use]
    pub fn empty(doc: &AnalysedDoc) -> Self {
        Self {
            doc: doc.map.doc,
            generation: doc.map.generation,
            statuses: (0..doc.len())
                .map(|i| doc.is_executable(i).then_some(BlockStatus::NeverRun))
                .collect(),
            blocks: doc.map.blocks.clone(),
        }
    }

    /// Record a status at a region index.
    pub(crate) fn set(&mut self, idx: usize, status: BlockStatus) {
        self.statuses[idx] = Some(status);
    }

    /// Status at a region index.
    #[must_use]
    pub fn at(&self, idx: usize) -> Option<&BlockStatus> {
        self.statuses.get(idx).and_then(Option::as_ref)
    }

    /// Status of a block id.
    #[must_use]
    pub fn get(&self, block: BlockId) -> Option<&BlockStatus> {
        let idx = self.blocks.iter().position(|b| *b == block)?;
        self.at(idx)
    }

    /// Every executable block's status, in document order.
    pub fn iter(&self) -> impl Iterator<Item = (BlockId, &BlockStatus)> {
        self.blocks
            .iter()
            .zip(self.statuses.iter())
            .filter_map(|(b, s)| s.as_ref().map(|s| (*b, s)))
    }

    /// The wire form of the whole map.
    #[must_use]
    pub fn wire(&self) -> Vec<(BlockId, BlockStatus)> {
        self.iter().map(|(b, s)| (b, s.clone())).collect()
    }

    /// The `StatusChanged` batch that takes `old` to `self`.
    ///
    /// A block whose status did not move is not in the batch: the frontend
    /// repaints exactly what changed, and a no-op sweep costs zero DOM work.
    #[must_use]
    pub fn delta(&self, old: &StatusMap) -> Vec<(BlockId, BlockStatus)> {
        let mut out = Vec::new();
        for (block, status) in self.iter() {
            if old.get(block) != Some(status) {
                out.push((block, status.clone()));
            }
        }
        out
    }
}

// ---------------------------------------------------------------------------
// The sweep
// ---------------------------------------------------------------------------

/// What the queue is doing, for clause C0. Queue state is a fact rather than a
/// judgement, so it wins over every other clause.
#[derive(Clone, Debug, Default)]
pub struct RunState {
    /// The block currently executing, its execution id, and when it started.
    pub running: Option<(BlockId, ExecutionId, u64)>,
    /// Blocks waiting, in queue order.
    pub queued: Vec<BlockId>,
}

impl RunState {
    fn status_of(&self, block: BlockId) -> Option<BlockStatus> {
        if let Some((b, exec, started_ms)) = self.running {
            if b == block {
                return Some(BlockStatus::Running { exec, started_ms });
            }
        }
        self.queued
            .iter()
            .position(|b| *b == block)
            .map(|position| BlockStatus::Queued {
                position: u32::try_from(position).unwrap_or(u32::MAX),
            })
    }
}

/// Everything the sweep reads. All borrows, no clones: a full sweep of a
/// 2 000-block document allocates only its output.
pub struct SweepInput<'a> {
    /// The document being swept.
    pub doc: &'a AnalysedDoc,
    /// Current versions of every dependency slot.
    pub versions: &'a Versions,
    /// The append-only history; `latest_by_block` is the only part that
    /// participates.
    pub ledger: &'a ExecutionLedger,
    /// The session's current epoch, for C2.
    pub epoch: SessionEpoch,
    /// Queue state, for C0.
    pub run: &'a RunState,
}

/// Run the full O(n²) sweep, clauses C0–C9, in document order.
///
/// This is the reference implementation. The engine runs [`crate::DepIndex`]'s
/// incremental path and, under `--verify-staleness`, runs this one beside it
/// and panics on divergence.
#[must_use]
pub fn sweep(input: &SweepInput<'_>) -> StatusMap {
    sweep_counted(input).0
}

/// The sweep, plus `(blocks classified, C7 pair tests)` for the ADR-017
/// counters. [`crate::DepIndex`] calls this so there is exactly one forward
/// sweep in the crate and the verifier compares against the same code the
/// engine runs on document open.
#[must_use]
pub fn sweep_counted(input: &SweepInput<'_>) -> (StatusMap, u32, u64) {
    let mut out = StatusMap::empty(input.doc);
    // Blocks above the cursor that are not Current, with the writes they may
    // still perform. Iterated in ordinal order so the witness a stale banner
    // names is the FIRST offending block — "stale because of line 1".
    let mut pending: Vec<(usize, PendingWrites<'_>)> = Vec::new();
    let (mut classified, mut tests) = (0u32, 0u64);

    for idx in 0..input.doc.len() {
        if !input.doc.is_executable(idx) {
            continue;
        }
        classified += 1;
        tests += pending.len() as u64;
        let status = classify(input, idx, &pending);
        if !is_settled(&status) {
            pending.push((idx, pending_writes(input, idx, &status)));
        }
        out.set(idx, status);
    }
    (out, classified, tests)
}

/// C0–C9 for one block. Split out because the incremental path calls it for a
/// handful of blocks rather than for all of them.
pub(crate) fn classify(
    input: &SweepInput<'_>,
    idx: usize,
    pending: &[(usize, PendingWrites<'_>)],
) -> BlockStatus {
    let doc = input.doc;
    let block = doc.block(idx);
    let region = &doc.map.regions[idx];
    let effects = &doc.effects[idx];

    // C0 — queued or running. A fact, not a judgement.
    if let Some(s) = input.run.status_of(block) {
        return s;
    }

    let latest = input.ledger.latest_for(block);

    // C1 — never run.
    let Some(record) = latest else {
        return BlockStatus::NeverRun;
    };

    // C2 — the session was rebuilt under it.
    if record.record.epoch != input.epoch {
        return BlockStatus::Stale {
            reason: StaleReason::EpochReset,
            since: Some(record.record.exec),
        };
    }

    // C3 — the text changed. `CodeHash` is over the canonical token stream, so
    // a comment, a reindent or a `///` reflow does not fire this (spec §23).
    if record.record.code_hash != region.code_hash {
        return BlockStatus::Stale {
            reason: StaleReason::CodeChanged,
            since: Some(record.record.exec),
        };
    }

    // C4 / C4b — the last run did not succeed.
    match &record.record.status {
        ExecStatus::Failed { rc, .. } => {
            return BlockStatus::Failed {
                exec: record.record.exec,
                rc: *rc,
            }
        }
        ExecStatus::Interrupted { rolled_back, .. } => {
            return BlockStatus::Interrupted {
                exec: record.record.exec,
                rolled_back: *rolled_back,
            }
        }
        _ => {}
    }

    // C5 — a name the code mentions no longer resolves. Distinct from Stale:
    // re-running would ERROR, not merely produce different numbers. This is the
    // clause a `gen`-keyed model and a doc-order model both get wrong on a
    // rename (§6.4 Example 6).
    if let Some(name) = unresolved_read(effects, input.versions) {
        return BlockStatus::Broken {
            reason: BrokenReason::UnresolvedName {
                suggestion: input.versions.nearest_var(&name),
                name,
            },
        };
    }

    // C6 — an input actually changed. Exact: this compares the identity of
    // every value the block consumed.
    for dep in &record.reads.deps {
        let now = input.versions.get(&dep.key);
        if now.map(|v| v.value) != Some(dep.version) {
            return BlockStatus::Stale {
                reason: StaleReason::InputChanged {
                    key: dep.key.clone(),
                    at: now.and_then(|v| v.at),
                },
                since: Some(record.record.exec),
            };
        }
    }

    // C7 — code above me that has not run in its current form, and whose effect
    // on me is therefore hypothetical. Note it fires only for upstream blocks
    // that are NOT Current: a Current upstream block cannot make me stale,
    // because whatever it wrote is already in the state I read, and if it wrote
    // something after I ran then C6 caught it.
    let reads = Reads::Exact(&record.reads);
    for (up_idx, writes) in pending {
        if let Some(witness) = may_intersect(writes, &reads) {
            let up = doc.block(*up_idx);
            return BlockStatus::Stale {
                reason: match witness {
                    Witness::Key(key) => StaleReason::UpstreamPending {
                        block: up,
                        via: key,
                    },
                    Witness::Opaque => StaleReason::UpstreamOpaque { block: up },
                },
                since: Some(record.record.exec),
            };
        }
    }

    // C8 — ran cleanly, but something outside the engine could have moved under
    // it. We do not lie about this; it gets its own glyph.
    if record.record.taint.contains(Taint::EXTERNAL) {
        return BlockStatus::CurrentUnverifiable {
            exec: record.record.exec,
            dataset: record.record.output_dataset,
            duration_us: record.record.duration_us,
            taint: record.record.taint,
        };
    }

    // C9.
    BlockStatus::Current {
        exec: record.record.exec,
        dataset: record.record.output_dataset,
        duration_us: record.record.duration_us,
    }
}

/// Does this status let the block out of the C7 scan? `Skipped` counts because
/// the planner only skips blocks it proved unaffected.
pub(crate) fn is_settled(status: &BlockStatus) -> bool {
    matches!(
        status,
        BlockStatus::Current { .. } | BlockStatus::CurrentUnverifiable { .. }
    )
}

/// `Writes_pending(A)` from `03` §6.2.
pub(crate) enum PendingWrites<'a> {
    /// The block has run in its current form; its recorded writes are exact.
    Exact(&'a RecordedWrites),
    /// The text changed, or it never ran: the new text's static writes UNION
    /// what the old text actually wrote. Both, because either could still land.
    Widened {
        /// What the previous execution actually wrote.
        dynamic: Option<&'a RecordedWrites>,
        /// What the current text may write.
        statik: &'a EffectSet,
    },
    /// A failed or interrupted command wrote an unknown amount of what it was
    /// going to write.
    Unknown,
}

/// [`pending_writes`] when the caller has a status map rather than a fresh
/// classification — the planner's case. `None` means "no opinion", which
/// widens to static ∪ dynamic: the conservative answer, never a narrowing one.
pub(crate) fn pending_writes_opt<'a>(
    input: &SweepInput<'a>,
    idx: usize,
    status: Option<&BlockStatus>,
) -> PendingWrites<'a> {
    match status {
        Some(s) => pending_writes(input, idx, s),
        None => PendingWrites::Widened {
            // `Committed::writes` is an `Arc` so the ledger and the control
            // thread's `Committed` message share one footprint; the sweep only
            // ever reads it, so it borrows THROUGH the `Arc` rather than
            // cloning one per candidate. `Option::map` is not a coercion site,
            // hence the explicit deref here and below.
            dynamic: input
                .ledger
                .latest_for(input.doc.block(idx))
                .map(|r| &*r.writes),
            statik: &input.doc.effects[idx],
        },
    }
}

/// `Reads_effective(B)`: the recorded footprint when the block ran in its
/// current form, the static set otherwise. This is the one place the
/// "exact once observed" rule is decided, so the sweep and the planner cannot
/// disagree about it.
pub(crate) fn reads_effective<'a>(input: &SweepInput<'a>, idx: usize) -> Reads<'a> {
    let block = input.doc.block(idx);
    match input.ledger.latest_for(block) {
        Some(c) if c.record.code_hash == input.doc.map.regions[idx].code_hash => {
            Reads::Exact(&c.reads)
        }
        _ => Reads::Static(&input.doc.effects[idx]),
    }
}

pub(crate) fn pending_writes<'a>(
    input: &SweepInput<'a>,
    idx: usize,
    status: &BlockStatus,
) -> PendingWrites<'a> {
    let block = input.doc.block(idx);
    let record = input.ledger.latest_for(block);
    match status {
        BlockStatus::Failed { .. } | BlockStatus::Interrupted { .. } => PendingWrites::Unknown,
        BlockStatus::NeverRun
        | BlockStatus::Queued { .. }
        | BlockStatus::Running { .. }
        | BlockStatus::Broken { .. }
        | BlockStatus::Stale {
            reason: StaleReason::CodeChanged | StaleReason::EpochReset,
            ..
        } => PendingWrites::Widened {
            dynamic: record.map(|r| &*r.writes),
            statik: &input.doc.effects[idx],
        },
        // Stale for any other reason means the text is unchanged and it DID
        // run, so what it wrote last time is what it will write again — except
        // that its inputs moved, which is what makes it pending in the first
        // place.
        BlockStatus::Stale { .. } => {
            record
                .map(|r| PendingWrites::Exact(&r.writes))
                .unwrap_or(PendingWrites::Widened {
                    dynamic: None,
                    statik: &input.doc.effects[idx],
                })
        }
        BlockStatus::Current { .. } | BlockStatus::CurrentUnverifiable { .. } => {
            PendingWrites::Exact(record.map_or(EMPTY_WRITES, |r| &r.writes))
        }
    }
}

static EMPTY_WRITES: &RecordedWrites = &RecordedWrites {
    writes: Vec::new(),
    creates: Vec::new(),
    drops: Vec::new(),
    renames: Vec::new(),
    unknown: false,
};

/// `Reads_effective(B)` from `03` §6.2: dynamic and exact when the block ran in
/// its current form, static and conservative otherwise.
pub(crate) enum Reads<'a> {
    /// What the block actually read.
    Exact(&'a RecordedReads),
    /// What the current text may read.
    Static(&'a EffectSet),
}

/// Which side of the may-intersect fired.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Witness {
    /// A named slot both sides agree on — the UI can say "via `income`".
    Key(DepKey),
    /// One side was unknown in a namespace the other touches. Honest, and it is
    /// why [`StaleReason::UpstreamOpaque`] exists as a separate variant.
    Opaque,
}

/// The `⋈` of `03` §6.2, over every namespace, with `Unknown` on either side
/// meaning `true`.
pub(crate) fn may_intersect(w: &PendingWrites<'_>, r: &Reads<'_>) -> Option<Witness> {
    match (w, r) {
        (PendingWrites::Unknown, Reads::Exact(reads)) => {
            // A block that provably read nothing cannot be affected by anything,
            // including a command that escaped the engine entirely.
            (!reads.is_empty()).then_some(Witness::Opaque)
        }
        (PendingWrites::Unknown, Reads::Static(e)) => {
            (!reads_nothing(e)).then_some(Witness::Opaque)
        }
        (PendingWrites::Exact(writes), Reads::Exact(reads)) => exact_pair(writes, reads),
        (PendingWrites::Exact(writes), Reads::Static(e)) => {
            if writes.unknown && !reads_nothing(e) {
                return Some(Witness::Opaque);
            }
            first_hit(writes.keys(), |k| static_reads_hit(e, k))
        }
        (PendingWrites::Widened { dynamic, statik }, r) => {
            if let Some(d) = dynamic {
                if let Some(hit) = may_intersect(&PendingWrites::Exact(d), r) {
                    return Some(hit);
                }
            }
            match r {
                Reads::Exact(reads) => {
                    if reads.is_empty() {
                        return None;
                    }
                    first_hit(reads.deps.iter().map(|d| &d.key), |k| {
                        static_writes_hit(statik, k)
                    })
                }
                Reads::Static(e) => static_pair(statik, e),
            }
        }
    }
}

fn exact_pair(writes: &RecordedWrites, reads: &RecordedReads) -> Option<Witness> {
    if reads.is_empty() {
        return None;
    }
    if writes.unknown {
        return Some(Witness::Opaque);
    }
    let mut scratch = String::new();
    let mut read_slots: Vec<(String, &DepKey)> = reads
        .deps
        .iter()
        .map(|d| {
            slot_into(&d.key, &mut scratch);
            (scratch.clone(), &d.key)
        })
        .collect();
    read_slots.sort_by(|a, b| a.0.cmp(&b.0));
    let mut hits: Vec<&DepKey> = Vec::new();
    for k in writes.keys() {
        slot_into(k, &mut scratch);
        if read_slots
            .binary_search_by(|probe| probe.0.as_str().cmp(scratch.as_str()))
            .is_ok()
        {
            hits.push(k);
        }
    }
    // A rename does not change data, but a reader of the OLD name is broken and
    // a reader of the NEW name is reading something that just appeared: both are
    // covered by the create/drop entries the runtime records beside the rename.
    let mut best: Option<(String, &DepKey)> = None;
    for k in hits {
        slot_into(k, &mut scratch);
        match &best {
            Some((s, _)) if s.as_str() <= scratch.as_str() => {}
            _ => best = Some((scratch.clone(), k)),
        }
    }
    best.map(|(_, k)| Witness::Key(k.clone()))
}

fn first_hit<'a, I, F>(keys: I, f: F) -> Option<Witness>
where
    I: Iterator<Item = &'a DepKey>,
    F: Fn(&DepKey) -> Option<Witness>,
{
    // Deterministic: collect the hits and take the lexicographically smallest
    // slot, so the witness does not depend on hash iteration order anywhere.
    let mut scratch = String::new();
    let mut best: Option<(String, Witness)> = None;
    for k in keys {
        let Some(hit) = f(k) else { continue };
        if hit == Witness::Opaque {
            return Some(Witness::Opaque);
        }
        slot_into(k, &mut scratch);
        match &best {
            Some((s, _)) if s.as_str() <= scratch.as_str() => {}
            _ => best = Some((scratch.clone(), hit)),
        }
    }
    best.map(|(_, hit)| hit)
}

/// Is this static read set provably empty in every namespace?
///
/// The one narrow exception to "unknown intersects everything": a block that
/// reads nothing cannot observe a difference caused by anything, so `display 1`
/// stays `Current` below a `shell` command instead of being greyed out forever.
fn reads_nothing(e: &EffectSet) -> bool {
    e.reads.is_empty()
        && !e.reads_metadata
        && !e.order_sensitive
        && e.macro_reads.is_empty()
        && e.scalar_reads.is_empty()
        && e.matrix_reads.is_empty()
        && e.program_reads.is_empty()
        && matches!(e.estimates, RwEffect::None | RwEffect::Write)
        && matches!(e.rclass, RwEffect::None | RwEffect::Write)
        && e.rng == RngEffect::None
        && e.cwd == stratum_effects::CwdEffect::None
        && e.file_reads.is_empty()
        && e.settings_read.is_empty()
        && e.taint.is_empty()
}

/// Does the static READ set of `e` touch slot `k`?
fn static_reads_hit(e: &EffectSet, k: &DepKey) -> Option<Witness> {
    match k {
        DepKey::Var { name, .. } => varset_hit(&e.reads, name, k),
        // Every block that reads a variable at all reads the set of rows those
        // values came from. Order is different: it enters only for a block that
        // is structurally order-sensitive (`_n`, `_N`, `by`, `in`, a subscript,
        // an RNG draw, a writer) — which is why a `sort` inserted for
        // readability does not invalidate every model in the file.
        DepKey::RowMembership { .. } => {
            (!e.reads.is_empty() || e.order_sensitive).then_some(Witness::Key(k.clone()))
        }
        DepKey::RowOrder { .. } => e.order_sensitive.then_some(Witness::Key(k.clone())),
        DepKey::VarLayout { .. } => e.reads_metadata.then_some(Witness::Key(k.clone())),
        DepKey::Macro { name } => nameset_hit(&e.macro_reads, name, k),
        DepKey::Scalar { name } => nameset_hit(&e.scalar_reads, name, k),
        DepKey::Matrix { name } => nameset_hit(&e.matrix_reads, name, k),
        DepKey::Program { name } => nameset_hit(&e.program_reads, name, k),
        DepKey::Estimates => rw_read_hit(e.estimates, k),
        DepKey::RClass => rw_read_hit(e.rclass, k),
        // `EffectSet` carries no s() field. `sreturn` is rare enough that the
        // honest conservative answer costs nothing measurable, and guessing
        // "does not read" here would be a soundness bug.
        DepKey::SClass => Some(Witness::Opaque),
        DepKey::Rng => (e.rng != RngEffect::None).then_some(Witness::Key(k.clone())),
        DepKey::Setting { name } => e
            .settings_read
            .iter()
            .any(|s| s.as_ref() == name)
            .then_some(Witness::Key(k.clone())),
        DepKey::Cwd => {
            (e.cwd != stratum_effects::CwdEffect::None).then_some(Witness::Key(k.clone()))
        }
        DepKey::File { path } => fileset_hit(&e.file_reads, path.as_str(), k),
    }
}

/// Does the static WRITE set of `e` touch slot `k`?
fn static_writes_hit(e: &EffectSet, k: &DepKey) -> Option<Witness> {
    match k {
        DepKey::Var { name, .. } => varset_hit(&e.writes, name, k)
            .or_else(|| varset_hit(&e.creates, name, k))
            .or_else(|| varset_hit(&e.drops, name, k))
            .or_else(|| {
                e.renames
                    .iter()
                    .any(|(from, to)| from.as_ref() == name || to.as_ref() == name)
                    .then_some(Witness::Key(k.clone()))
            }),
        DepKey::RowMembership { .. } => tri_hit(e.row_membership, k),
        DepKey::RowOrder { .. } => tri_hit(e.row_order, k),
        DepKey::VarLayout { .. } => (!e.creates.is_empty()
            || !e.drops.is_empty()
            || !e.renames.is_empty()
            || e.frame != stratum_effects::FrameEffect::None)
            .then_some(Witness::Key(k.clone())),
        DepKey::Macro { name } => nameset_hit(&e.macro_writes, name, k),
        DepKey::Scalar { name } => nameset_hit(&e.scalar_writes, name, k),
        DepKey::Matrix { name } => nameset_hit(&e.matrix_writes, name, k),
        DepKey::Program { name } => nameset_hit(&e.program_writes, name, k),
        DepKey::Estimates => rw_write_hit(e.estimates, k),
        DepKey::RClass => rw_write_hit(e.rclass, k),
        DepKey::SClass => Some(Witness::Opaque),
        DepKey::Rng => (e.rng != RngEffect::None).then_some(Witness::Key(k.clone())),
        DepKey::Setting { name } => e
            .settings_write
            .iter()
            .any(|s| s.as_ref() == name)
            .then_some(Witness::Key(k.clone())),
        DepKey::Cwd => {
            (e.cwd == stratum_effects::CwdEffect::Changes).then_some(Witness::Key(k.clone()))
        }
        DepKey::File { path } => fileset_hit(&e.file_writes, path.as_str(), k),
    }
}

/// Static writes against static reads — the both-sides-conservative case, which
/// is what an unrun block above an unrun block looks like.
fn static_pair(w: &EffectSet, r: &EffectSet) -> Option<Witness> {
    // Named agreement first, so the banner can name a variable rather than
    // shrugging.
    if let Some(name) = first_common_name(&w.writes, &r.reads)
        .or_else(|| first_common_name(&w.creates, &r.reads))
        .or_else(|| first_common_name(&w.drops, &r.reads))
    {
        return Some(Witness::Key(DepKey::Var {
            frame: String::new(),
            name,
        }));
    }
    for (ws, rs) in [
        (&w.macro_writes, &r.macro_reads),
        (&w.scalar_writes, &r.scalar_reads),
        (&w.matrix_writes, &r.matrix_reads),
        (&w.program_writes, &r.program_reads),
    ] {
        if !ws.unknown && !rs.unknown {
            if let Some(n) = ws
                .names
                .iter()
                .find(|n| rs.names.binary_search(n).is_ok())
                .cloned()
            {
                return Some(Witness::Key(DepKey::Macro {
                    name: n.into_string(),
                }));
            }
        } else if !ws.is_empty() && !rs.is_empty() {
            return Some(Witness::Opaque);
        }
    }
    if w.writes.may_intersect(&r.reads)
        || w.creates.may_intersect(&r.reads)
        || w.drops.may_intersect(&r.reads)
    {
        return Some(Witness::Opaque);
    }
    if w.row_membership != Tri::No && (!r.reads.is_empty() || r.order_sensitive) {
        return Some(Witness::Opaque);
    }
    if w.row_order != Tri::No && r.order_sensitive {
        return Some(Witness::Opaque);
    }
    if r.reads_metadata && (!w.creates.is_empty() || !w.drops.is_empty() || !w.renames.is_empty()) {
        return Some(Witness::Opaque);
    }
    if writes_rw(w.estimates) && reads_rw(r.estimates) {
        return Some(Witness::Key(DepKey::Estimates));
    }
    if writes_rw(w.rclass) && reads_rw(r.rclass) {
        return Some(Witness::Key(DepKey::RClass));
    }
    if w.rng != RngEffect::None && r.rng != RngEffect::None {
        return Some(Witness::Key(DepKey::Rng));
    }
    if w.cwd == stratum_effects::CwdEffect::Changes && r.cwd != stratum_effects::CwdEffect::None {
        return Some(Witness::Key(DepKey::Cwd));
    }
    for s in &w.settings_write {
        if r.settings_read.contains(s) {
            return Some(Witness::Key(DepKey::Setting {
                name: s.to_string(),
            }));
        }
    }
    if fileset_intersects(&w.file_writes, &r.file_reads) {
        return Some(Witness::Opaque);
    }
    None
}

fn first_common_name(w: &VarSet, r: &VarSet) -> Option<String> {
    if w.unknown || r.unknown {
        return None;
    }
    w.named
        .iter()
        .find(|n| r.contains_name(n))
        .map(|n| n.to_string())
}

fn varset_hit(set: &VarSet, name: &str, k: &DepKey) -> Option<Witness> {
    if set.contains_name(name) || set.patterns.iter().any(|p| p.matches(name)) {
        return Some(Witness::Key(k.clone()));
    }
    set.unknown.then_some(Witness::Opaque)
}

fn nameset_hit(set: &NameSet, name: &str, k: &DepKey) -> Option<Witness> {
    if set.names.iter().any(|n| n.as_ref() == name) {
        return Some(Witness::Key(k.clone()));
    }
    set.unknown.then_some(Witness::Opaque)
}

fn fileset_hit(set: &FileSet, path: &str, k: &DepKey) -> Option<Witness> {
    if set.paths.iter().any(|p| p.as_str() == path) {
        return Some(Witness::Key(k.clone()));
    }
    set.unknown.then_some(Witness::Opaque)
}

fn fileset_intersects(w: &FileSet, r: &FileSet) -> bool {
    if (w.unknown && !r.is_empty()) || (r.unknown && !w.is_empty()) {
        return true;
    }
    w.paths.iter().any(|p| r.paths.contains(p))
}

fn tri_hit(t: Tri, k: &DepKey) -> Option<Witness> {
    match t {
        Tri::No => None,
        Tri::Yes => Some(Witness::Key(k.clone())),
        Tri::Unknown => Some(Witness::Opaque),
    }
}

fn rw_read_hit(rw: RwEffect, k: &DepKey) -> Option<Witness> {
    match rw {
        RwEffect::Read | RwEffect::ReadWrite => Some(Witness::Key(k.clone())),
        RwEffect::Unknown => Some(Witness::Opaque),
        _ => None,
    }
}

fn rw_write_hit(rw: RwEffect, k: &DepKey) -> Option<Witness> {
    match rw {
        RwEffect::Write | RwEffect::ReadWrite => Some(Witness::Key(k.clone())),
        RwEffect::Unknown => Some(Witness::Opaque),
        _ => None,
    }
}

fn reads_rw(rw: RwEffect) -> bool {
    matches!(rw, RwEffect::Read | RwEffect::ReadWrite | RwEffect::Unknown)
}

fn writes_rw(rw: RwEffect) -> bool {
    matches!(
        rw,
        RwEffect::Write | RwEffect::ReadWrite | RwEffect::Unknown
    )
}

/// C5's check: the first statically-named variable read that does not resolve
/// in the current frame.
///
/// Only NAMED reads are checked. A pattern or an unknown read set cannot fail
/// to resolve — we do not know what it names — and reporting `Broken` on a
/// guess would put the scariest glyph in the product on the least evidence.
fn unresolved_read(e: &EffectSet, versions: &Versions) -> Option<String> {
    if versions.is_empty() {
        // Nothing has ever been loaded; every name is unresolved and saying so
        // for every block would be noise, not information. C1/C2 already own
        // that case.
        return None;
    }
    e.reads
        .named
        .iter()
        .find(|n| !versions.resolves_var(n))
        .map(|n| n.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_keys_are_distinct_and_stable() {
        let a = slot_of(&DepKey::Var {
            frame: "default".into(),
            name: "mpg".into(),
        });
        let b = slot_of(&DepKey::Macro { name: "mpg".into() });
        assert_ne!(a, b);
        assert_eq!(
            a,
            slot_of(&DepKey::Var {
                frame: "default".into(),
                name: "mpg".into()
            })
        );
    }

    #[test]
    fn nearest_var_prefers_the_renamed_stem() {
        let mut v = Versions::new("default");
        v.set(
            DepKey::Var {
                frame: "default".into(),
                name: "mpg_hwy".into(),
            },
            0,
            None,
        );
        v.set(
            DepKey::Var {
                frame: "default".into(),
                name: "price".into(),
            },
            0,
            None,
        );
        assert_eq!(v.nearest_var("mpg").as_deref(), Some("mpg_hwy"));
    }

    #[test]
    fn a_dropped_slot_reads_as_changed() {
        let mut v = Versions::new("default");
        let k = DepKey::Var {
            frame: "default".into(),
            name: "x".into(),
        };
        v.set(k.clone(), 3, None);
        assert_eq!(v.get(&k).map(|x| x.value), Some(3));
        v.remove(&k);
        assert_eq!(v.get(&k), None);
    }
}
