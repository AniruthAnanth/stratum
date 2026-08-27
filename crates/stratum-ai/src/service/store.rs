//! A4 / C18 — the one store this crate writes: `ui/ui.redb`.
//!
//! # The rule, and why it is a type
//!
//! Two processes open one redb file and redb takes an exclusive lock. A4 splits
//! them: `engine/session.redb` belongs to the `stratum serve` process,
//! `ui/ui.redb` belongs to the desktop, and `stratum-ai` is linked into the
//! desktop. If this crate ever opened the engine's file the failure would not be
//! a corruption — it would be the *engine* failing to start, on somebody else's
//! machine, with an error naming a file this crate is not mentioned in.
//!
//! So the path is not a parameter. [`UiStore::open`] takes the cache root and
//! appends [`UI_DB_RELATIVE`] itself; there is no constructor that takes a full
//! path, and `tests/ui_store_only.rs` opens `engine/session.redb` exclusively
//! first and asserts this store still opens — which it could not do if it
//! reached for the wrong file.
//!
//! # What lives here
//!
//! `07` §11.3's third cache layer (persistent, for auto-comment and repro
//! explanations, keyed by content hash so re-running over an unchanged file
//! costs nothing) and the chat transcripts whose stable *association* the
//! committed sidecar holds (CONTRACTS §12, A34). Both are purged by "Delete all
//! AI history" alongside the audit log.

use camino::{Utf8Path, Utf8PathBuf};
use redb::{Database, ReadableDatabase as _, ReadableTable as _, TableDefinition};

use crate::tasks::cache::CacheKey;

/// The desktop's store, relative to `.stratum/cache/<hash>/`.
pub const UI_DB_RELATIVE: &str = "ui/ui.redb";

/// The engine's store. Named here **only** so the refusal below can name it, and
/// so a `grep` for it in this crate finds a constant that is never opened.
pub const ENGINE_DB_RELATIVE: &str = "engine/session.redb";

/// `conversation id -> transcript JSON`.
const TRANSCRIPTS: TableDefinition<'_, &str, &str> = TableDefinition::new("ai_transcripts");

/// `cache key -> response JSON`. Keyed by content hash, so re-running
/// auto-comment over an unchanged file is free.
const RESPONSES: TableDefinition<'_, &[u8; 16], &str> = TableDefinition::new("ai_responses");

/// The store failed.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// redb said no.
    #[error("ui.redb: {0}")]
    Db(String),
    /// The directory could not be made.
    #[error("ui.redb: {0}")]
    Io(String),
}

/// The desktop-owned AI store.
#[derive(Debug)]
pub struct UiStore {
    db: Database,
    path: Utf8PathBuf,
}

impl UiStore {
    /// Open (creating) `<cache_root>/ui/ui.redb`.
    ///
    /// # Errors
    /// [`StoreError`] when the directory cannot be created or redb cannot open
    /// the file — most often because another process in this same role already
    /// holds it, which is a real condition worth reporting rather than
    /// retrying.
    pub fn open(cache_root: &Utf8Path) -> Result<Self, StoreError> {
        let path = cache_root.join(UI_DB_RELATIVE);
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

    /// Store a conversation transcript.
    ///
    /// # Errors
    /// [`StoreError::Db`].
    pub fn put_transcript(&self, conversation_id: &str, json: &str) -> Result<(), StoreError> {
        let txn = self
            .db
            .begin_write()
            .map_err(|e| StoreError::Db(e.to_string()))?;
        {
            let mut t = txn
                .open_table(TRANSCRIPTS)
                .map_err(|e| StoreError::Db(e.to_string()))?;
            t.insert(conversation_id, json)
                .map_err(|e| StoreError::Db(e.to_string()))?;
        }
        txn.commit().map_err(|e| StoreError::Db(e.to_string()))
    }

    /// Read a transcript back.
    ///
    /// A missing table is `None`, not an error: a fresh clone has never written
    /// one, and the committed sidecar's association pointing at a transcript the
    /// user does not have a copy of is the *designed* state (A34), not a fault.
    #[must_use]
    pub fn transcript(&self, conversation_id: &str) -> Option<String> {
        let txn = self.db.begin_read().ok()?;
        let t = txn.open_table(TRANSCRIPTS).ok()?;
        let v = t.get(conversation_id).ok()??;
        Some(v.value().to_owned())
    }

    /// Every conversation id held, sorted.
    #[must_use]
    pub fn conversations(&self) -> Vec<String> {
        let Ok(txn) = self.db.begin_read() else {
            return Vec::new();
        };
        let Ok(t) = txn.open_table(TRANSCRIPTS) else {
            return Vec::new();
        };
        let Ok(iter) = t.iter() else {
            return Vec::new();
        };
        iter.filter_map(Result::ok)
            .map(|(k, _)| k.value().to_owned())
            .collect()
    }

    /// Store a response against a content-hash key.
    ///
    /// # Errors
    /// [`StoreError::Db`].
    pub fn put_response(&self, key: CacheKey, json: &str) -> Result<(), StoreError> {
        let txn = self
            .db
            .begin_write()
            .map_err(|e| StoreError::Db(e.to_string()))?;
        {
            let mut t = txn
                .open_table(RESPONSES)
                .map_err(|e| StoreError::Db(e.to_string()))?;
            t.insert(&key.bytes(), json)
                .map_err(|e| StoreError::Db(e.to_string()))?;
        }
        txn.commit().map_err(|e| StoreError::Db(e.to_string()))
    }

    /// Read a cached response back.
    #[must_use]
    pub fn response(&self, key: CacheKey) -> Option<String> {
        let txn = self.db.begin_read().ok()?;
        let t = txn.open_table(RESPONSES).ok()?;
        let v = t.get(&key.bytes()).ok()??;
        Some(v.value().to_owned())
    }

    /// "Delete all AI history": every transcript and every cached response.
    ///
    /// # Errors
    /// [`StoreError::Db`].
    pub fn purge_all(&self) -> Result<(), StoreError> {
        let txn = self
            .db
            .begin_write()
            .map_err(|e| StoreError::Db(e.to_string()))?;
        {
            let mut t = txn
                .open_table(TRANSCRIPTS)
                .map_err(|e| StoreError::Db(e.to_string()))?;
            t.retain(|_, _| false)
                .map_err(|e| StoreError::Db(e.to_string()))?;
        }
        {
            let mut t = txn
                .open_table(RESPONSES)
                .map_err(|e| StoreError::Db(e.to_string()))?;
            t.retain(|_, _| false)
                .map_err(|e| StoreError::Db(e.to_string()))?;
        }
        txn.commit().map_err(|e| StoreError::Db(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::types::{ModelId, ProviderId};

    fn key(n: u32) -> CacheKey {
        CacheKey::new(
            ProviderId::Anthropic,
            &ModelId::from("claude-opus-5"),
            "p",
            n,
        )
    }

    #[test]
    fn the_store_opens_under_ui_and_nowhere_else() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        let store = UiStore::open(root).unwrap();
        assert!(
            store.path().as_str().ends_with("ui/ui.redb"),
            "{}",
            store.path()
        );
        assert!(
            !root.join(ENGINE_DB_RELATIVE).exists(),
            "the engine's file was created"
        );
    }

    #[test]
    fn transcripts_and_responses_round_trip_and_purge() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        let store = UiStore::open(root).unwrap();

        store.put_transcript("c1", r#"{"turns":[]}"#).unwrap();
        store.put_response(key(1), r#"{"text":"cached"}"#).unwrap();
        assert_eq!(store.transcript("c1").as_deref(), Some(r#"{"turns":[]}"#));
        assert_eq!(
            store.response(key(1)).as_deref(),
            Some(r#"{"text":"cached"}"#)
        );
        assert_eq!(store.conversations(), vec!["c1".to_owned()]);

        store.purge_all().unwrap();
        assert!(store.transcript("c1").is_none());
        assert!(store.response(key(1)).is_none());
        assert!(store.conversations().is_empty());
    }

    #[test]
    fn a_transcript_that_was_never_written_reads_as_absent_not_as_an_error() {
        // A collaborator who clones the repo sees "this block has a conversation
        // you do not have a copy of" (A34), which is a state, not a fault.
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        let store = UiStore::open(root).unwrap();
        assert!(store.transcript("never-written").is_none());
        assert!(store.conversations().is_empty());
    }
}
