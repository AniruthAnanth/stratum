//! Tunable thresholds, the instrumentation counters, and the Q9 memory policy.
//!
//! # Why counters live in the shipping build
//!
//! ADR-017 is binding: *"a performance acceptance bullet must assert a counter
//! — work done, allocations, regions re-hashed, bytes copied — and not a
//! duration."* A counter that only exists under `cfg(test)` cannot be asserted
//! about the code that actually ships, so these are always compiled.
//!
//! They are affordable because **nothing here is incremented per row.** Every
//! `fetch_add` in this crate happens once per chunk (65 536 rows), once per
//! radix pass, or once per journal entry. A 10 M-row `replace` performs 153
//! relaxed atomic adds, not 10 000 000.
//!
//! # Thresholds
//!
//! `04` §12.3: the thresholds live in ONE module so they can be retuned from a
//! benchmark without editing a kernel.

use std::sync::atomic::{AtomicU64, Ordering};

/// Below this many rows the radix sort's key-materialisation and histogram
/// passes cost more than a comparator sort does in total (`04` §6.2).
pub const RADIX_MIN_ROWS: u64 = 1 << 16;

/// A key wider than this goes to the comparator path. A `str200` key would make
/// the radix sort do 200 passes over `n × 200` bytes to sort a column whose
/// first eight bytes almost always decide the answer (`04` §6.2).
pub const RADIX_MAX_KEY_BYTES: usize = 16;

/// Rayon is not asked to help below this many chunks; the dispatch costs more
/// than the work and the answer is identical either way.
pub const PAR_MIN_CHUNKS: usize = 2;

/// Bytes below which the bulk row-major ingest stays single-threaded
/// (`04` §12.3: "above a 1 MiB threshold").
pub const PAR_MIN_INGEST_BYTES: usize = 1 << 20;

/// Every counter this crate maintains. All monotonic, all relaxed.
///
/// Read them with [`Counters::snapshot`] and compare two snapshots with
/// [`Snapshot::since`]; that is the shape every acceptance assertion in
/// `tests/journal.rs`, `tests/cow.rs` and `tests/sort.rs` uses.
#[derive(Debug, Default)]
pub struct Counters {
    /// Rows visited by a column scan, summed over chunks.
    pub rows_touched: AtomicU64,
    /// Chunks deep-copied by `Arc::make_mut` behind the write barrier. This is
    /// the A18 number: one per chunk a command actually writes into.
    pub chunks_cloned: AtomicU64,
    /// Bytes those chunk copies moved.
    pub chunk_bytes_cloned: AtomicU64,
    /// Entries pushed onto an undo journal.
    pub journal_entries: AtomicU64,
    /// Bytes an undo journal is currently retaining.
    pub journal_bytes: AtomicU64,
    /// Whole-column rewrites (storage-type promotion, `recast`).
    pub column_rewrites: AtomicU64,
    /// Column `Arc` clones that were *pointer* clones — snapshots, `frame copy`.
    pub column_arc_clones: AtomicU64,
    /// Counting-sort passes performed by the radix sort.
    pub radix_passes: AtomicU64,
    /// Rows scattered by the radix sort, summed over passes.
    pub radix_rows: AtomicU64,
    /// Comparator invocations on the `pdqsort` path. Zero on the radix path.
    pub comparisons: AtomicU64,
    /// Rows widened to `f64` through a scratch buffer — i.e. rows that were NOT
    /// zero-copy. A `Double` column contributes nothing here.
    pub rows_widened: AtomicU64,
    /// Cells written by the bulk row-major ingest. One copy per cell is the
    /// floor for a layout change, so this doubles as "copies on the bulk path".
    pub ingest_cells: AtomicU64,
}

/// A plain-value reading of [`Counters`], safe to subtract.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[allow(missing_docs)] // one-for-one with `Counters`, documented there.
pub struct Snapshot {
    pub rows_touched: u64,
    pub chunks_cloned: u64,
    pub chunk_bytes_cloned: u64,
    pub journal_entries: u64,
    pub journal_bytes: u64,
    pub column_rewrites: u64,
    pub column_arc_clones: u64,
    pub radix_passes: u64,
    pub radix_rows: u64,
    pub comparisons: u64,
    pub rows_widened: u64,
    pub ingest_cells: u64,
}

impl Counters {
    /// Read every counter. Not atomic as a group — counters are only ever read
    /// from a test or a bench with no other work in flight.
    #[must_use]
    pub fn snapshot(&self) -> Snapshot {
        let g = |a: &AtomicU64| a.load(Ordering::Relaxed);
        Snapshot {
            rows_touched: g(&self.rows_touched),
            chunks_cloned: g(&self.chunks_cloned),
            chunk_bytes_cloned: g(&self.chunk_bytes_cloned),
            journal_entries: g(&self.journal_entries),
            journal_bytes: g(&self.journal_bytes),
            column_rewrites: g(&self.column_rewrites),
            column_arc_clones: g(&self.column_arc_clones),
            radix_passes: g(&self.radix_passes),
            radix_rows: g(&self.radix_rows),
            comparisons: g(&self.comparisons),
            rows_widened: g(&self.rows_widened),
            ingest_cells: g(&self.ingest_cells),
        }
    }
}

impl Snapshot {
    /// `self - earlier`, field by field. Saturating, so a counter that wrapped
    /// (it cannot, at `u64`) reads as zero rather than as a colossal delta.
    #[must_use]
    pub fn since(&self, earlier: Snapshot) -> Snapshot {
        Snapshot {
            rows_touched: self.rows_touched.saturating_sub(earlier.rows_touched),
            chunks_cloned: self.chunks_cloned.saturating_sub(earlier.chunks_cloned),
            chunk_bytes_cloned: self
                .chunk_bytes_cloned
                .saturating_sub(earlier.chunk_bytes_cloned),
            journal_entries: self.journal_entries.saturating_sub(earlier.journal_entries),
            journal_bytes: self.journal_bytes.saturating_sub(earlier.journal_bytes),
            column_rewrites: self.column_rewrites.saturating_sub(earlier.column_rewrites),
            column_arc_clones: self
                .column_arc_clones
                .saturating_sub(earlier.column_arc_clones),
            radix_passes: self.radix_passes.saturating_sub(earlier.radix_passes),
            radix_rows: self.radix_rows.saturating_sub(earlier.radix_rows),
            comparisons: self.comparisons.saturating_sub(earlier.comparisons),
            rows_widened: self.rows_widened.saturating_sub(earlier.rows_widened),
            ingest_cells: self.ingest_cells.saturating_sub(earlier.ingest_cells),
        }
    }
}

static COUNTERS: Counters = Counters {
    rows_touched: AtomicU64::new(0),
    chunks_cloned: AtomicU64::new(0),
    chunk_bytes_cloned: AtomicU64::new(0),
    journal_entries: AtomicU64::new(0),
    journal_bytes: AtomicU64::new(0),
    column_rewrites: AtomicU64::new(0),
    column_arc_clones: AtomicU64::new(0),
    radix_passes: AtomicU64::new(0),
    radix_rows: AtomicU64::new(0),
    comparisons: AtomicU64::new(0),
    rows_widened: AtomicU64::new(0),
    ingest_cells: AtomicU64::new(0),
};

/// The process-wide counters.
#[inline]
#[must_use]
pub fn counters() -> &'static Counters {
    &COUNTERS
}

/// Add `n` to one counter. Relaxed: these are diagnostics, never a happens-before.
#[inline]
pub(crate) fn bump(counter: &AtomicU64, n: u64) {
    counter.fetch_add(n, Ordering::Relaxed);
}

/// Subtract `n` from one counter, saturating at zero.
///
/// Only [`Counters::journal_bytes`] uses this: it is a *level*, not a total, so
/// a rolled-back or committed journal has to give its bytes back.
#[inline]
pub(crate) fn drop_level(counter: &AtomicU64, n: u64) {
    let _ = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
        Some(v.saturating_sub(n))
    });
}

// ---------------------------------------------------------------------------
// Q9 — the out-of-RAM policy for `use`
// ---------------------------------------------------------------------------

/// **Q9, v1.** The engine is resident-only and fails *precisely* rather than
/// spilling or silently thrashing.
///
/// `04` §15 left this open ("fail with a precise message, or spill columns to a
/// memory-mapped scratch file"), and A18 defused it: because a column is a
/// `Vec<Arc<chunk>>`, a chunk that is backed by an mmap instead of by the heap
/// changes no kernel signature at all. So the expensive half of the question —
/// "does `Column` need an `Mmap` variant *now*" — is answered **no**, and the
/// cheap half is answered here.
///
/// Under the §0a speed priority, spilling is the wrong v1 default anyway: a
/// dataset the researcher keeps open for hours must stay resident, because a
/// paged-out column turns a 6 ms `summarize` into a disk seek per chunk. The
/// honest behaviour when it does not fit is a refusal that says how much was
/// needed and how much was allowed — not a load that appears to work and then
/// stalls unpredictably for the rest of the session.
///
/// The limit is *injected*, never sensed: `stratum-data` may not reach the
/// platform layer (ARCHITECTURE §8.1), so the host — which does know the
/// machine — calls [`MemoryPolicy::set_limit_bytes`]. Unset means unlimited,
/// which is what every test and every headless run wants.
#[derive(Debug)]
pub struct MemoryPolicy {
    limit: AtomicU64,
}

impl Default for MemoryPolicy {
    /// Unlimited, which is what every test and every headless run wants.
    fn default() -> Self {
        Self {
            limit: AtomicU64::new(u64::MAX),
        }
    }
}

impl MemoryPolicy {
    /// Set the ceiling in bytes. `u64::MAX` means "no ceiling".
    pub fn set_limit_bytes(&self, bytes: u64) {
        self.limit.store(bytes, Ordering::Relaxed);
    }

    /// The current ceiling.
    #[must_use]
    pub fn limit_bytes(&self) -> u64 {
        self.limit.load(Ordering::Relaxed)
    }

    /// Would `required` more bytes, on top of `resident`, fit under the ceiling?
    ///
    /// # Errors
    ///
    /// [`CapacityError`] — carrying required, resident and limit — when it would
    /// not. That is the "rc 909 + free/required bytes" ARCHITECTURE §9 asks for,
    /// with the numbers attached rather than described.
    pub fn admit(&self, resident: u64, required: u64) -> Result<(), CapacityError> {
        let limit = self.limit_bytes();
        let total = resident.saturating_add(required);
        if total <= limit {
            Ok(())
        } else {
            Err(CapacityError {
                required_bytes: required,
                resident_bytes: resident,
                limit_bytes: limit,
            })
        }
    }
}

static MEMORY_POLICY: MemoryPolicy = MemoryPolicy {
    limit: AtomicU64::new(u64::MAX),
};

/// The process-wide [`MemoryPolicy`].
#[inline]
#[must_use]
pub fn memory_policy() -> &'static MemoryPolicy {
    &MEMORY_POLICY
}

/// "Not enough memory to load this dataset", with the numbers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error(
    "op. sys. refuses to provide memory: {required_bytes} bytes required, \
     {resident_bytes} bytes already resident, {limit_bytes} bytes allowed"
)]
pub struct CapacityError {
    /// What the operation asked for.
    pub required_bytes: u64,
    /// What the frames already hold.
    pub resident_bytes: u64,
    /// The configured ceiling.
    pub limit_bytes: u64,
}

impl CapacityError {
    /// Stata's return code for "op. sys. refuses to provide memory".
    pub const RC: u16 = 909;

    /// Bytes still available under the ceiling. Zero when already over.
    #[must_use]
    pub fn free_bytes(&self) -> u64 {
        self.limit_bytes.saturating_sub(self.resident_bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thresholds_are_the_measured_ones() {
        // `04` §6.2: radix above 2^16 rows, comparator for keys wider than 16
        // bytes. Both are quoted in the module doc of `sort`.
        assert_eq!(RADIX_MIN_ROWS, 65_536);
        assert_eq!(RADIX_MAX_KEY_BYTES, 16);
    }

    #[test]
    fn an_unset_policy_admits_everything() {
        let p = MemoryPolicy {
            limit: AtomicU64::new(u64::MAX),
        };
        assert!(p.admit(0, u64::MAX / 2).is_ok());
    }

    #[test]
    fn a_refusal_carries_the_numbers_and_rc_909() {
        let p = MemoryPolicy {
            limit: AtomicU64::new(1_000),
        };
        let e = p
            .admit(400, 800)
            .expect_err("800 on top of 400 exceeds 1000");
        assert_eq!(e.required_bytes, 800);
        assert_eq!(e.free_bytes(), 600);
        assert_eq!(CapacityError::RC, 909);
        // The message has to be actionable on its own; a bare "out of memory"
        // is what we are deliberately not shipping.
        assert!(e.to_string().contains("800 bytes required"));
    }

    #[test]
    fn admission_does_not_overflow_on_absurd_requests() {
        let p = MemoryPolicy {
            limit: AtomicU64::new(1_000),
        };
        assert!(p.admit(u64::MAX, u64::MAX).is_err());
    }
}
