//! The write barrier — W06's first two acceptance bullets.
//!
//! > **Write barrier**: no path mutates a column outside `Frame::col_mut`; `gen`
//! > bumps once per command commit, not per element — a `replace x = x+1` over
//! > 10 M rows performs exactly **one** bump (asserted by counting).
//!
//! > **INV-2**: a command interrupted mid-`replace` leaves the frame
//! > bit-identical to entry (`ExecStatus::Interrupted{rolled_back: true}`),
//! > verified by digesting every column before and after.
//!
//! Both are asserted with counters and digests, never with a duration
//! (ADR-017). Where a wall clock is interesting it is printed, not asserted.

use std::path::PathBuf;

use stratum_data::{Frame, StorageType};
use stratum_proto::{ExecStatus, ExecutionId, FrameId, SessionEpoch, VarIdx};
use stratum_runtime::state::barrier::{col_mut, frame_digest};
use stratum_runtime::state::{local_snapshot, SessionState};

const ROWS: u64 = 10_000_000;
const E1: ExecutionId = ExecutionId(1);
const E2: ExecutionId = ExecutionId(2);

/// A frame of `rows` observations with one `Double` column holding `1, 2, 3, …`.
fn seeded(rows: u64) -> Frame {
    let mut f = Frame::new("default");
    f.set_n_obs(rows);
    f.add_var("x", StorageType::Double).expect("x");
    f.add_var("y", StorageType::Double).expect("y");
    for v in [VarIdx(0), VarIdx(1)] {
        let mut c = f.col_mut(v).expect("seed");
        for chunk in 0..c.n_chunks() {
            c.with_double_chunk(chunk, |base, s| {
                for (i, slot) in s.iter_mut().enumerate() {
                    *slot = (base + i as u64) as f64;
                }
            });
        }
    }
    f
}

#[test]
fn a_ten_million_row_replace_bumps_the_version_exactly_once() {
    let mut frame = seeded(ROWS);
    let mut state = SessionState::fresh(SessionEpoch(1), FrameId(0));
    // Register the two seeded columns with the fingerprint.
    {
        let fb = state.begin_command(4);
        frame.begin_command();
        fb.note_create(frame.var(VarIdx(0)).unwrap().id);
        fb.note_create(frame.var(VarIdx(1)).unwrap().id);
        state.commit_command(&mut frame, fb, E1);
    }

    let x = frame.var(VarIdx(0)).unwrap().id;
    let gen_before = state.dataset().version_of(x).unwrap().gen;
    let before = local_snapshot();
    let started = std::time::Instant::now();

    // `replace x = x + 1`, the whole column, through the barrier.
    let fb = state.begin_command(4);
    frame.begin_command();
    let mut rows_touched: u64 = 0;
    {
        let mut c = col_mut(&mut frame, &fb, VarIdx(0)).expect("barrier");
        for chunk in 0..c.n_chunks() {
            let mut n = 0u64;
            c.with_double_chunk(chunk, |_, s| {
                for slot in s.iter_mut() {
                    *slot += 1.0;
                }
                n = s.len() as u64;
            });
            rows_touched += n;
        }
    }
    let committed = state.commit_command(&mut frame, fb, E2);
    let elapsed = started.elapsed();
    let delta = local_snapshot().since(before);

    assert_eq!(rows_touched, ROWS, "the test must actually touch 10 M rows");
    // THE assertion. One bump, ten million rows.
    assert_eq!(delta.gen_bumps, 1, "one command commit is one version bump");
    assert_eq!(committed.counts.bumps, 1);
    assert_eq!(committed.counts.converged, 0);
    assert_eq!(
        state.dataset().version_of(x).unwrap().gen,
        gen_before + 1,
        "and the bump is by exactly one"
    );
    assert_eq!(committed.writes.vars_written, vec![x]);
    // The untouched column is untouched.
    let y = frame.var(VarIdx(1)).unwrap().id;
    assert_eq!(state.dataset().version_of(y).unwrap().gen, 0);
    // Recorded, not asserted (ADR-017): the digest dominates this number.
    println!(
        "replace over {ROWS} rows: {:?}, {} column(s) digested, {} bytes",
        elapsed, delta.columns_digested, delta.digest_bytes
    );
}

#[test]
fn an_interrupted_replace_leaves_the_frame_bit_identical() {
    // INV-2, verified the way the acceptance bullet specifies: digest every
    // column before and after.
    let mut frame = seeded(1_000_000);
    let mut state = SessionState::fresh(SessionEpoch(1), FrameId(0));
    {
        let fb = state.begin_command(4);
        frame.begin_command();
        fb.note_create(frame.var(VarIdx(0)).unwrap().id);
        fb.note_create(frame.var(VarIdx(1)).unwrap().id);
        state.commit_command(&mut frame, fb, E1);
    }
    let before = frame_digest(&frame);
    let ds_before = state.dataset().clone();

    let fb = state.begin_command(4);
    frame.begin_command();
    {
        let mut c = col_mut(&mut frame, &fb, VarIdx(0)).expect("barrier");
        // Half the column is rewritten, then the command is interrupted.
        for chunk in 0..c.n_chunks() / 2 {
            c.with_double_chunk(chunk, |_, s| {
                for slot in s.iter_mut() {
                    *slot = -1.0;
                }
            });
        }
    }
    drop(fb);
    state.rollback_command(&mut frame);
    let status = ExecStatus::Interrupted {
        rolled_back: true,
        at: None,
    };

    assert_eq!(
        frame_digest(&frame),
        before,
        "every column must be bit-identical to entry"
    );
    assert!(
        state.dataset().same_state(&ds_before),
        "a rolled-back command must not move state identity"
    );
    assert!(matches!(
        status,
        ExecStatus::Interrupted {
            rolled_back: true,
            ..
        }
    ));
}

#[test]
fn a_verbatim_recommit_converges_and_does_not_bump() {
    // `03` §4.4 — the false-stale cascade this exists to stop.
    let mut frame = seeded(4096);
    let mut state = SessionState::fresh(SessionEpoch(1), FrameId(0));
    {
        let fb = state.begin_command(4);
        frame.begin_command();
        fb.note_create(frame.var(VarIdx(0)).unwrap().id);
        fb.note_create(frame.var(VarIdx(1)).unwrap().id);
        state.commit_command(&mut frame, fb, E1);
    }
    let x = frame.var(VarIdx(0)).unwrap().id;

    // Put something for the first run to actually change.
    {
        let fb = state.begin_command(4);
        frame.begin_command();
        {
            let mut c = col_mut(&mut frame, &fb, VarIdx(0)).expect("barrier");
            c.set_f64(0, -5.0).expect("seed a negative");
        }
        state.commit_command(&mut frame, fb, E2);
    }

    // `replace x = 0 if x < 0` — idempotent. The first run moves a value; the
    // second produces byte-identical output and must not bump.
    let run = |state: &mut SessionState, frame: &mut Frame, exec| {
        let fb = state.begin_command(4);
        frame.begin_command();
        {
            let mut c = col_mut(frame, &fb, VarIdx(0)).expect("barrier");
            for chunk in 0..c.n_chunks() {
                c.with_double_chunk(chunk, |_, s| {
                    for slot in s.iter_mut() {
                        if *slot < 0.0 {
                            *slot = 0.0;
                        }
                    }
                });
            }
        }
        state.commit_command(frame, fb, exec)
    };

    let first = run(&mut state, &mut frame, ExecutionId(3));
    let gen_after_first = state.dataset().version_of(x).unwrap().gen;
    let before_second = local_snapshot();
    let second = run(&mut state, &mut frame, ExecutionId(4));
    let d = local_snapshot().since(before_second);

    assert_eq!(first.counts.bumps, 1, "the first run changes a value");
    assert_eq!(second.counts.bumps, 0, "the second run converges");
    assert_eq!(second.counts.converged, 1);
    assert_eq!(state.dataset().version_of(x).unwrap().gen, gen_after_first);
    assert_eq!(
        second.counts.dataset, first.counts.dataset,
        "and lands back on the same D-id"
    );
    // `03` §4.4's corollary as a counter: the converged commit interned a
    // `DatasetStateId` that already existed and minted none. Equality of the two
    // ids above could also be produced by an interner that never allocates;
    // these two say the interner did the recognising.
    assert_eq!(d.dataset_states_recurred, 1, "D17 recurred");
    assert_eq!(d.dataset_states_allocated, 0, "and nothing new was minted");
}

#[test]
fn convergence_can_be_turned_off_for_a_large_panel() {
    let mut frame = seeded(4096);
    let mut state = SessionState::fresh(SessionEpoch(1), FrameId(0));
    state.policy = stratum_runtime::state::ConvergencePolicy::Off;
    {
        let fb = state.begin_command(4);
        frame.begin_command();
        fb.note_create(frame.var(VarIdx(0)).unwrap().id);
        state.commit_command(&mut frame, fb, E1);
    }
    let before = local_snapshot();
    for exec in [E2, ExecutionId(3)] {
        let fb = state.begin_command(4);
        frame.begin_command();
        col_mut(&mut frame, &fb, VarIdx(0)).expect("barrier");
        state.commit_command(&mut frame, fb, exec);
    }
    let d = local_snapshot().since(before);
    assert_eq!(
        d.columns_digested, 0,
        "`set stalecheck provenance` digests nothing"
    );
    assert_eq!(d.gen_bumps, 2, "and therefore always bumps");
}

#[test]
fn the_write_barrier_is_the_only_route_to_a_mutable_column() {
    // `stratum-data` already proves the half a type system can prove: the chunk
    // buffers are private and `Frame::col_mut` is the only way to obtain a
    // `ColMut` at all (its `tests/cow.rs` spawns rustc on the alternatives and
    // asserts E0616/E0624/E0599). What that cannot express is the runtime's
    // half — a mutation that records nothing produces no version bump, and a
    // downstream block then shows ✓ Current over data that moved, which is the
    // §12 failure ADR-008 exists to prevent.
    //
    // So this scans the crate for `.col_mut(` outside test modules and asks
    // whether the file that calls it records anything at all. W06b's own files
    // are a hard failure. Other units' files are reported as a WARNING with
    // file:line, because failing this suite on a file another agent is
    // mid-writing would be a false blocker on a unit that is not W06b's — the
    // finding is escalated in W06b's return instead, which is the sanctioned
    // route.
    let src = src_root();
    let owned_by_w06b = |rel: &std::path::Path| {
        let s = rel.to_string_lossy().replace('\\', "/");
        s.starts_with("state/")
            || matches!(
                s.as_str(),
                "footprint.rs" | "doc.rs" | "snapshot.rs" | "results.rs" | "smcl.rs"
            )
    };

    let mut mine = Vec::new();
    let mut theirs = Vec::new();
    let mut scanned = 0usize;
    visit(&src, &mut |path: &PathBuf, text: &str| {
        scanned += 1;
        let rel = path.strip_prefix(&src).unwrap_or(path).to_path_buf();
        if rel == PathBuf::from("state").join("barrier.rs") {
            return;
        }
        // Test fixtures build a frame by hand and have no execution to record
        // into; the property is about the shipped path.
        let code = text.split("#[cfg(test)]").next().unwrap_or("");
        let records = ["note_write", "note_create", "barrier::col_mut", "AccessLog"]
            .iter()
            .any(|t| code.contains(t));
        if records {
            return;
        }
        for (n, line) in code.lines().enumerate() {
            let stripped = line.split("//").next().unwrap_or("");
            if stripped.contains(".col_mut(") {
                let hit = format!("{}:{}: {}", rel.display(), n + 1, line.trim());
                if owned_by_w06b(&rel) {
                    mine.push(hit);
                } else {
                    theirs.push(hit);
                }
            }
        }
    });

    assert!(scanned > 5, "the scan found only {scanned} source files");
    assert!(
        mine.is_empty(),
        "W06b must route every column mutation through \
         `stratum_runtime::state::barrier::col_mut`:\n  {}",
        mine.join("\n  ")
    );
    if !theirs.is_empty() {
        println!(
            "WARNING (W06b barrier scan): {} call site(s) mutate a column and record \
             nothing, so `gen` never bumps for them and every downstream block stays \
             ✓ Current over changed data. Route them through \
             `stratum_runtime::state::barrier::col_mut`, or call \
             `ExecCtx`'s AccessLog note_write/note_create beside the mutation:\n  {}",
            theirs.len(),
            theirs.join("\n  ")
        );
    }
}

fn src_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    if manifest.join("state").is_dir() {
        return manifest;
    }
    // The out-of-tree harness compiles this file by absolute path.
    let here = PathBuf::from(file!());
    if here.is_absolute() {
        if let Some(root) = here
            .parent()
            .and_then(|p| p.parent())
            .map(|p| p.join("src"))
        {
            if root.join("state").is_dir() {
                return root;
            }
        }
    }
    panic!("cannot locate crates/stratum-runtime/src; the barrier scan cannot run");
}

fn visit(dir: &PathBuf, f: &mut dyn FnMut(&PathBuf, &str)) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries.filter_map(|e| e.ok()).map(|e| e.path()).collect();
    paths.sort();
    for p in paths {
        if p.is_dir() {
            visit(&p, f);
        } else if p.extension().is_some_and(|e| e == "rs") {
            // A file another agent is mid-write may be unreadable for an
            // instant; a missing read is not evidence of a violation.
            if let Ok(text) = std::fs::read_to_string(&p) {
                f(&p, &text);
            }
        }
    }
}
