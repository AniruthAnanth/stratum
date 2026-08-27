//! Content convergence — `03` §4.4, ADR-008.
//!
//! Provenance versions alone have one bad failure mode: re-running a block that
//! produces byte-identical output still bumps `gen`, and every model below it
//! turns grey. That is the single most common false-stale cascade in practice —
//! the user re-runs a cleaning block to double-check and the whole file greys
//! out — and it is what makes people stop trusting the indicator at all.
//!
//! The fix is to blake3 each dirty column at commit and *not* bump when the
//! digest is unchanged. `DatasetStateId`s are interned by fingerprint, so a
//! converged re-run lands back on the same `D17`; spec §13's "Dataset state:
//! D17" is a recurring identity rather than a counter that only goes up.
//!
//! # What it costs, and the knob
//!
//! blake3 runs at ~1.5 GB/s single-threaded, so a 100 MB column costs ~20 ms at
//! commit. That is affordable for the case it buys and unaffordable for a 40 GB
//! panel, hence [`ConvergencePolicy::Bounded`] — the default — which digests a
//! column only while it is under [`DEFAULT_MAX_DIGEST_BYTES`] and falls back to
//! provenance above it. Falling back over-marks, which INV-1 permits; it never
//! under-marks.
//!
//! The digest itself is `stratum_data::Column::digest` (CONTRACTS §1.1,
//! canonical little-endian encoding so a digest compares across platforms).
//! There is deliberately no second implementation here.

use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use stratum_proto::{ColumnDigest, VarId};

/// `03` §4.4's default per-column ceiling.
pub const DEFAULT_MAX_DIGEST_BYTES: u64 = 256 * 1024 * 1024;

/// `set stalecheck provenance|content|content(<size>)` — `03` §4.4.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(tag = "policy", rename_all = "snake_case")]
pub enum ConvergencePolicy {
    /// `provenance` — always bump. Never wrong, just noisier.
    Off,
    /// `content(<size>)` — digest columns at or below `max_bytes`.
    Bounded {
        /// Per-column ceiling in resident bytes.
        max_bytes: u64,
    },
    /// `content` — digest whatever it costs.
    Always,
}

impl Default for ConvergencePolicy {
    fn default() -> Self {
        Self::Bounded {
            max_bytes: DEFAULT_MAX_DIGEST_BYTES,
        }
    }
}

impl ConvergencePolicy {
    /// Should a column of `bytes` resident bytes be digested at commit?
    #[must_use]
    pub fn admits(&self, bytes: u64) -> bool {
        match *self {
            ConvergencePolicy::Off => false,
            ConvergencePolicy::Always => true,
            ConvergencePolicy::Bounded { max_bytes } => bytes <= max_bytes,
        }
    }

    /// The spelling `set stalecheck` accepts, for `c(stalecheck)` and for the
    /// reproducibility report's settings block.
    #[must_use]
    pub fn as_setting(&self) -> String {
        match *self {
            ConvergencePolicy::Off => "provenance".to_owned(),
            ConvergencePolicy::Always => "content".to_owned(),
            ConvergencePolicy::Bounded { max_bytes } => format!("content({max_bytes})"),
        }
    }
}

/// The digest a column had at its current `gen`.
///
/// Kept beside the fingerprint rather than inside it: the digest is evidence for
/// a bump decision, not part of state identity. Two sessions that reached the
/// same state by different routes must intern to the same `DatasetStateId`, and
/// they will hold different cache contents while doing so.
#[derive(Clone, Debug, Default)]
pub struct DigestCache {
    at_gen: FxHashMap<VarId, (u32, ColumnDigest)>,
}

/// What the cache says about a freshly computed digest.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Convergence {
    /// The column's bytes are unchanged: do not bump `gen`.
    Converged,
    /// The bytes moved, or we have nothing to compare against.
    Diverged,
    /// The policy declined to digest this column; bump on provenance.
    NotChecked,
}

impl DigestCache {
    /// An empty cache.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Compare `digest` against what `var` held at `gen`.
    ///
    /// `gen` is the version the cached digest must belong to. A cache entry from
    /// an older generation is *not* usable: it would compare this command's
    /// output against a state two commands back and could report convergence for
    /// a column that genuinely moved and moved back — which is fine — or, more
    /// dangerously, mask an intervening bump the fold has already absorbed.
    pub fn check(&self, var: VarId, gen: u32, digest: ColumnDigest) -> Convergence {
        match self.at_gen.get(&var) {
            Some((g, d)) if *g == gen && *d == digest => Convergence::Converged,
            _ => Convergence::Diverged,
        }
    }

    /// Record the digest `var` carries at `gen`.
    pub fn record(&mut self, var: VarId, gen: u32, digest: ColumnDigest) {
        self.at_gen.insert(var, (gen, digest));
    }

    /// Forget a column (`drop`). `VarId`s are never reused, so this is hygiene
    /// against unbounded growth in a long session, not correctness.
    pub fn forget(&mut self, var: VarId) {
        self.at_gen.remove(&var);
    }

    /// The digest recorded for `var`, whatever generation it belongs to.
    #[must_use]
    pub fn get(&self, var: VarId) -> Option<(u32, ColumnDigest)> {
        self.at_gen.get(&var).copied()
    }

    /// Entries held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.at_gen.len()
    }

    /// True when nothing is cached.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.at_gen.is_empty()
    }

    /// Drop everything (`clear`, `use`, epoch reset).
    pub fn clear(&mut self) {
        self.at_gen.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_policy_is_bounded_at_the_designed_size() {
        assert_eq!(
            ConvergencePolicy::default(),
            ConvergencePolicy::Bounded {
                max_bytes: 256 * 1024 * 1024
            }
        );
        assert!(ConvergencePolicy::default().admits(80_000_000));
        assert!(!ConvergencePolicy::default().admits(40 * 1024 * 1024 * 1024));
        assert!(!ConvergencePolicy::Off.admits(1));
        assert!(ConvergencePolicy::Always.admits(u64::MAX));
    }

    #[test]
    fn settings_round_trip_their_spelling() {
        assert_eq!(ConvergencePolicy::Off.as_setting(), "provenance");
        assert_eq!(ConvergencePolicy::Always.as_setting(), "content");
        assert_eq!(
            ConvergencePolicy::Bounded { max_bytes: 1024 }.as_setting(),
            "content(1024)"
        );
    }

    #[test]
    fn a_digest_only_converges_against_its_own_generation() {
        let mut c = DigestCache::new();
        let d = ColumnDigest([1; 16]);
        assert_eq!(c.check(VarId(1), 0, d), Convergence::Diverged);
        c.record(VarId(1), 0, d);
        assert_eq!(c.check(VarId(1), 0, d), Convergence::Converged);
        assert_eq!(c.check(VarId(1), 1, d), Convergence::Diverged);
        assert_eq!(
            c.check(VarId(1), 0, ColumnDigest([2; 16])),
            Convergence::Diverged
        );
        c.forget(VarId(1));
        assert_eq!(c.check(VarId(1), 0, d), Convergence::Diverged);
    }
}
