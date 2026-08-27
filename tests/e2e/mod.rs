//! The e2e scenario aggregator — plan W25.
//!
//! `crates/stratum-e2e/src/lib.rs` pulls this in with `#[cfg(test)] #[path]`, so
//! everything below runs under `cargo nextest run -p stratum-e2e` on all three
//! OSes. Scenario files live here rather than inside the crate because
//! `IMPLEMENTATION_PLAN` §8 hands them to five different units: W15, W16 and W17
//! own `scenario_{a,b,c}.rs`, W26 owns `scenario_d.rs`, and W25 owns only this
//! file, `harness.rs` and the fixtures.
//!
//! # REGISTRATION POINT
//!
//! A Rust module that nothing declares is not compiled, so each scenario file
//! needs one line here the day it is written:
//!
//! ```text
//! mod scenario_a;   // W17
//! mod scenario_b;   // W15
//! mod scenario_c;   // W16
//! ```
//!
//! The lines for files that do not exist yet are absent rather than
//! commented-in, because a `mod` for a missing file fails the build for every
//! unit, not just the missing one. This is the same shape as
//! `xtask/src/main.rs`'s note about W11a and W22: the owner of the aggregator
//! adds the line, and it is a one-line merge rather than a conflict.
//!
//! **Adding the line is this file's whole job, and repair round 3 found it not
//! done.** `scenario_b.rs` had been written by W15 — 567 lines, and its header
//! says in as many words that "the `mod scenario_b;` line in it is W25's to
//! add" — and was declared by nothing, so none of it compiled. That is the
//! defect this unit spent two rounds reporting against `xtask/src/e2e.rs`,
//! occurring in the one registration point W25 actually owns. The check that
//! stops it recurring is `every_scenario_file_in_this_directory_is_declared`
//! below: it reads the directory rather than trusting this list.
//!
//! Note that the *scripts* for scenarios A, B, C and E already exist and already
//! run — they are values in `stratum_e2e::fixtures`, driven by `harness.rs`
//! below. A `scenario_a.rs` file, when W15 writes it, is an additional Rust-level
//! home for assertions the declarative script cannot express; it is not a
//! prerequisite for Scenario A being executed.

mod harness;

// W17's Scenario A — the Rust-level joints the declarative script cannot
// express (its header lists them). Declared by W17 on landing, exactly as the
// REGISTRATION POINT note above prescribes; its dependency budget is
// `stratum_proto`, `serde_json` and `std`, all already edges of this crate.
mod scenario_a;

// W15's Scenario B. Written in this wave and declared by nothing until repair
// round 3, which is the whole reason the guard test at the foot of this file
// exists. Its header states its dependency budget — `stratum_proto`,
// `serde_json`, `rmp_serde` and `std`, nothing from the harness — precisely so
// this line costs the crate no new edge, and all four are already here.
mod scenario_b;

// W26's Scenario D. Held back through wave 1 because its
// `d4_is_still_actually_blocked_on_w09` tripwire was keyed on
// `crates/stratum-cli/Cargo.toml` — a manifest the architect created for W07's
// `serve/**`, not a thing W09 brings — so it was red at HEAD and declaring it
// here would have reported one defect as two.
//
// W26 re-keyed it in repair round 1: `d4_unblocked_because` now reads
// `STRATUM_BIN` or `crates/stratum-cli/src/cmd/run.rs`, which is the capability
// D.4 actually needs. `cargo test -p stratum-workspace --test scenario_d` is 8
// passed / 2 ignored, so the line the file was written for goes in.
//
// It costs this crate nothing to carry: the file depends on `stratum_workspace`,
// `stratum_proto`, `camino` and `std` only — all already in this crate's
// dependency and dev-dependency tables — and nothing in it touches the harness.
// W16's Scenario C. Same case as Scenario B above, found by the same guard.
mod scenario_c;

mod scenario_d;

// ---------------------------------------------------------------------------

/// The guard that makes this file's job checkable instead of remembered.
///
/// Every defect W25 has reported in three repair rounds is the same one: a
/// source file exists, is owned, is finished, and is declared by no module
/// tree, so no compiler has ever seen it and its tests are green in nobody's
/// CI. `xtask/src/e2e.rs` is that (W00's crate root, still owed);
/// `apps/desktop/src-tauri/src/e2e_cmds.rs` was that. Round 3 found
/// `tests/e2e/scenario_b.rs` in the same state — 567 lines behind a `mod` line
/// **this unit owns**, which makes it W25's own instance of the defect it kept
/// escalating about other people's files.
///
/// So the list above is no longer the authority. This test reads the directory
/// and fails on any `scenario_*.rs` the aggregator does not declare, naming the
/// line to add. It cannot be satisfied by remembering.
#[test]
fn every_scenario_file_in_this_directory_is_declared() {
    let dir = stratum_e2e::fixtures::repo_root()
        .expect("repository root")
        .join("tests/e2e");
    let me = std::fs::read_to_string(dir.join("mod.rs")).expect("this file");

    let mut undeclared: Vec<String> = Vec::new();
    for entry in std::fs::read_dir(&dir).expect("tests/e2e is readable") {
        let name = entry.expect("directory entry").file_name();
        let name = name.to_string_lossy();
        let Some(stem) = name.strip_suffix(".rs") else {
            continue;
        };
        if !stem.starts_with("scenario_") {
            continue;
        }
        if !me.lines().any(|l| l.trim() == format!("mod {stem};")) {
            undeclared.push(stem.to_owned());
        }
    }
    undeclared.sort();

    assert!(
        undeclared.is_empty(),
        "tests/e2e/{{{}}}.rs exist and are compiled by NOTHING. Add `mod {};` to \
         tests/e2e/mod.rs — that line is W25's to write and no other unit may write \
         it. A scenario file no compiler has seen asserts nothing, however finished \
         it looks in a diff.",
        undeclared.join(","),
        undeclared.join("; mod ")
    );
}
