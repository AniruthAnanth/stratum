//! Incremental recomputation — design 03 §6.5.
//!
//! The normative rule is O(n²) in blocks. We never run it that way. An inverted
//! index maps each dependency slot to the blocks whose `Reads_effective`
//! touches it, so a commit or an edit re-evaluates a handful of blocks instead
//! of the document.
//!
//! # The honesty mechanism
//!
//! An incremental status model that drifts is worse than none: it would report
//! `Current` for a block whose input moved, which is the one direction INV-1
//! forbids. So the full sweep stays in the tree as the reference implementation
//! and `--verify-staleness` runs both and panics on divergence. It is on by
//! default in debug builds and under the `verify-staleness` feature, which is
//! what the test profile carries.
//!
//! # What is asserted, and what is merely recorded (ADR-017)
//!
//! The gate is a COUNTER: [`SweepStats::classified`] — how many blocks the
//! incremental path had to re-run C0–C9 for. It is machine-independent and it
//! expresses the property the 16 ms budget was standing in for, which is that
//! the work does not scale with document size. Durations are recorded in
//! `benches/staleness.rs`.

use rustc_hash::{FxHashMap, FxHashSet};
use smallvec::SmallVec;
use stratum_proto::{BlockStatus, DepKey};

use crate::staleness::{
    classify, is_settled, pending_writes, slot_into, sweep_counted, AnalysedDoc, PendingWrites,
    RecordedWrites, StatusMap, SweepInput,
};

/// Work the sweep actually did. Counters, never durations.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SweepStats {
    /// Blocks for which C0–C9 was evaluated. THE gate.
    pub classified: u32,
    /// `(block, upstream)` pairs the C7 clause tested.
    pub upstream_tests: u64,
    /// Full sweeps performed.
    pub full: u64,
    /// Incremental updates performed.
    pub incremental: u64,
    /// Times the verifier ran the full sweep beside the incremental one.
    pub verified: u64,
}

/// Blocks whose `Reads_effective` touches a given dependency slot.
#[derive(Debug, Default)]
pub struct DepIndex {
    by_key: FxHashMap<String, SmallVec<[u32; 4]>>,
    /// Blocks whose read set is unknown in some namespace. Always rechecked:
    /// "I might depend on that" is never allowed to resolve to "no".
    opaque_readers: Vec<u32>,
    /// Ordinals of blocks that are not settled, ascending. C7's candidates.
    unsettled: Vec<u32>,
    stats: SweepStats,
    verify: bool,
}

impl DepIndex {
    /// A fresh index. Verification follows the build profile: on in debug and
    /// under the `verify-staleness` feature, off in a plain release engine.
    #[must_use]
    pub fn new() -> Self {
        Self {
            verify: cfg!(debug_assertions) || cfg!(feature = "verify-staleness"),
            ..Self::default()
        }
    }

    /// Force verification on or off — this is what `--verify-staleness` sets.
    pub fn set_verify(&mut self, on: bool) {
        self.verify = on;
    }

    /// Is the O(n²) cross-check running?
    #[must_use]
    pub fn verifying(&self) -> bool {
        self.verify
    }

    /// The counters.
    #[must_use]
    pub fn stats(&self) -> SweepStats {
        self.stats
    }

    /// Reset the counters between measured phases.
    pub fn reset_stats(&mut self) {
        self.stats = SweepStats::default();
    }

    /// Full sweep: document open, epoch change, and the verifier.
    ///
    /// There is exactly ONE forward sweep in this crate
    /// ([`crate::staleness::sweep_counted`]); this method counts it and
    /// rebuilds the index behind it, so the verifier below compares the
    /// incremental path against the same code the engine runs on open.
    pub fn full(&mut self, input: &SweepInput<'_>) -> StatusMap {
        let (out, classified, tests) = sweep_counted(input);
        self.stats.classified += classified;
        self.stats.upstream_tests += tests;
        self.stats.full += 1;
        self.rebuild(input);
        out
    }

    /// Rebuild the inverted index against the current document and ledger.
    fn rebuild(&mut self, input: &SweepInput<'_>) {
        self.by_key.clear();
        self.opaque_readers.clear();
        let mut scratch = String::new();
        for idx in 0..input.doc.len() {
            if !input.doc.is_executable(idx) {
                continue;
            }
            let i = u32::try_from(idx).unwrap_or(u32::MAX);
            let block = input.doc.block(idx);
            let region = &input.doc.map.regions[idx];
            let record = input.ledger.latest_for(block);
            match record {
                // Ran in its current form: the recorded footprint is exact and
                // is what C6 consults, so index exactly those slots.
                Some(c) if c.record.code_hash == region.code_hash => {
                    for dep in &c.reads.deps {
                        slot_into(&dep.key, &mut scratch);
                        self.by_key.entry(scratch.clone()).or_default().push(i);
                    }
                    if c.reads.deps.is_empty() {
                        // Reads nothing: nothing can reach it. Deliberately not
                        // an opaque reader.
                    }
                }
                // Unrun or edited: the static set is what C7 will use, and any
                // namespace it cannot name puts the block in `opaque_readers`.
                _ => {
                    let e = &input.doc.effects[idx];
                    if static_reads_are_opaque(e) {
                        self.opaque_readers.push(i);
                    }
                    for name in &e.reads.named {
                        slot_into(
                            &DepKey::Var {
                                frame: input.versions.frame().to_owned(),
                                name: name.to_string(),
                            },
                            &mut scratch,
                        );
                        self.by_key.entry(scratch.clone()).or_default().push(i);
                    }
                    for name in &e.macro_reads.names {
                        slot_into(
                            &DepKey::Macro {
                                name: name.to_string(),
                            },
                            &mut scratch,
                        );
                        self.by_key.entry(scratch.clone()).or_default().push(i);
                    }
                }
            }
        }
        self.unsettled.clear();
    }

    /// Candidates after a commit whose write footprint is `writes`, made at
    /// ordinal `at`.
    ///
    /// C6 candidates are the readers of every slot written, plus every opaque
    /// reader. C7 candidates are the same set restricted to ordinals below the
    /// committing block — plus, when the committing block's own status moved
    /// across the Current boundary, the blocks that read what it writes.
    #[must_use]
    pub fn candidates_for_commit(&self, writes: &RecordedWrites, at: Option<usize>) -> Vec<u32> {
        let mut out: FxHashSet<u32> = self.opaque_readers.iter().copied().collect();
        if writes.unknown {
            // Nothing can be ruled out; the caller falls back to a full sweep.
            return Vec::new();
        }
        let mut scratch = String::new();
        for key in writes.keys() {
            slot_into(key, &mut scratch);
            if let Some(readers) = self.by_key.get(&scratch) {
                out.extend(readers.iter().copied());
            }
        }
        if let Some(at) = at {
            out.insert(u32::try_from(at).unwrap_or(u32::MAX));
        }
        let mut v: Vec<u32> = out.into_iter().collect();
        v.sort_unstable();
        v
    }

    /// Candidates after a text edit at ordinal `at`.
    ///
    /// Only block `at` changes by C3. C7 must then be re-evaluated for the
    /// blocks below it that could read what it may write — which is the same
    /// index lookup, plus every opaque reader.
    #[must_use]
    pub fn candidates_for_edit(&self, doc: &AnalysedDoc, at: usize, frame: &str) -> Vec<u32> {
        let mut out: FxHashSet<u32> = self
            .opaque_readers
            .iter()
            .copied()
            .filter(|i| *i as usize > at)
            .collect();
        out.insert(u32::try_from(at).unwrap_or(u32::MAX));
        let e = &doc.effects[at];
        if e.writes.unknown || e.creates.unknown || e.drops.unknown {
            return Vec::new(); // cannot be narrowed; caller sweeps fully
        }
        let mut scratch = String::new();
        for name in e
            .writes
            .named
            .iter()
            .chain(e.creates.named.iter())
            .chain(e.drops.named.iter())
        {
            slot_into(
                &DepKey::Var {
                    frame: frame.to_owned(),
                    name: name.to_string(),
                },
                &mut scratch,
            );
            if let Some(readers) = self.by_key.get(&scratch) {
                out.extend(readers.iter().copied().filter(|i| *i as usize > at));
            }
        }
        for name in &e.macro_writes.names {
            slot_into(
                &DepKey::Macro {
                    name: name.to_string(),
                },
                &mut scratch,
            );
            if let Some(readers) = self.by_key.get(&scratch) {
                out.extend(readers.iter().copied().filter(|i| *i as usize > at));
            }
        }
        let mut v: Vec<u32> = out.into_iter().collect();
        v.sort_unstable();
        v
    }

    /// Re-evaluate exactly `candidates`, carrying every other block's status
    /// forward from `prev`.
    ///
    /// An empty candidate list means "could not be narrowed" and falls back to
    /// the full sweep, which is always sound.
    pub fn incremental(
        &mut self,
        input: &SweepInput<'_>,
        prev: &StatusMap,
        candidates: &[u32],
    ) -> StatusMap {
        if candidates.is_empty() || prev.generation != input.doc.map.generation {
            return self.full(input);
        }
        let want: FxHashSet<u32> = candidates.iter().copied().collect();
        let mut out = StatusMap::empty(input.doc);
        let mut pending: Vec<(usize, PendingWrites<'_>)> = Vec::new();
        let mut classified = 0u32;
        let mut tests = 0u64;

        for idx in 0..input.doc.len() {
            if !input.doc.is_executable(idx) {
                continue;
            }
            let i = u32::try_from(idx).unwrap_or(u32::MAX);
            let status = if want.contains(&i) {
                classified += 1;
                tests += pending.len() as u64;
                classify(input, idx, &pending)
            } else {
                // Carried forward. The verifier is what keeps this honest.
                prev.at(idx).cloned().unwrap_or(BlockStatus::NeverRun)
            };
            if !is_settled(&status) {
                pending.push((idx, pending_writes(input, idx, &status)));
            }
            out.set(idx, status);
        }

        self.stats.classified += classified;
        self.stats.upstream_tests += tests;
        self.stats.incremental += 1;

        if self.verify {
            self.stats.verified += 1;
            let full = {
                // The full sweep must not be counted against the gate: it exists
                // only to check the fast path.
                let saved = self.stats;
                let m = self.full(input);
                self.stats = saved;
                m
            };
            verify_against_full(&out, &full, input.doc);
        } else {
            self.rebuild(input);
        }
        out
    }
}

/// Panic with the exact divergence. Loud is the point: an incremental status
/// model that quietly disagrees with the reference is the bug this crate cannot
/// ship.
fn verify_against_full(incremental: &StatusMap, full: &StatusMap, doc: &AnalysedDoc) {
    for idx in 0..doc.len() {
        let (a, b) = (incremental.at(idx), full.at(idx));
        assert!(
            a == b,
            "--verify-staleness: incremental and full sweep disagree at block {} \
             ({}): incremental {a:?}, full {b:?}. The incremental candidate set \
             missed a block whose status moved; that is a soundness bug against \
             INV-1, not a performance one.",
            idx,
            doc.block(idx),
        );
    }
}

/// Is any namespace of this static read set unnameable?
fn static_reads_are_opaque(e: &stratum_effects::EffectSet) -> bool {
    e.reads.unknown
        || !e.reads.patterns.is_empty()
        || e.macro_reads.unknown
        || e.scalar_reads.unknown
        || e.matrix_reads.unknown
        || e.program_reads.unknown
        || e.reads_metadata
        || e.order_sensitive
        || !e.taint.is_empty()
        || e.file_reads.unknown
        || !matches!(
            e.estimates,
            stratum_effects::RwEffect::None | stratum_effects::RwEffect::Write
        )
        || !matches!(
            e.rclass,
            stratum_effects::RwEffect::None | stratum_effects::RwEffect::Write
        )
        || e.rng != stratum_effects::RngEffect::None
        || e.cwd != stratum_effects::CwdEffect::None
}
