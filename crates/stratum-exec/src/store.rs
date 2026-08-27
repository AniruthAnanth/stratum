//! A4 / C18 — the one store this crate writes: `engine/session.redb`.
//!
//! # The rule, and why it is a type
//!
//! redb takes an exclusive OS file lock, so two processes cannot share one file.
//! A4 therefore splits the cache in two: `engine/session.redb` belongs to the
//! `stratum serve` process, `ui/ui.redb` belongs to the desktop. If this crate
//! ever opened the desktop's file the failure would not be corruption — it would
//! be the *desktop* failing to start, on someone else's machine, naming a file
//! this crate is not mentioned in.
//!
//! So the path is not a parameter. [`SessionStore::open`] takes the cache root
//! and appends [`ENGINE_DB_RELATIVE`] itself; there is no constructor that takes
//! a full path. `stratum-ai` states the mirror-image rule in its own `UiStore`.
//!
//! # What is persisted, and the one thing that deliberately is not
//!
//! History and identity: the [`ExecutionRecord`] rows behind spec §11's History
//! pane and spec §20's "Created by `analysis.do:42`", the id counters that make
//! "Execution 41" still mean Execution 41 tomorrow ([`IdAllocator::restored`]),
//! and result text so a reopened project can render a card without re-running
//! the command that produced it.
//!
//! **Block statuses are not persisted, and must never be.** A status is a claim
//! about the CURRENT session (INV-1: `Current` means re-running right now would
//! produce these exact bytes). On reopen there is no session — the version table
//! is empty and the epoch is new — so every restored `Current` would be a lie of
//! precisely the §12 kind this engine exists to prevent. C2 marks every block
//! `Stale{EpochReset}` on a fresh session, and recomputing from the ledger is
//! both correct and cheap. Persisting the answer would trade a millisecond of
//! sweep for a research-integrity bug.

use camino::{Utf8Path, Utf8PathBuf};
use redb::{Database, ReadableDatabase as _, ReadableTable as _, TableDefinition};
use stratum_proto::{ExecutionRecord, ResultId};

use crate::ledger::{ExecutionLedger, IdAllocator};

/// The engine's store, relative to `.stratum/cache/<blake3-of-abs-path>/`.
pub const ENGINE_DB_RELATIVE: &str = "engine/session.redb";

/// The desktop's store. Named here **only** so the rule above can name it, and
/// so a `grep` for it in this crate finds a constant that is never opened.
pub const UI_DB_RELATIVE: &str = "ui/ui.redb";

/// `seq -> rmp-serde(ExecutionRecord)`. Keyed by `seq` because `seq` is the
/// global completion order the History pane pages through, so `page(from_seq)`
/// is a range scan rather than a sort.
const RECORDS: TableDefinition<'_, u64, &[u8]> = TableDefinition::new("exec_records");

/// `counter name -> high-water mark`. Named keys rather than a packed array so
/// adding a seventh counter later cannot silently reinterpret the other six.
const COUNTERS: TableDefinition<'_, &str, u64> = TableDefinition::new("id_counters");

/// `ResultId -> plain text`. The styled runs are reconstructible from the log;
/// what a reopened project needs is the text a card shows and `Raw ▸` serves.
const RESULT_TEXT: TableDefinition<'_, u64, &str> = TableDefinition::new("result_text");

/// The counter names, in [`IdAllocator::counters`] order. The array order is the
/// wire format of that function, so this table is the one place the mapping is
/// written down.
const COUNTER_NAMES: [&str; 6] = ["exec", "run", "result", "state", "dataset", "block"];

/// The store failed.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// redb said no — most often because another process in this same role
    /// already holds the file, which is a real condition worth reporting rather
    /// than retrying.
    #[error("session.redb: {0}")]
    Db(String),
    /// The directory could not be made.
    #[error("session.redb: {0}")]
    Io(String),
    /// A record could not be encoded or decoded. A row written by a newer schema
    /// is skipped on read rather than failing the open: losing history is
    /// survivable, refusing to start the engine is not.
    #[error("session.redb: {0}")]
    Codec(String),
}

/// The engine-owned session store.
#[derive(Debug)]
pub struct SessionStore {
    db: Database,
    path: Utf8PathBuf,
}

impl SessionStore {
    /// Open (creating) `<cache_root>/engine/session.redb`.
    ///
    /// # Errors
    /// [`StoreError`] when the directory cannot be created or redb cannot open
    /// the file.
    pub fn open(cache_root: &Utf8Path) -> Result<Self, StoreError> {
        let path = cache_root.join(ENGINE_DB_RELATIVE);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| StoreError::Io(format!("{parent}: {e}")))?;
        }
        let db = Database::create(path.as_std_path())
            .map_err(|e| StoreError::Db(format!("{path}: {e}")))?;
        Ok(Self { db, path })
    }

    /// The file actually opened. Asserted by the A4 test.
    #[must_use]
    pub fn path(&self) -> &Utf8Path {
        &self.path
    }

    // -- identity ----------------------------------------------------------

    /// Persist the id high-water marks.
    ///
    /// # Errors
    /// [`StoreError::Db`].
    pub fn put_counters(&self, counters: [u64; 6]) -> Result<(), StoreError> {
        let txn = self.db.begin_write().map_err(db)?;
        {
            let mut t = txn.open_table(COUNTERS).map_err(db)?;
            for (name, value) in COUNTER_NAMES.iter().zip(counters) {
                t.insert(*name, value).map_err(db)?;
            }
        }
        txn.commit().map_err(db)
    }

    /// The stored high-water marks, or zeroes when nothing was stored.
    ///
    /// A missing counter reads as 0, which restores to "allocate from 1" — the
    /// same behaviour as a fresh project rather than an error.
    #[must_use]
    pub fn counters(&self) -> [u64; 6] {
        let mut out = [0u64; 6];
        let Ok(txn) = self.db.begin_read() else {
            return out;
        };
        let Ok(t) = txn.open_table(COUNTERS) else {
            return out;
        };
        for (slot, name) in out.iter_mut().zip(COUNTER_NAMES) {
            if let Ok(Some(v)) = t.get(name) {
                *slot = v.value();
            }
        }
        out
    }

    /// An allocator that will not reissue an id this project already used.
    #[must_use]
    pub fn restore_ids(&self) -> IdAllocator {
        IdAllocator::restored(self.counters())
    }

    // -- history -----------------------------------------------------------

    /// Append one history row.
    ///
    /// # Errors
    /// [`StoreError::Codec`] when the record cannot be encoded,
    /// [`StoreError::Db`] when redb refuses the write.
    pub fn put_record(&self, record: &ExecutionRecord) -> Result<(), StoreError> {
        let bytes = rmp_serde::to_vec_named(record)
            .map_err(|e| StoreError::Codec(format!("record {}: {e}", record.exec.0)))?;
        let txn = self.db.begin_write().map_err(db)?;
        {
            let mut t = txn.open_table(RECORDS).map_err(db)?;
            t.insert(record.seq, bytes.as_slice()).map_err(db)?;
        }
        txn.commit().map_err(db)
    }

    /// The `seq` one past the highest row stored, i.e. where a checkpoint
    /// resumes.
    #[must_use]
    pub fn next_seq(&self) -> u64 {
        let Ok(txn) = self.db.begin_read() else {
            return 0;
        };
        let Ok(t) = txn.open_table(RECORDS) else {
            return 0;
        };
        // Bound to a local: the access guards `last()` hands back borrow `t`,
        // and letting them be the block's tail expression would drop `t` first.
        let next = match t.last() {
            Ok(Some((k, _))) => k.value() + 1,
            _ => 0,
        };
        next
    }

    /// History rows from `from_seq`, at most `limit`, oldest first — the same
    /// shape as [`crate::LedgerView::page`], so the History pane behaves
    /// identically whether the rows came from memory or from disk.
    ///
    /// A row that fails to decode is skipped, not fatal; see [`StoreError`].
    #[must_use]
    pub fn records(&self, from_seq: u64, limit: u32) -> Vec<ExecutionRecord> {
        let Ok(txn) = self.db.begin_read() else {
            return Vec::new();
        };
        let Ok(t) = txn.open_table(RECORDS) else {
            return Vec::new();
        };
        let Ok(iter) = t.range(from_seq..) else {
            return Vec::new();
        };
        iter.filter_map(Result::ok)
            .take(limit as usize)
            .filter_map(|(_, v)| rmp_serde::from_slice(v.value()).ok())
            .collect()
    }

    /// Write everything the ledger has that this store does not, plus the
    /// current id counters, in one transaction.
    ///
    /// Returns the number of rows appended. Counters are written LAST inside the
    /// same commit: a crash between the two would otherwise leave the allocator
    /// promising ids that already appear in history, and a reused
    /// [`stratum_proto::ExecutionId`] would make "Execution 41" ambiguous — the
    /// one thing spec §13's on-screen ids may never be.
    ///
    /// # Errors
    /// [`StoreError::Codec`] or [`StoreError::Db`].
    pub fn checkpoint(
        &self,
        ledger: &ExecutionLedger,
        ids: &IdAllocator,
    ) -> Result<usize, StoreError> {
        let from = self.next_seq();
        let pending: Vec<&ExecutionRecord> = ledger
            .records()
            .iter()
            .map(|c| &c.record)
            .filter(|r| r.seq >= from)
            .collect();
        let counters = ids.counters();

        let txn = self.db.begin_write().map_err(db)?;
        {
            let mut t = txn.open_table(RECORDS).map_err(db)?;
            for record in &pending {
                let bytes = rmp_serde::to_vec_named(record)
                    .map_err(|e| StoreError::Codec(format!("record {}: {e}", record.exec.0)))?;
                t.insert(record.seq, bytes.as_slice()).map_err(db)?;
            }
        }
        {
            let mut t = txn.open_table(COUNTERS).map_err(db)?;
            for (name, value) in COUNTER_NAMES.iter().zip(counters) {
                t.insert(*name, value).map_err(db)?;
            }
        }
        txn.commit().map_err(db)?;
        Ok(pending.len())
    }

    // -- result blobs ------------------------------------------------------

    /// Persist a result's plain text.
    ///
    /// # Errors
    /// [`StoreError::Db`].
    pub fn put_result_text(&self, result: ResultId, text: &str) -> Result<(), StoreError> {
        let txn = self.db.begin_write().map_err(db)?;
        {
            let mut t = txn.open_table(RESULT_TEXT).map_err(db)?;
            t.insert(result.0, text).map_err(db)?;
        }
        txn.commit().map_err(db)
    }

    /// A stored result's plain text, if this project kept one.
    #[must_use]
    pub fn result_text(&self, result: ResultId) -> Option<String> {
        let txn = self.db.begin_read().ok()?;
        let t = txn.open_table(RESULT_TEXT).ok()?;
        let v = t.get(result.0).ok()??;
        Some(v.value().to_owned())
    }

    /// Drop every stored result blob, keeping history and identity. This is what
    /// "clear cached output" means: the record that a command ran is history and
    /// survives; the bytes it printed are a cache and do not.
    ///
    /// # Errors
    /// [`StoreError::Db`].
    pub fn purge_results(&self) -> Result<(), StoreError> {
        let txn = self.db.begin_write().map_err(db)?;
        {
            let mut t = txn.open_table(RESULT_TEXT).map_err(db)?;
            t.retain(|_, _| false).map_err(db)?;
        }
        txn.commit().map_err(db)
    }
}

/// redb's errors are several types with no common trait we depend on; this is
/// the one place they become a [`StoreError`].
fn db<E: std::fmt::Display>(e: E) -> StoreError {
    StoreError::Db(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use stratum_proto::{
        BlockId, CodeHash, DatasetStateId, ExecOrigin, ExecStatus, ExecutionId, RunId,
        SessionEpoch, SessionId, StateId, Taint,
    };

    use crate::ledger::Committed;
    use crate::staleness::{RecordedReads, RecordedWrites};

    fn record(exec: u64) -> ExecutionRecord {
        ExecutionRecord {
            exec: ExecutionId(exec),
            seq: 0,
            session: SessionId(1),
            epoch: SessionEpoch(0),
            run: RunId(1),
            block: BlockId(exec),
            doc: None,
            origin: ExecOrigin::Editor,
            code_hash: CodeHash([7; 16]),
            source: format!("summarize v{exec}"),
            input_state: StateId(0),
            output_state: StateId(1),
            input_dataset: DatasetStateId(0),
            output_dataset: DatasetStateId(1),
            result: None,
            status: ExecStatus::Succeeded,
            started_at_ms: 1_700_000_000_000,
            duration_us: 4_200,
            stale_on_arrival: false,
            taint: Taint::empty(),
        }
    }

    fn committed(exec: u64) -> Committed {
        Committed {
            record: record(exec),
            reads: Arc::new(RecordedReads::default()),
            writes: Arc::new(RecordedWrites::default()),
        }
    }

    fn store(tmp: &tempfile::TempDir) -> SessionStore {
        SessionStore::open(Utf8Path::from_path(tmp.path()).unwrap()).unwrap()
    }

    #[test]
    fn the_store_opens_under_engine_and_never_touches_the_desktops_file() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        let s = store(&tmp);
        assert!(
            s.path().as_str().ends_with("engine/session.redb"),
            "{}",
            s.path()
        );
        assert!(
            !root.join(UI_DB_RELATIVE).exists(),
            "the desktop's file was created"
        );
    }

    #[test]
    fn history_round_trips_and_pages_from_a_seq() {
        let tmp = tempfile::tempdir().unwrap();
        let s = store(&tmp);
        assert_eq!(s.next_seq(), 0);

        let mut ledger = ExecutionLedger::new();
        for exec in 1..=5 {
            ledger.append(committed(exec));
        }
        let ids = IdAllocator::new();
        for _ in 0..5 {
            ids.exec();
        }

        assert_eq!(s.checkpoint(&ledger, &ids).unwrap(), 5);
        assert_eq!(s.next_seq(), 5);
        // Nothing new to write: a checkpoint is not a rewrite.
        assert_eq!(s.checkpoint(&ledger, &ids).unwrap(), 0);

        let page = s.records(2, 2);
        assert_eq!(page.len(), 2);
        assert_eq!(page[0].seq, 2);
        assert_eq!(page[0].source, "summarize v3");
        assert_eq!(page[1].seq, 3);
    }

    #[test]
    fn reopening_a_project_does_not_reissue_an_execution_id() {
        // "Execution 41" must still mean Execution 41 tomorrow (spec §13).
        let tmp = tempfile::tempdir().unwrap();
        {
            let s = store(&tmp);
            let ids = IdAllocator::new();
            for _ in 0..41 {
                ids.exec();
            }
            s.put_counters(ids.counters()).unwrap();
        }
        let s = store(&tmp);
        assert_eq!(s.counters()[0], 41);
        assert_eq!(s.restore_ids().exec(), ExecutionId(42));
    }

    #[test]
    fn a_fresh_store_restores_to_allocate_from_one() {
        let tmp = tempfile::tempdir().unwrap();
        let s = store(&tmp);
        assert_eq!(s.counters(), [0; 6]);
        assert_eq!(s.restore_ids().exec(), ExecutionId(1));
        assert!(s.records(0, 10).is_empty());
        assert!(s.result_text(ResultId(1)).is_none());
    }

    #[test]
    fn purging_results_keeps_history() {
        let tmp = tempfile::tempdir().unwrap();
        let s = store(&tmp);
        s.put_record(&record(1)).unwrap();
        s.put_result_text(ResultId(9), "  Variable |  Obs\n")
            .unwrap();
        assert_eq!(
            s.result_text(ResultId(9)).as_deref(),
            Some("  Variable |  Obs\n")
        );

        s.purge_results().unwrap();
        assert!(s.result_text(ResultId(9)).is_none());
        assert_eq!(s.records(0, 10).len(), 1, "history is not a cache");
    }
}
