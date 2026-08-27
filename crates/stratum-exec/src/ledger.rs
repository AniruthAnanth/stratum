//! The append-only execution ledger — design 03 §3, CONTRACTS §6.
//!
//! Every completion attempt appends one immutable row. Nothing is ever
//! rewritten: `latest_by_block` is an index INTO the history, not a mutable
//! "current" table, which is what makes spec §11's History pane and spec §20's
//! "Created by `analysis.do:42`" answerable from the same structure the
//! staleness sweep reads.
//!
//! Only `latest_by_block` participates in staleness. Everything else is history.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use rustc_hash::FxHashMap;
use stratum_proto::{
    BlockId, DatasetStateId, DepKey, ExecutionId, ExecutionRecord, ResultId, RunId, StateId,
};

use crate::staleness::{RecordedReads, RecordedWrites};

/// One committed execution: the wire record, plus the two footprints the
/// staleness sweep compares. The footprints stay engine-side — they are what
/// C6 and C7 consult, and shipping them to every window would put a variable
/// list on the wire after every keystroke-triggered commit.
#[derive(Clone, Debug)]
pub struct Committed {
    /// The append-only history row (immutable once `status` leaves Running).
    pub record: ExecutionRecord,
    /// What this execution actually read.
    pub reads: Arc<RecordedReads>,
    /// What this execution actually wrote.
    pub writes: Arc<RecordedWrites>,
}

/// Session-monotone id counters.
///
/// All counters are per-session, monotone, never reused and never recycled
/// after a `clear` — so "Execution 41" still means Execution 41 tomorrow, which
/// is exactly what spec §13 puts on screen. Shared across the control thread
/// and the session worker, hence the atomics.
#[derive(Debug, Default)]
pub struct IdAllocator {
    exec: AtomicU64,
    run: AtomicU64,
    result: AtomicU64,
    state: AtomicU64,
    dataset: AtomicU64,
    block: AtomicU64,
}

impl IdAllocator {
    /// A fresh allocator. Ids start at 1: `BlockId(0)` is
    /// [`BlockId::EPHEMERAL`], so a block id must never be allocated as zero.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Restore the counters from the sidecar so ids do not restart at 1 when a
    /// project is reopened.
    #[must_use]
    pub fn restored(counters: [u64; 6]) -> Self {
        Self {
            exec: AtomicU64::new(counters[0]),
            run: AtomicU64::new(counters[1]),
            result: AtomicU64::new(counters[2]),
            state: AtomicU64::new(counters[3]),
            dataset: AtomicU64::new(counters[4]),
            block: AtomicU64::new(counters[5]),
        }
    }

    /// The current high-water marks, for persistence.
    #[must_use]
    pub fn counters(&self) -> [u64; 6] {
        [
            self.exec.load(Ordering::Relaxed),
            self.run.load(Ordering::Relaxed),
            self.result.load(Ordering::Relaxed),
            self.state.load(Ordering::Relaxed),
            self.dataset.load(Ordering::Relaxed),
            self.block.load(Ordering::Relaxed),
        ]
    }

    /// Next execution id.
    pub fn exec(&self) -> ExecutionId {
        ExecutionId(self.exec.fetch_add(1, Ordering::Relaxed) + 1)
    }
    /// Next run id.
    pub fn run(&self) -> RunId {
        RunId(self.run.fetch_add(1, Ordering::Relaxed) + 1)
    }
    /// Next result id.
    pub fn result(&self) -> ResultId {
        ResultId(self.result.fetch_add(1, Ordering::Relaxed) + 1)
    }
    /// Next whole-session state id.
    pub fn state(&self) -> StateId {
        StateId(self.state.fetch_add(1, Ordering::Relaxed) + 1)
    }
    /// Next dataset state id. Interning a converged fingerprint back to an
    /// EXISTING id is the runtime's job (`03` §4.4); this only mints new ones.
    pub fn dataset(&self) -> DatasetStateId {
        DatasetStateId(self.dataset.fetch_add(1, Ordering::Relaxed) + 1)
    }
    /// Next block id. **`BlockId` is allocated only by `stratum-exec`**
    /// (CONTRACTS §2), and only here.
    pub fn block(&self) -> BlockId {
        BlockId(self.block.fetch_add(1, Ordering::Relaxed) + 1)
    }
}

/// The history.
#[derive(Debug, Default)]
pub struct ExecutionLedger {
    records: Vec<Committed>,
    by_exec: FxHashMap<ExecutionId, usize>,
    latest_by_block: FxHashMap<BlockId, usize>,
    next_seq: u64,
}

impl ExecutionLedger {
    /// An empty ledger.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Append one row, stamping `seq` with the next global completion order.
    ///
    /// `EPHEMERAL` and `NONE` blocks are recorded in history but never enter
    /// `latest_by_block` (A3): a command-bar run must not repaint every trivia
    /// region in the document, and `latest_by_block[0]` would otherwise be
    /// ambiguous between "the last command-bar run" and "every comment".
    pub fn append(&mut self, mut committed: Committed) -> u64 {
        let seq = self.next_seq;
        self.next_seq += 1;
        committed.record.seq = seq;
        let idx = self.records.len();
        self.by_exec.insert(committed.record.exec, idx);
        if committed.record.block.is_real() {
            self.latest_by_block.insert(committed.record.block, idx);
        }
        self.records.push(committed);
        seq
    }

    /// The record that owns this block's status, or `None` for `NeverRun`.
    #[must_use]
    pub fn latest_for(&self, block: BlockId) -> Option<&Committed> {
        self.latest_by_block.get(&block).map(|i| &self.records[*i])
    }

    /// Look one up by execution id.
    #[must_use]
    pub fn by_exec(&self, exec: ExecutionId) -> Option<&Committed> {
        self.by_exec.get(&exec).map(|i| &self.records[*i])
    }

    /// Every row, oldest first.
    #[must_use]
    pub fn records(&self) -> &[Committed] {
        &self.records
    }

    /// The next `seq` that will be stamped.
    #[must_use]
    pub fn next_seq(&self) -> u64 {
        self.next_seq
    }

    /// Drop a block from the status index without touching history — the
    /// "retired" half of reconcile (CONTRACTS §2): the block left the document,
    /// its records stay.
    pub fn retire(&mut self, block: BlockId) {
        self.latest_by_block.remove(&block);
    }

    /// A read-only view for the provenance queries spec §20 asks.
    #[must_use]
    pub fn view(&self) -> LedgerView<'_> {
        LedgerView { ledger: self }
    }
}

/// Provenance and history queries over the ledger.
pub struct LedgerView<'a> {
    ledger: &'a ExecutionLedger,
}

impl<'a> LedgerView<'a> {
    /// `EngineRequest::Ledger` — rows from `from_seq`, at most `limit`.
    #[must_use]
    pub fn page(&self, from_seq: u64, limit: u32) -> (Vec<ExecutionRecord>, u64) {
        let start = self
            .ledger
            .records
            .partition_point(|r| r.record.seq < from_seq);
        let end = (start + limit as usize).min(self.ledger.records.len());
        let rows: Vec<ExecutionRecord> = self.ledger.records[start..end]
            .iter()
            .map(|c| c.record.clone())
            .collect();
        let next = rows.last().map_or(from_seq, |r| r.seq + 1);
        (rows, next)
    }

    /// "Created by `analysis.do:42`" (spec §20). Exact for executed code; a
    /// caller that gets `None` falls back to the static effect sets and must
    /// label the answer as inferred.
    #[must_use]
    pub fn creator(&self, frame: &str, name: &str) -> Option<(BlockId, ExecutionId)> {
        let key = DepKey::Var {
            frame: frame.to_owned(),
            name: name.to_owned(),
        };
        self.ledger
            .records
            .iter()
            .rev()
            .find(|c| c.writes.creates.contains(&key))
            .map(|c| (c.record.block, c.record.exec))
    }

    /// "Used by …" (spec §20), in document order of first read.
    #[must_use]
    pub fn readers(&self, frame: &str, name: &str) -> Vec<BlockId> {
        let key = DepKey::Var {
            frame: frame.to_owned(),
            name: name.to_owned(),
        };
        let mut out = Vec::new();
        for c in &self.ledger.records {
            if c.record.block.is_real()
                && c.reads.deps.iter().any(|d| d.key == key)
                && !out.contains(&c.record.block)
            {
                out.push(c.record.block);
            }
        }
        out
    }

    /// The last `n` command lines, newest last — `AiContextWant::RECENT_COMMANDS`.
    #[must_use]
    pub fn recent_commands(&self, n: usize) -> Vec<String> {
        let start = self.ledger.records.len().saturating_sub(n);
        self.ledger.records[start..]
            .iter()
            .map(|c| c.record.source.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_monotone_and_never_zero() {
        let ids = IdAllocator::new();
        assert_eq!(ids.block(), BlockId(1));
        assert_eq!(ids.block(), BlockId(2));
        assert_ne!(ids.block(), BlockId::EPHEMERAL);
        assert_eq!(ids.exec(), ExecutionId(1));
    }

    #[test]
    fn restored_counters_do_not_reissue() {
        let ids = IdAllocator::restored([41, 0, 0, 0, 0, 7]);
        assert_eq!(ids.exec(), ExecutionId(42));
        assert_eq!(ids.block(), BlockId(8));
    }
}
