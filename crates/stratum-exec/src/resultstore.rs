//! The result store — design 03 §9.4.
//!
//! > The channel carries **notifications**; the store carries the **truth**.
//!
//! That split is the whole reason output is never lost. When the event channel
//! is full the engine drops a redundant `Output` notification instead of
//! blocking, because the bytes are already here and the next notification
//! carries the new length anyway. An engine that blocks on a paused UI is an
//! engine that stops mid-`regress` because someone dragged a window.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::RwLock;
use rustc_hash::FxHashMap;
use stratum_proto::{AssetRef, RawRef, ResultId, ResultPayload, SessionId, StyledRun};

/// Inline `head` budget on a [`RawRef`]: the full text is always fetchable from
/// the asset scheme, so this only has to make `Raw ▸` instant for the ~99 % of
/// results that fit.
const RAW_HEAD_BYTES: usize = 8_192;

/// An append-only styled text buffer.
///
/// Readers take `committed_len` with `Acquire` and then read only that prefix,
/// so a reader never observes a partially-appended run. The writer publishes
/// with `Release` after the runs are in place. The chunk list is behind an
/// `RwLock` whose critical section is a `push` — the control thread and the UI
/// never wait on the session worker for longer than that (C50).
#[derive(Debug, Default)]
pub struct TextBuf {
    runs: RwLock<Vec<StyledRun>>,
    committed_len: AtomicU64,
    committed_runs: AtomicU64,
}

impl TextBuf {
    /// An empty buffer.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Append styled runs and publish them. Returns the new committed length in
    /// bytes, which is exactly what `EngineEvent::Output`'s consumer needs to
    /// know it is behind.
    pub fn append(&self, new: &[StyledRun]) -> u64 {
        let added: usize = new.iter().map(|r| r.text.len()).sum();
        {
            let mut runs = self.runs.write();
            runs.extend_from_slice(new);
            self.committed_runs
                .store(runs.len() as u64, Ordering::Relaxed);
        }
        self.committed_len
            .fetch_add(added as u64, Ordering::Release)
            + added as u64
    }

    /// Bytes committed so far.
    #[must_use]
    pub fn len(&self) -> u64 {
        self.committed_len.load(Ordering::Acquire)
    }

    /// True when nothing has been appended.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The committed prefix, as styled runs.
    #[must_use]
    pub fn runs(&self) -> Vec<StyledRun> {
        let n = self.committed_runs.load(Ordering::Acquire) as usize;
        self.runs.read()[..n].to_vec()
    }

    /// The committed prefix, flattened through the one sanctioned flattener
    /// (A12) so the log, the CLI and the goldens cannot drift apart.
    #[must_use]
    pub fn plain(&self) -> String {
        let n = self.committed_runs.load(Ordering::Acquire) as usize;
        stratum_proto::styled::to_plain(&self.runs.read()[..n])
    }
}

/// Every result's text and payloads, addressed by [`ResultId`].
#[derive(Debug, Default)]
pub struct ResultStore {
    texts: RwLock<FxHashMap<ResultId, Arc<TextBuf>>>,
    items: RwLock<FxHashMap<ResultId, Vec<ResultPayload>>>,
}

impl ResultStore {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Get or create the buffer for a result.
    pub fn text(&self, result: ResultId) -> Arc<TextBuf> {
        if let Some(buf) = self.texts.read().get(&result) {
            return Arc::clone(buf);
        }
        let mut w = self.texts.write();
        Arc::clone(w.entry(result).or_insert_with(|| Arc::new(TextBuf::new())))
    }

    /// Commit a payload — a table, a graph, a matrix.
    pub fn push_item(&self, result: ResultId, payload: ResultPayload) -> u32 {
        let mut items = self.items.write();
        let v = items.entry(result).or_default();
        v.push(payload);
        (v.len() - 1) as u32
    }

    /// Every payload committed for a result.
    #[must_use]
    pub fn items(&self, result: ResultId) -> Vec<ResultPayload> {
        self.items.read().get(&result).cloned().unwrap_or_default()
    }

    /// The `RawRef` for a result: an inline head plus the asset URL the raw
    /// pane fetches. Spec §17 — "every result exposes View raw/classic output"
    /// — so this is built for EVERY result, never only for the ones we have a
    /// rich renderer for.
    #[must_use]
    pub fn raw_ref(&self, session: SessionId, result: ResultId) -> RawRef {
        let buf = self.text(result);
        let plain = buf.plain();
        let bytes = plain.len() as u64;
        let head_end = head_boundary(&plain);
        RawRef {
            bytes,
            lines: u32::try_from(plain.lines().count()).unwrap_or(u32::MAX),
            head: plain[..head_end].to_owned(),
            truncated: head_end < plain.len(),
            asset: AssetRef {
                path: format!("result/{}/{}/raw", session.0, result.0),
                mime: "text/plain; charset=utf-8".to_owned(),
                bytes,
            },
        }
    }

    /// Drop everything for a result — used when an epoch reset retires a
    /// session's output.
    pub fn forget(&self, result: ResultId) {
        self.texts.write().remove(&result);
        self.items.write().remove(&result);
    }

    /// How many results are held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.texts.read().len()
    }

    /// True when no result has been recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.texts.read().is_empty()
    }
}

/// The largest prefix of `s` no longer than [`RAW_HEAD_BYTES`] that ends on a
/// line boundary — and, failing that, on a char boundary, because slicing a
/// `String` mid-UTF-8 panics and a panic in the output path would take the
/// engine down over a piece of formatting.
fn head_boundary(s: &str) -> usize {
    if s.len() <= RAW_HEAD_BYTES {
        return s.len();
    }
    match s[..RAW_HEAD_BYTES].rfind('\n') {
        Some(nl) => nl + 1,
        None => {
            let mut end = RAW_HEAD_BYTES;
            while end > 0 && !s.is_char_boundary(end) {
                end -= 1;
            }
            end
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stratum_proto::StyleId;

    fn run(text: &str) -> StyledRun {
        StyledRun {
            text: text.to_owned(),
            style: StyleId::Result,
        }
    }

    #[test]
    fn appends_are_visible_only_once_committed() {
        let buf = TextBuf::new();
        assert!(buf.is_empty());
        assert_eq!(buf.append(&[run("abc"), run("de")]), 5);
        assert_eq!(buf.len(), 5);
        assert_eq!(buf.plain(), "abcde");
    }

    #[test]
    fn the_raw_head_cuts_at_a_line_boundary() {
        let store = ResultStore::new();
        let id = ResultId(7);
        let line = format!("{}\n", "x".repeat(99));
        for _ in 0..200 {
            store.text(id).append(&[run(&line)]);
        }
        let raw = store.raw_ref(SessionId(3), id);
        assert!(raw.truncated);
        assert!(raw.head.ends_with('\n'));
        assert!(raw.head.len() <= RAW_HEAD_BYTES);
        assert_eq!(raw.asset.path, "result/3/7/raw");
    }

    #[test]
    fn a_multibyte_head_never_splits_a_char() {
        let store = ResultStore::new();
        let id = ResultId(1);
        store.text(id).append(&[run(&"é".repeat(RAW_HEAD_BYTES))]);
        let raw = store.raw_ref(SessionId(1), id);
        assert!(raw.head.len() <= RAW_HEAD_BYTES);
        assert!(raw.truncated);
    }
}
