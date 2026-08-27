//! State identity — W06's convergence and rename acceptance bullets.
//!
//! > **Convergence**: re-running a block verbatim leaves `gen` unchanged and
//! > interns back to the **same** `DatasetStateId`. This is the test that proves
//! > `D17` is a recurring identity, not a counter.
//!
//! > **Rename** keeps `VarId` and `gen`, bumps `var_layout`; a downstream block
//! > reading the new name stays Current, one reading the old name goes `Broken`.
//!
//! The status labels themselves are assigned by `stratum-exec`'s C0–C9 sweep.
//! What this crate owns, and what is asserted here, is the input that sweep
//! consults: C6 compares a recorded `DepFootprint` version against
//! `VersionTable::version_of`, and C5 fires when a name no longer resolves. So
//! "stays Current" is `Version::At(recorded)` and "goes Broken" is
//! `Version::Unresolved`, which is the same claim one layer down and is
//! testable without W08.

use std::sync::Arc;

use stratum_proto::{DepKey, ExecutionId, FrameId, SessionEpoch, VarId};
use stratum_runtime::snapshot::{Version, VersionTable};
use stratum_runtime::state::dataset::{Carry, DatasetFingerprint, DatasetInterner};
use stratum_runtime::state::fingerprint::{Ns, PathKey, RngFingerprint, StateFingerprint};
use stratum_runtime::state::{local_snapshot, ConvergencePolicy, DigestCache};

const E1: ExecutionId = ExecutionId(1);
const E2: ExecutionId = ExecutionId(2);
const FRAME: FrameId = FrameId(0);

fn auto_like() -> DatasetFingerprint {
    let mut ds = DatasetFingerprint::empty(FRAME, Carry::default());
    for (i, n) in [(1u32, "make"), (2, "price"), (3, "mpg"), (4, "weight")] {
        ds.create(VarId(i), n, E1);
    }
    ds.change_membership(74);
    ds
}

fn table(ds: &DatasetFingerprint) -> VersionTable {
    let mut fp = StateFingerprint::fresh(SessionEpoch(1), FRAME);
    fp.set_frame(FRAME, ds.clone());
    VersionTable::from_fingerprint(1, &fp, &|_| Some("default".into()))
}

fn var(name: &str) -> DepKey {
    DepKey::Var {
        frame: "default".into(),
        name: name.into(),
    }
}

#[test]
fn a_rename_keeps_identity_and_generation_and_only_the_old_name_breaks() {
    let mut ds = auto_like();
    let id = ds.id_of("mpg").expect("mpg");
    let gen = ds.version_of(id).expect("version").gen;
    let layout = ds.var_layout;
    let acc = ds.acc;

    // A downstream block recorded `mpg` at this version before the rename.
    let recorded = table(&ds).version_of(&var("mpg"));
    assert_eq!(recorded, Version::At(u64::from(gen)));

    assert!(ds.rename("mpg", "mpg_hwy"));

    assert_eq!(ds.id_of("mpg_hwy"), Some(id), "VarId survives a rename");
    assert_eq!(ds.version_of(id).unwrap().gen, gen, "and so does gen");
    assert_eq!(ds.var_layout, layout + 1, "only the layout counter moves");
    assert_eq!(ds.acc, acc, "no data moved, so the fold cannot move");

    let after = table(&ds);
    // A block reading the NEW name sees the version it always saw: Current.
    assert_eq!(
        after.version_of(&var("mpg_hwy")),
        Version::At(u64::from(gen))
    );
    // A block reading the OLD name cannot resolve it: Broken, not Stale.
    // Re-running it would ERROR, not merely produce different numbers.
    assert_eq!(after.version_of(&var("mpg")), Version::Unresolved);
}

#[test]
fn a_reorder_moves_no_version_a_downstream_block_reads() {
    let mut ds = auto_like();
    let before = table(&ds);
    ds.reorder();
    let after = table(&ds);
    for n in ["make", "price", "mpg", "weight"] {
        assert_eq!(
            before.version_of(&var(n)),
            after.version_of(&var(n)),
            "`order` changes VarIdx, which is position, not identity"
        );
    }
    assert_ne!(
        before.version_of(&DepKey::VarLayout {
            frame: "default".into()
        }),
        after.version_of(&DepKey::VarLayout {
            frame: "default".into()
        }),
        "a metadata-sensitive block still sees the layout move"
    );
}

#[test]
fn dropping_rows_is_one_counter_not_one_bump_per_column() {
    // `03` §4.3: "O(1), not O(#vars)". Asserted as a counter (ADR-017).
    let mut ds = DatasetFingerprint::empty(FRAME, Carry::default());
    for i in 1..=200u32 {
        ds.create(VarId(i), &format!("v{i}"), E1);
    }
    ds.change_membership(1_000_000);
    let acc = ds.acc;
    let before = local_snapshot();

    ds.change_membership(400_000); // `drop if`

    assert_eq!(
        local_snapshot().since(before).gen_bumps,
        0,
        "no per-column bump for a membership change"
    );
    assert_eq!(ds.acc, acc, "and no per-column fold update either");
    assert_eq!(ds.eff(VarId(7)).unwrap().row_membership, ds.row_membership);
}

#[test]
fn a_converged_column_returns_the_same_dataset_state_id() {
    let mut interner = DatasetInterner::new();
    let mut ds = auto_like();
    let first = interner.intern(&mut ds);
    let gen = ds.version_of(VarId(2)).unwrap().gen;

    // Re-running the block: the barrier digests `price`, finds it unchanged, and
    // re-stamps provenance instead of bumping.
    ds.touch_origin(VarId(2), E2);
    let again = interner.intern(&mut ds);

    assert_eq!(first, again, "D17 recurs");
    assert_eq!(ds.version_of(VarId(2)).unwrap().gen, gen);
    assert_eq!(
        ds.version_of(VarId(2)).unwrap().origin,
        E2,
        "provenance still moves"
    );
    assert_eq!(interner.collisions(), 0);
}

#[test]
fn a_genuine_change_and_a_change_back_both_land_on_their_own_id() {
    // The property that makes interning worth having: state identity is a
    // function of state, so going A -> B -> A returns to A's id.
    let mut interner = DatasetInterner::new();
    let mut ds = auto_like();
    let a = interner.intern(&mut ds);
    ds.bump_value(VarId(2), E2);
    let b = interner.intern(&mut ds);
    assert_ne!(a, b);

    // Undo it the only way versions allow: a fresh fingerprint describing the
    // earlier state. (A real `preserve`/`restore` rebuilds exactly this.)
    let mut back = auto_like();
    let c = interner.intern(&mut back);
    assert_eq!(
        a, c,
        "the same state is the same id, however it was reached"
    );
    assert_eq!(interner.len(), 2);
    assert_eq!(interner.collisions(), 0);
}

#[test]
fn the_digest_cache_only_converges_within_a_generation() {
    let mut cache = DigestCache::new();
    let d = stratum_proto::ColumnDigest([9; 16]);
    cache.record(VarId(1), 3, d);
    assert_eq!(
        cache.check(VarId(1), 3, d),
        stratum_runtime::state::Convergence::Converged
    );
    assert_eq!(
        cache.check(VarId(1), 4, d),
        stratum_runtime::state::Convergence::Diverged,
        "a digest from an older generation must not authorise a skipped bump"
    );
}

#[test]
fn the_policy_declines_a_column_larger_than_its_ceiling() {
    let p = ConvergencePolicy::Bounded { max_bytes: 1024 };
    assert!(p.admits(1024));
    assert!(!p.admits(1025));
    // Declining over-marks, which INV-1 permits; it never under-marks.
    assert_eq!(
        ConvergencePolicy::default().as_setting(),
        "content(268435456)"
    );
}

#[test]
fn the_incremental_fold_tracks_the_definition_through_a_long_session() {
    // The accumulator is only ever updated incrementally on the hot path. If it
    // could drift from the definitional fold, two genuinely different states
    // could intern to one id and a block would show ✓ Current over changed data.
    let mut fp = StateFingerprint::fresh(SessionEpoch(1), FRAME);
    let mut ds = auto_like();
    let mut interner = DatasetInterner::new();
    interner.intern(&mut ds);
    fp.set_frame(FRAME, ds.clone());

    for step in 0..500u32 {
        match step % 10 {
            0 => {
                ds.bump_value(VarId(1 + step % 4), ExecutionId(u64::from(step)));
            }
            1 => {
                ds.create(VarId(1000 + step), &format!("g{step}"), E1);
            }
            2 => ds.change_membership(u64::from(74 + step)),
            3 => ds.change_order(Some(Arc::from(vec![VarId(3)])), true),
            4 => ds.reorder(),
            5 => {
                fp.bump_named(Ns::Local, &format!("l{}", step % 7));
            }
            6 => {
                fp.bump_named(Ns::Global, "root");
            }
            7 => {
                fp.bump_named(Ns::Setting, "type");
            }
            8 => fp.set_rng(RngFingerprint {
                draws: u64::from(step),
                ..RngFingerprint::fresh()
            }),
            _ => fp.stamp_file(
                PathKey::new(format!("/data/{}.dta", step % 3)),
                stratum_runtime::state::FileStamp::from_parts(i128::from(step), 10, 1, None),
            ),
        }
        interner.intern(&mut ds);
        fp.set_frame(FRAME, ds.clone());
        assert_eq!(
            fp.acc,
            fp.recompute_acc(),
            "incremental fold diverged from the definition at step {step}"
        );
    }
    assert_eq!(interner.collisions(), 0, "128 bits, and nothing collided");
    println!(
        "interned {} distinct dataset states over 500 operations",
        interner.len()
    );
}

#[test]
fn dropping_a_column_and_creating_a_new_one_with_the_same_name_is_a_new_state() {
    // `VarId` is never reused, so `drop x` then `gen x` is not the state we
    // started from — and a block that read the old `x` must not stay Current.
    let mut interner = DatasetInterner::new();
    let mut ds = auto_like();
    let a = interner.intern(&mut ds);
    ds.drop_var(VarId(3), "mpg");
    ds.create(VarId(99), "mpg", E2);
    let b = interner.intern(&mut ds);
    assert_ne!(a, b);
    assert_eq!(ds.id_of("mpg"), Some(VarId(99)));
}
