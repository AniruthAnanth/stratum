//! W02 acceptance: **`Arc` copy-on-write**.
//!
//! The bullet reads "`frame copy` of a 10 M × 20 frame allocates O(nvars) and is
//! < 1 ms". Per ADR-017 the duration is **recorded, not asserted** — the same
//! unchanged tree measured 105 MiB/s and 76 MiB/s an hour apart on this class of
//! machine — and what is asserted is the counter that expresses the same
//! property, more strictly than the bullet asked:
//!
//! * `frame copy` allocates a **fixed small number of blocks**, identical at
//!   1 000 × 5, 100 000 × 20 and 10 000 000 × 20. Not O(nvars); O(1) in both
//!   dimensions, because every shareable field is one `Arc`.
//! * `Frame::snapshot` allocates **zero**.
//! * A write after a copy or a snapshot duplicates exactly one chunk, and the
//!   other holder's bytes do not move.
#![allow(unsafe_code)] // see tests/journal.rs — the counting allocator.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::time::Instant;

use stratum_data::{Frame, StorageType, CHUNK_ROWS};
use stratum_proto::VarIdx;

thread_local! {
    static N_ALLOC: Cell<u64> = const { Cell::new(0) };
    static N_BYTES: Cell<u64> = const { Cell::new(0) };
}

struct Counting;

fn note(size: usize) {
    let _ = N_ALLOC.try_with(|c| c.set(c.get() + 1));
    let _ = N_BYTES.try_with(|c| c.set(c.get() + size as u64));
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

fn measure<T>(f: impl FnOnce() -> T) -> (T, u64, u64) {
    N_ALLOC.with(|c| c.set(0));
    N_BYTES.with(|c| c.set(0));
    let out = f();
    (out, N_ALLOC.with(Cell::get), N_BYTES.with(Cell::get))
}

/// `nvars` byte columns of `nobs` observations.
///
/// Byte, not double, and that is not a shortcut: a copy that moves no cells
/// cannot care how wide a cell is, so the storage width is exactly the variable
/// the property is independent of. It also keeps the 10 M × 20 case to 200 MB
/// rather than 1.6 GB, on a machine that is also running the rest of the suite.
fn frame(nobs: u64, nvars: u32) -> Frame {
    let mut f = Frame::new("default");
    f.set_n_obs(nobs);
    for i in 0..nvars {
        f.add_var(&format!("v{i}"), StorageType::Byte)
            .expect("fresh name");
    }
    f
}

#[test]
fn frame_copy_allocation_does_not_depend_on_rows_or_columns() {
    let small = frame(1_000, 5);
    let medium = frame(100_000, 20);

    let (_, a_small, _) = measure(|| small.copy("small_copy"));
    let (_, a_medium, _) = measure(|| medium.copy("medium_copy"));

    assert_eq!(
        a_small, a_medium,
        "a copy that shares every column cannot allocate more for a bigger frame"
    );
    assert!(
        a_small <= 8,
        "frame copy made {a_small} allocations; it should be a handful of Arc bumps"
    );
}

#[test]
fn frame_copy_of_ten_million_by_twenty_is_pointer_work() {
    let f = frame(10_000_000, 20);
    let resident = f.resident_bytes();
    assert!(
        resident >= 200_000_000,
        "the fixture should really be 200 MB, not {resident}"
    );

    let t = Instant::now();
    let (copy, allocs, bytes) = measure(|| f.copy("backup"));
    let elapsed = t.elapsed();

    // ASSERTED (ADR-017): the counter.
    assert!(
        allocs <= 8,
        "frame copy of a 10 M x 20 frame made {allocs} allocations"
    );
    assert!(
        bytes < 4_096,
        "frame copy allocated {bytes} bytes for a {resident}-byte frame"
    );
    assert_eq!(copy.n_vars(), 20);
    assert_eq!(copy.n_obs(), 10_000_000);

    // RECORDED, never asserted: wall clock on a shared developer machine is not
    // a gate-grade instrument (ADR-017 finding 2).
    eprintln!(
        "recorded: frame copy 10Mx20 ({resident} bytes resident) in {elapsed:?}, \
         {allocs} allocations, {bytes} bytes"
    );
}

#[test]
fn a_snapshot_allocates_nothing_at_all() {
    let f = frame(1_000_000, 12);
    let (snap, allocs, bytes) = measure(|| f.snapshot());
    assert_eq!(allocs, 0, "a snapshot is five pointer clones");
    assert_eq!(bytes, 0);
    assert_eq!(snap.n_vars(), 12);
    assert_eq!(snap.n_obs(), 1_000_000);
    assert_eq!(snap.version(), f.version());
}

#[test]
fn writing_after_a_snapshot_duplicates_one_chunk_not_one_column() {
    let mut f = Frame::new("default");
    f.set_n_obs(1_000_000); // 16 chunks
    f.add_var("x", StorageType::Double).expect("fresh name");
    let snap = f.snapshot();
    let before = snap.col(VarIdx(0)).expect("column").digest();

    // No `begin_command`: the journal is closed, so the ONLY reason a chunk
    // gets duplicated here is the live snapshot. That isolates the CoW cost.
    let (_, allocs, bytes) = measure(|| {
        f.col_mut(VarIdx(0))
            .expect("exists")
            .set_f64(0, 42.0)
            .expect("double");
    });

    let chunk_bytes = (CHUNK_ROWS * 8) as u64;
    assert!(
        bytes < chunk_bytes * 2,
        "writing one cell behind a snapshot allocated {bytes} bytes; a chunk is {chunk_bytes}"
    );
    // The handful: `Arc<Vec<ColumnRef>>`, `Arc<Vec<Variable>>` and
    // `Arc<Column>` each become unique (three `Vec`s of POINTERS), and one
    // chunk is duplicated. Nothing here grows with the column.
    assert!(allocs <= 8, "{allocs} allocations");

    assert_eq!(
        snap.col(VarIdx(0)).expect("column").digest(),
        before,
        "the snapshot must not see the write"
    );
    assert_eq!(f.col(VarIdx(0)).expect("column").get_f64(0), Some(42.0));
    assert!(stratum_core::is_missing(
        snap.col(VarIdx(0))
            .expect("column")
            .get_f64(0)
            .expect("numeric")
    ));
}

#[test]
fn two_frames_sharing_a_column_diverge_only_where_written() {
    let mut a = Frame::new("default");
    a.set_n_obs(200_000); // 4 chunks
    a.add_var("x", StorageType::Double).expect("fresh name");
    {
        let mut c = a.col_mut(VarIdx(0)).expect("exists");
        for ch in 0..c.n_chunks() {
            c.with_double_chunk(ch, |first, xs| {
                for (i, v) in xs.iter_mut().enumerate() {
                    *v = (first + i as u64) as f64;
                }
            });
        }
    }
    let b = a.copy("backup");
    let b_before = b.col(VarIdx(0)).expect("column").digest();

    // Write into chunk 2 only.
    a.col_mut(VarIdx(0))
        .expect("exists")
        .set_f64(150_000, -1.0)
        .expect("double");

    assert_eq!(
        b.col(VarIdx(0)).expect("column").digest(),
        b_before,
        "the copy is untouched"
    );
    assert_eq!(
        b.col(VarIdx(0)).expect("column").get_f64(150_000),
        Some(150_000.0)
    );
    assert_eq!(
        a.col(VarIdx(0)).expect("column").get_f64(150_000),
        Some(-1.0)
    );
    // The chunks nobody wrote are still literally the same allocation.
    assert_eq!(
        a.col(VarIdx(0)).expect("column").get_f64(0),
        b.col(VarIdx(0)).expect("column").get_f64(0)
    );
}

#[test]
fn preserve_and_restore_are_snapshot_and_swap() {
    // `04` §3.2: preserve/restore become O(nvars) pointer clones instead of
    // Stata's temp-file dance.
    let mut f = Frame::new("default");
    f.set_n_obs(50_000);
    f.add_var("x", StorageType::Long).expect("fresh name");
    f.col_mut(VarIdx(0))
        .expect("exists")
        .set_f64(0, 7.0)
        .expect("long");

    let (preserved, allocs, _) = measure(|| f.snapshot());
    assert_eq!(allocs, 0);

    f.col_mut(VarIdx(0))
        .expect("exists")
        .set_f64(0, 8.0)
        .expect("long");
    assert_eq!(f.col(VarIdx(0)).expect("column").get_f64(0), Some(8.0));
    assert_eq!(
        preserved.col(VarIdx(0)).expect("column").get_f64(0),
        Some(7.0)
    );
}

// ---------------------------------------------------------------------------
// W02 acceptance bullet 1: "`Frame::col_mut` is the **only** path to a mutable
// column; a compile-fail test (`trybuild`) proves raw buffers are unreachable
// from outside the crate."
//
// DEVIATION, declared: the mechanism is not `trybuild`. `trybuild` is not in the
// workspace dependency table, and adding it would edit `Cargo.toml` and
// `Cargo.lock` — both W00's under R0 — while its fixtures live in
// `tests/ui/*.rs` + `*.stderr`, paths no unit owns. What `trybuild` actually
// does is spawn the compiler on a snippet outside the crate and match the
// diagnostic; that is spawned here directly, with no new dependency and no file
// this unit does not own.
//
// This is a strictly stronger check than the `compile_fail` doctests in
// `src/lib.rs`, and the difference matters: rustdoc's `compile_fail` passes when
// the snippet fails to compile for ANY reason, and its `,E0616` error-code
// annotation is NOT enforced on stable — verified on rustc 1.96.0, where a
// snippet whose real error is E0308 passed a block tagged `compile_fail,E0616`.
// So the doctests prove "does not compile" and this test proves "does not
// compile *because the buffer is private*", which is the property the bullet is
// about. Both are kept; neither subsumes the other.
// ---------------------------------------------------------------------------

/// `(crate name, the diagnostic that must appear, source)`. `None` marks the
/// positive control.
///
/// The control is load-bearing, not decoration. A mis-wired probe — wrong `-L`,
/// a stale rlib, no `rustc` on `PATH` — makes every snippet fail to compile,
/// and a suite that only asserts failure would then report the write barrier as
/// proven having compiled nothing at all. The control turns that into a red
/// test.
const BARRIER_PROBES: &[(&str, Option<&str>, &str)] = &[
    (
        "probe_private_chunk_vec",
        Some("E0616"), // field is private
        r#"
        pub fn probe() {
            let c = stratum_data::column::NumCol::<f64>::missing(10);
            let _ = c.chunks;
        }
        "#,
    ),
    (
        "probe_private_chunk_mut",
        Some("E0624"), // method is private
        r#"
        pub fn probe() {
            let mut c = stratum_data::column::NumCol::<f64>::missing(10);
            let _ = c.chunk_mut(0);
        }
        "#,
    ),
    (
        "probe_snapshot_has_no_col_mut",
        Some("E0599"), // no method named `col_mut`
        r#"
        pub fn probe() {
            let mut f = stratum_data::Frame::new("default");
            f.set_n_obs(4);
            f.add_var("x", stratum_data::StorageType::Double).unwrap();
            let snap = f.snapshot();
            let i = snap.index_of("x").unwrap();
            snap.col_mut(i);
        }
        "#,
    ),
    (
        "probe_barrier_control",
        None,
        r#"
        pub fn probe() {
            let mut f = stratum_data::Frame::new("default");
            f.set_n_obs(4);
            f.add_var("x", stratum_data::StorageType::Double).unwrap();
            let i = f.index_of("x").unwrap();
            f.begin_command();
            f.col_mut(i).unwrap().set_f64(0, 1.5).unwrap();
            f.rollback();
        }
        "#,
    ),
];

/// The newest `<stem>*.rlib` cargo has built into this test binary's `deps`
/// directory.
///
/// Newest-wins because a `--all-features` build and a default build leave two
/// hashes side by side; the barrier is a privacy property and privacy does not
/// vary with a feature flag, so either answers the question. If the pick is
/// wrong in some way this reasoning missed, the positive control stops
/// compiling and the test says so.
fn newest_rlib(deps: &std::path::Path, stem: &str) -> std::path::PathBuf {
    let mut best: Option<(std::time::SystemTime, std::path::PathBuf)> = None;
    for entry in std::fs::read_dir(deps)
        .expect("deps directory is readable")
        .flatten()
    {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !(name.starts_with(stem) && name.ends_with(".rlib")) {
            continue;
        }
        let stamp = match entry.metadata().and_then(|m| m.modified()) {
            Ok(t) => t,
            Err(_) => continue,
        };
        let newer = match &best {
            Some((t, _)) => stamp > *t,
            None => true,
        };
        if newer {
            best = Some((stamp, entry.path()));
        }
    }
    match best {
        Some((_, p)) => p,
        None => panic!("no {stem}*.rlib under {}", deps.display()),
    }
}

#[test]
fn the_write_barrier_is_the_only_route_to_a_mutable_column() {
    let exe = std::env::current_exe().expect("this test binary has a path");
    let deps = exe.parent().expect("deps directory").to_path_buf();
    let data = newest_rlib(&deps, "libstratum_data-");

    let scratch = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("write-barrier");
    std::fs::create_dir_all(&scratch).expect("scratch directory");

    // `rustc` unless cargo names a different driver; matches how cargo itself
    // resolves the compiler, so a `RUSTC` shim in CI is honoured.
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());

    for (name, expected, src) in BARRIER_PROBES {
        let file = scratch.join(format!("{name}.rs"));
        std::fs::write(&file, src).expect("write probe source");

        // `--emit=metadata`: these are resolution and privacy diagnostics, all
        // of them raised before codegen, so there is no reason to pay for a
        // link. `--crate-type=lib` keeps the probe free of a `main`.
        let out = std::process::Command::new(&rustc)
            .arg("--edition=2021")
            .arg("--crate-type=lib")
            .arg("--emit=metadata")
            .args(["--crate-name", name])
            .arg(&file)
            .arg("-L")
            .arg(format!("dependency={}", deps.display()))
            .arg("--extern")
            .arg(format!("stratum_data={}", data.display()))
            .arg("--out-dir")
            .arg(&scratch)
            .output()
            .expect("spawn rustc");

        let stderr = String::from_utf8_lossy(&out.stderr);
        match expected {
            None => assert!(
                out.status.success(),
                "the positive control must compile, so that the three probes \
                 below it are known to have been compiled at all.\n\
                 rustc said:\n{stderr}"
            ),
            Some(code) => {
                assert!(
                    !out.status.success(),
                    "{name} compiled; a raw buffer is reachable from outside the crate"
                );
                assert!(
                    stderr.contains(code),
                    "{name} failed, but not with {code} — so it did not fail for \
                     the reason the barrier is supposed to enforce.\n\
                     rustc said:\n{stderr}"
                );
            }
        }
    }
}
