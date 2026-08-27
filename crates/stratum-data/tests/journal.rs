//! W02 acceptance: **journal cost is proportional to work** (A18), and rollback
//! is bit-identical (INV-2).
//!
//! ADR-017 is binding here: every gate below asserts a *counter* — journal
//! entries, retained bytes, allocation count, largest single allocation — and
//! never a duration. Durations are recorded in `benches/journal.rs`.
//!
//! # Why this file installs a global allocator
//!
//! The acceptance bullet says "asserted by counting allocations". Counting them
//! requires a `#[global_allocator]`, which requires `unsafe impl GlobalAlloc`.
//! `src/lib.rs` carries `#![forbid(unsafe_code)]` so the library is under the
//! same rule as `stratum-core`; the package lint is `deny`, not `forbid`,
//! precisely so this test binary can opt in. An unprovable gate is the failure
//! mode ADR-017 exists to stop, so the opt-in is the lesser evil and it is
//! confined to test code that never links into a shipping binary.
//!
//! Counters are **thread-local**, so the other tests in this binary running in
//! parallel cannot pollute a measurement.
#![allow(unsafe_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use stratum_core::missing::{is_missing, SYSMISS};
use stratum_data::{Frame, StorageType, CHUNK_ROWS};
use stratum_proto::VarIdx;

// ---------------------------------------------------------------------------
// The instrument
// ---------------------------------------------------------------------------

thread_local! {
    static N_ALLOC: Cell<u64> = const { Cell::new(0) };
    static N_BYTES: Cell<u64> = const { Cell::new(0) };
    static MAX_BYTES: Cell<u64> = const { Cell::new(0) };
}

struct Counting;

fn note(size: usize) {
    let n = size as u64;
    let _ = N_ALLOC.try_with(|c| c.set(c.get() + 1));
    let _ = N_BYTES.try_with(|c| c.set(c.get() + n));
    let _ = MAX_BYTES.try_with(|c| c.set(c.get().max(n)));
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        note(l.size());
        System.alloc(l)
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        note(l.size());
        System.alloc_zeroed(l)
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        note(new);
        System.realloc(p, l, new)
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        System.dealloc(p, l);
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// What one measured region allocated.
#[derive(Clone, Copy, Debug, Default)]
struct Allocs {
    count: u64,
    bytes: u64,
    largest: u64,
}

/// Run `f`, reporting what it allocated on this thread.
fn measure<T>(f: impl FnOnce() -> T) -> (T, Allocs) {
    N_ALLOC.with(|c| c.set(0));
    N_BYTES.with(|c| c.set(0));
    MAX_BYTES.with(|c| c.set(0));
    let out = f();
    let a = Allocs {
        count: N_ALLOC.with(Cell::get),
        bytes: N_BYTES.with(Cell::get),
        largest: MAX_BYTES.with(Cell::get),
    };
    (out, a)
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// The dataset every A18 bullet is written against: one `double` column of
/// 10 million observations. 153 chunks, 80 MB.
const N: u64 = 10_000_000;
const CHUNK_BYTES: u64 = CHUNK_ROWS as u64 * 8;
const N_CHUNKS: u64 = 153;

fn ten_million_doubles() -> Frame {
    let mut f = Frame::new("default");
    f.set_n_obs(N);
    f.add_var("x", StorageType::Double).expect("fresh name");
    // Give it real values so a rollback has something to be wrong about.
    f.begin_command();
    {
        let mut c = f.col_mut(VarIdx(0)).expect("exists");
        for ch in 0..c.n_chunks() {
            c.with_double_chunk(ch, |first, xs| {
                for (i, v) in xs.iter_mut().enumerate() {
                    *v = (first + i as u64) as f64;
                }
            });
        }
    }
    f.commit();
    f
}

// ---------------------------------------------------------------------------
// The acceptance
// ---------------------------------------------------------------------------

#[test]
fn the_chunk_arithmetic_is_the_one_the_bullets_assume() {
    assert_eq!(CHUNK_ROWS, 65_536);
    assert_eq!(N.div_ceil(CHUNK_ROWS as u64), N_CHUNKS);
    assert_eq!(CHUNK_BYTES, 524_288, "one chunk of doubles is 512 KiB");
}

#[test]
fn replace_in_1_on_ten_million_rows_journals_exactly_one_chunk() {
    let mut f = ten_million_doubles();
    let before = f.digest(VarIdx(0)).expect("column");

    let (_, a) = measure(|| {
        f.begin_command();
        f.col_mut(VarIdx(0))
            .expect("exists")
            .set_f64(0, 1.0)
            .expect("a double takes any value");
    });

    // THE bullet: one chunk, 512 KiB, whatever the column's length.
    assert_eq!(f.journal().len(), 1, "entries");
    assert_eq!(f.journal().retained_bytes(), CHUNK_BYTES, "retained bytes");

    // And the allocation side of it: the command copied one chunk and nothing
    // that scales with the column. The pre-A18 design allocated 80 MB here.
    assert!(
        a.bytes < CHUNK_BYTES + 64 * 1024,
        "allocated {} bytes for a one-observation replace; one chunk is {CHUNK_BYTES}",
        a.bytes
    );
    assert!(
        a.largest <= CHUNK_BYTES + 64,
        "largest single allocation was {}",
        a.largest
    );
    assert!(
        a.count < 32,
        "a one-observation replace made {} allocations",
        a.count
    );

    f.rollback();
    assert_eq!(
        f.digest(VarIdx(0)).expect("column"),
        before,
        "rollback must restore the column bit for bit"
    );
    assert_eq!(f.col(VarIdx(0)).expect("column").get_f64(0), Some(0.0));
}

#[test]
fn a_full_column_replace_journals_one_entry_per_chunk_and_allocates_chunk_wise() {
    let mut f = ten_million_doubles();
    let before = f.digest(VarIdx(0)).expect("column");

    let (_, a) = measure(|| {
        f.begin_command();
        let mut c = f.col_mut(VarIdx(0)).expect("exists");
        for ch in 0..c.n_chunks() {
            c.with_double_chunk(ch, |_, xs| {
                for v in xs.iter_mut() {
                    *v += 1.0;
                }
            });
        }
    });

    assert_eq!(f.journal().len(), N_CHUNKS as usize, "ceil(n / CHUNK_ROWS)");
    assert_eq!(f.journal().retained_bytes(), N * 8);

    // "does NOT allocate a second whole column beyond the chunk in flight":
    // no single allocation is bigger than one chunk, and the total is one
    // column's worth of chunks plus bookkeeping — not two.
    assert!(
        a.largest <= CHUNK_BYTES + 64,
        "largest single allocation was {} bytes; a chunk is {CHUNK_BYTES}",
        a.largest
    );
    let one_column = N * 8;
    assert!(
        a.bytes < one_column + one_column / 16,
        "allocated {} bytes rewriting a {one_column}-byte column",
        a.bytes
    );
    // One allocation per chunk made unique, plus the journal's own growth: the
    // count is O(chunks), never O(rows).
    assert!(
        a.count < N_CHUNKS * 2,
        "{} allocations for {N_CHUNKS} chunks",
        a.count
    );

    assert_eq!(f.col(VarIdx(0)).expect("column").get_f64(0), Some(1.0));
    f.rollback();
    assert_eq!(
        f.digest(VarIdx(0)).expect("column"),
        before,
        "every dirtied chunk must come back"
    );
}

#[test]
fn an_interrupted_replace_restores_every_dirtied_chunk() {
    // The INV-2 case: a command that got part way through and then failed.
    let mut f = ten_million_doubles();
    let before = f.digest(VarIdx(0)).expect("column");

    f.begin_command();
    {
        let mut c = f.col_mut(VarIdx(0)).expect("exists");
        // Dirty a scattered subset, including the short tail chunk.
        for ch in [0usize, 7, 42, 100, 152] {
            c.with_double_chunk(ch, |_, xs| {
                for v in xs.iter_mut() {
                    *v = SYSMISS;
                }
            });
        }
    }
    assert_eq!(f.journal().len(), 5);
    assert!(is_missing(
        f.col(VarIdx(0))
            .expect("column")
            .get_f64(0)
            .expect("numeric")
    ));

    f.rollback();
    assert_eq!(f.digest(VarIdx(0)).expect("column"), before);
    assert_eq!(f.journal().len(), 0, "rollback releases the retention");
    assert_eq!(
        f.col(VarIdx(0)).expect("column").get_f64(7 * 65_536),
        Some(458_752.0)
    );
}

#[test]
fn writing_one_chunk_a_million_times_retains_it_once() {
    let mut f = ten_million_doubles();
    f.begin_command();
    {
        let mut c = f.col_mut(VarIdx(0)).expect("exists");
        for row in 0..100_000u64 {
            c.set_f64(row, 0.0).expect("double");
        }
    }
    // 100 000 rows spans chunks 0 and 1 and nothing else.
    assert_eq!(f.journal().len(), 2);
    assert_eq!(f.journal().retained_bytes(), 2 * CHUNK_BYTES);
}

#[test]
fn a_command_that_is_not_rollbackable_retains_nothing() {
    // Outside begin_command the journal is closed, so the bulk load path pays
    // no retention at all — which is what makes `use` a straight write.
    let mut f = ten_million_doubles();
    let (_, a) = measure(|| {
        f.col_mut(VarIdx(0))
            .expect("exists")
            .set_f64(0, 5.0)
            .expect("double");
    });
    assert_eq!(f.journal().len(), 0);
    assert_eq!(
        a.count, 0,
        "a write with nothing to retain must not allocate"
    );
    assert!(
        a.bytes < 4096,
        "a closed journal allocated {} bytes",
        a.bytes
    );
    assert_eq!(f.col(VarIdx(0)).expect("column").get_f64(0), Some(5.0));
}

#[test]
fn rollback_restores_a_promotion_and_the_writes_that_followed_it() {
    let mut f = Frame::new("default");
    f.set_n_obs(4);
    let b = f.add_var("b", StorageType::Byte).expect("fresh");
    f.begin_command();
    f.col_mut(b).expect("exists").set_f64(0, 7.0).expect("fits");
    f.commit();
    let before = f.digest(b).expect("column");
    let ty_before = f.var(b).expect("variable").ty;

    f.begin_command();
    // `replace b = 40000` — Stata promotes rather than erroring (measured).
    assert!(f.col_mut(b).expect("exists").set_f64(0, 40_000.0).is_err());
    f.recast_var(b, StorageType::Long).expect("exists");
    f.col_mut(b)
        .expect("exists")
        .set_f64(0, 40_000.0)
        .expect("fits a long");
    assert_eq!(f.var(b).expect("variable").ty, StorageType::Long);

    f.rollback();
    assert_eq!(f.var(b).expect("variable").ty, ty_before, "type comes back");
    assert_eq!(f.digest(b).expect("column"), before, "and so do the bytes");
}

#[test]
fn rollback_undoes_a_sort_without_retaining_the_columns() {
    let mut f = Frame::new("default");
    f.set_n_obs(6);
    let x = f.add_var("x", StorageType::Long).expect("fresh");
    f.begin_command();
    {
        let mut c = f.col_mut(x).expect("exists");
        for (row, v) in [5.0, 2.0, 9.0, 2.0, -1.0, 4.0].into_iter().enumerate() {
            c.set_f64(row as u64, v).expect("long");
        }
    }
    f.commit();
    let before = f.digest(x).expect("column");

    f.begin_command();
    f.sort_by(&[(x, stratum_proto::SortDir::Asc)])
        .expect("sortable");
    assert_eq!(f.col(x).expect("column").get_f64(0), Some(-1.0));
    // One entry: the inverse permutation. Not one entry per column.
    assert_eq!(f.journal().len(), 1);
    assert_eq!(f.journal().retained_bytes(), 6 * 4);

    f.rollback();
    assert_eq!(f.digest(x).expect("column"), before);
    assert_eq!(f.col(x).expect("column").get_f64(0), Some(5.0));
}

#[test]
fn rollback_undoes_a_structural_change() {
    let mut f = Frame::new("default");
    f.set_n_obs(3);
    f.add_var("a", StorageType::Byte).expect("fresh");
    f.commit();

    f.begin_command();
    f.add_var("b", StorageType::Double).expect("fresh");
    f.drop_var(VarIdx(0)).expect("exists");
    assert_eq!(f.n_vars(), 1);
    assert_eq!(f.index_of("b"), Some(VarIdx(0)));

    f.rollback();
    assert_eq!(f.n_vars(), 1, "only `a` existed at entry");
    assert_eq!(f.index_of("a"), Some(VarIdx(0)));
    assert_eq!(f.index_of("b"), None);
}
