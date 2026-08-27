//! The chunk-granular undo journal (A18) that makes INV-2 affordable.
//!
//! > **INV-2.** A command with `Atomicity::Rollbackable` either completes or
//! > leaves dataset and session state exactly as at entry.
//!
//! The pre-audit rule was "`col_mut` retains the previous `Arc` before making it
//! unique". Retaining raises the strong count to 2, so `Arc::make_mut`
//! deep-copies — which means **every** `replace x = x+1` on 10 M doubles
//! allocated and memcpy'd 80 MB, whether or not anything interrupted it, and a
//! rollback still had to restore the whole column.
//!
//! Here the barrier journals the **chunk** it is about to dirty. `replace x = 1
//! in 1` retains one 512 KiB chunk. A whole-column `replace` retains
//! `ceil(n / CHUNK_ROWS)` chunks *as it walks them* — one extra pass, never a
//! second column in flight — and rollback costs exactly what the command wrote.
//!
//! Everything here is counted: [`Journal::len`] is entries,
//! [`Journal::retained_bytes`] is the live retention, and
//! `perf::counters().journal_entries` / `journal_bytes` are the process-wide
//! totals the acceptance tests assert on (ADR-017: counters, not durations).

use std::sync::Arc;

use rustc_hash::FxHashSet;
use stratum_proto::VarIdx;

use crate::chunk::StrLChunk;
use crate::column::{Column, ColumnRef};
use crate::perf::{bump, counters, drop_level};

/// One retained chunk, in its own width. No enum-of-`Vec<u8>` reinterpretation:
/// the saved value has the same type as the slot it goes back into, so a
/// restore cannot be a transmute that compiles.
#[derive(Clone, Debug)]
pub(crate) enum Saved {
    I8(Arc<[i8]>),
    I16(Arc<[i16]>),
    I32(Arc<[i32]>),
    F32(Arc<[f32]>),
    F64(Arc<[f64]>),
    Bytes(Arc<[u8]>),
    StrL(Arc<StrLChunk>),
}

impl Saved {
    fn bytes(&self) -> u64 {
        match self {
            Saved::I8(a) => a.len() as u64,
            Saved::I16(a) => (a.len() * 2) as u64,
            Saved::I32(a) => (a.len() * 4) as u64,
            Saved::F32(a) => (a.len() * 4) as u64,
            Saved::F64(a) => (a.len() * 8) as u64,
            Saved::Bytes(a) => a.len() as u64,
            Saved::StrL(a) => a.heap_bytes(),
        }
    }
}

/// Take the chunk `c` of `col` without copying it: one refcount bump.
///
/// The copy happens later and only if the command actually writes, because
/// `NumCol::chunk_mut`'s `Arc::make_mut` sees the strong count of 2 this
/// created. That ordering is the whole A18 mechanism.
pub(crate) fn retain(col: &Column, c: usize) -> Saved {
    match col {
        Column::Byte(x) => Saved::I8(x.chunk_arc(c)),
        Column::Int(x) => Saved::I16(x.chunk_arc(c)),
        Column::Long(x) => Saved::I32(x.chunk_arc(c)),
        Column::Float(x) => Saved::F32(x.chunk_arc(c)),
        Column::Double(x) => Saved::F64(x.chunk_arc(c)),
        Column::Str(x) => Saved::Bytes(x.chunk_arc(c)),
        Column::StrL(x) => Saved::StrL(x.chunk_arc(c)),
    }
}

fn restore(col: &mut Column, c: usize, saved: Saved) {
    match (col, saved) {
        (Column::Byte(x), Saved::I8(a)) => x.restore_chunk(c, a),
        (Column::Int(x), Saved::I16(a)) => x.restore_chunk(c, a),
        (Column::Long(x), Saved::I32(a)) => x.restore_chunk(c, a),
        (Column::Float(x), Saved::F32(a)) => x.restore_chunk(c, a),
        (Column::Double(x), Saved::F64(a)) => x.restore_chunk(c, a),
        (Column::Str(x), Saved::Bytes(a)) => x.restore_chunk(c, a),
        (Column::StrL(x), Saved::StrL(a)) => x.restore_chunk(c, a),
        // Unreachable in one direction only: a type promotion journals the
        // WHOLE column before it changes the variant, and rollback replays in
        // reverse, so the variant is always back before its chunks are.
        _ => unreachable!("a chunk was restored into a column of a different type"),
    }
}

/// What the journal is holding on to.
#[derive(Debug)]
enum Entry {
    Chunk {
        var: VarIdx,
        chunk: u32,
        saved: Saved,
    },
    /// A storage-type promotion or any other whole-column rewrite. One `Arc`.
    Column { var: VarIdx, saved: ColumnRef },
    /// A structural change to the variable list itself (`gen`, `drop`,
    /// `order`). The column vector is behind an `Arc`, so retaining the whole
    /// list is one pointer clone rather than `nvars` of them.
    Columns { saved: Arc<Vec<ColumnRef>> },
    /// A reordering (`sort`, `gsort`). ARCHITECTURE §7.6: "row-index changes
    /// still retain the previous index whole; it is one `Arc<[u32]>`, not
    /// per-column data." At 10 M rows that is 40 MB rather than the 1.2 GB a
    /// per-column retention of a sorted frame would cost, and undoing it is one
    /// more gather pass.
    RowOrder { inverse: Arc<[u32]> },
}

/// The per-command undo log.
#[derive(Debug, Default)]
pub struct Journal {
    entries: Vec<Entry>,
    /// `(var, chunk)` pairs already retained, so a command that writes a chunk
    /// a million times retains it once. Looked up on chunk *transitions* only —
    /// `ColMut` remembers the chunk it is currently inside — so this is one hash
    /// per 65 536 rows, not one per row.
    dirty: FxHashSet<(u32, u32)>,
    bytes: u64,
    open: bool,
}

impl Journal {
    /// An empty, closed journal.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Begin a command. Any un-committed retention from a previous command is
    /// dropped, which is the correct reading of "the command completed".
    pub fn begin(&mut self) {
        self.clear();
        self.open = true;
    }

    /// Is a command in flight?
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Retained entries — chunks plus whole-column rewrites.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when nothing has been retained.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Bytes this journal is holding alive right now.
    #[must_use]
    pub fn retained_bytes(&self) -> u64 {
        self.bytes
    }

    /// The command succeeded: let go.
    pub fn commit(&mut self) {
        self.clear();
    }

    fn clear(&mut self) {
        drop_level(&counters().journal_bytes, self.bytes);
        self.entries.clear();
        self.dirty.clear();
        self.bytes = 0;
        self.open = false;
    }

    /// Retain chunk `chunk` of `var` unless it is already retained.
    ///
    /// Returns `true` when this call is what retained it — the caller uses that
    /// to know a fresh `Arc::make_mut` copy is about to happen.
    pub(crate) fn note_chunk(&mut self, var: VarIdx, chunk: u32, col: &Column) -> bool {
        if !self.open || !self.dirty.insert((var.0, chunk)) {
            return false;
        }
        let saved = retain(col, chunk as usize);
        let n = saved.bytes();
        self.bytes += n;
        self.entries.push(Entry::Chunk { var, chunk, saved });
        bump(&counters().journal_entries, 1);
        bump(&counters().journal_bytes, n);
        true
    }

    /// Retain a whole column, for a rewrite that changes its type or length.
    pub(crate) fn note_column(&mut self, var: VarIdx, col: &ColumnRef) {
        if !self.open {
            return;
        }
        let n = col.heap_bytes();
        self.bytes += n;
        self.entries.push(Entry::Column {
            var,
            saved: Arc::clone(col),
        });
        // Every chunk-level retention for this variable is now redundant: the
        // whole column goes back in one move, and the replay is in reverse.
        self.dirty.retain(|&(v, _)| v != var.0);
        bump(&counters().journal_entries, 1);
        bump(&counters().journal_bytes, n);
    }

    /// Retain the whole column list before a structural change.
    pub(crate) fn note_columns(&mut self, cols: &Arc<Vec<ColumnRef>>) {
        if !self.open {
            return;
        }
        self.entries.push(Entry::Columns {
            saved: Arc::clone(cols),
        });
        bump(&counters().journal_entries, 1);
    }

    /// Retain the inverse of a reordering the frame is about to apply.
    ///
    /// Every chunk retained so far refers to the *pre-sort* row layout, and
    /// replaying in reverse restores those chunks before this entry is undone,
    /// so the layouts always line up.
    pub(crate) fn note_row_order(&mut self, inverse: Arc<[u32]>) {
        if !self.open {
            return;
        }
        let n = (inverse.len() * 4) as u64;
        self.bytes += n;
        self.entries.push(Entry::RowOrder { inverse });
        bump(&counters().journal_entries, 1);
        bump(&counters().journal_bytes, n);
    }

    /// Put every retained chunk and column back, newest first.
    ///
    /// Reverse order is required, not cosmetic: a command that promotes a
    /// column and then writes chunks of the promoted column must have the
    /// chunk restores undone before the variant changes back underneath them.
    pub(crate) fn rollback_into(&mut self, cols: &mut Arc<Vec<ColumnRef>>) {
        while let Some(entry) = self.entries.pop() {
            match entry {
                Entry::Chunk { var, chunk, saved } => {
                    let slot = &mut Arc::make_mut(cols)[var.0 as usize];
                    restore(Arc::make_mut(slot), chunk as usize, saved);
                }
                Entry::Column { var, saved } => {
                    Arc::make_mut(cols)[var.0 as usize] = saved;
                }
                Entry::Columns { saved } => {
                    *cols = saved;
                }
                Entry::RowOrder { inverse } => {
                    let live: &mut Vec<ColumnRef> = Arc::make_mut(cols);
                    crate::sort::permute_all(live, &inverse);
                }
            }
        }
        self.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::column::NumCol;

    #[test]
    fn a_chunk_is_retained_once_however_often_it_is_written() {
        let col = Column::Double(NumCol::from_slice(&[1.0f64; 10]));
        let mut j = Journal::new();
        j.begin();
        assert!(j.note_chunk(VarIdx(0), 0, &col));
        assert!(!j.note_chunk(VarIdx(0), 0, &col));
        assert!(!j.note_chunk(VarIdx(0), 0, &col));
        assert_eq!(j.len(), 1);
        assert_eq!(j.retained_bytes(), 80);
    }

    #[test]
    fn a_closed_journal_retains_nothing() {
        // Reads and non-rollbackable commands must not pay for a journal.
        let col = Column::Double(NumCol::from_slice(&[1.0f64; 10]));
        let mut j = Journal::new();
        assert!(!j.note_chunk(VarIdx(0), 0, &col));
        assert_eq!(j.len(), 0);
        assert_eq!(j.retained_bytes(), 0);
    }

    #[test]
    fn commit_gives_the_bytes_back() {
        let col = Column::Double(NumCol::from_slice(&[1.0f64; 10]));
        let mut j = Journal::new();
        j.begin();
        j.note_chunk(VarIdx(0), 0, &col);
        assert_eq!(j.retained_bytes(), 80);
        j.commit();
        assert_eq!(j.retained_bytes(), 0);
        assert!(!j.is_open());
    }
}
