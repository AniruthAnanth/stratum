//! **Scenario B — stale state.** Spec §38-B, plan §2 row B, plan W15.
//!
//! > execute transformation → execute dependent model → change transformation
//! > code → model output marked stale → rerun from changed block.
//!
//! # What this file is, and what it is not
//!
//! The *script* for Scenario B already exists and already runs: it is
//! `stratum_e2e::fixtures::scenario_b()`, driven by `tests/e2e/harness.rs`. W25's
//! `mod.rs` says exactly what a `scenario_b.rs` adds on top of that — "an
//! additional Rust-level home for assertions the declarative script cannot
//! express" — and this file contains only those.
//!
//! The script drives a host and observes glyphs. What it cannot observe is the
//! set of *joints* Scenario B is carried by, every one of which spans two
//! artifacts that no compiler checks against each other:
//!
//! | joint | the two sides |
//! |---|---|
//! | the nine statuses | `stratum_proto::BlockStatus` ↔ `STATUS_RANK` in `ipc/hand.ts` |
//! | the total order | CONTRACTS §3's prose ↔ the numbers in that table |
//! | "income was modified at E44" | `StaleReason`/`DepKey` variants ↔ the switches in `components/StaleBanner.tsx` |
//! | `✓⚠` | `Taint`'s bit positions ↔ the `TAINT` table in `state/exec.ts` |
//! | the wire | `EngineEvent::StatusChanged` ↔ **both** encodings |
//!
//! A gap in any of them renders as *silence*, which is the one failure mode §12
//! rules out: a stale block whose reason has no sentence draws an empty strip, a
//! `CurrentUnverifiable` whose taint bit moved draws an unqualified tick, and a
//! `StatusChanged` that will not decode leaves the whole document looking
//! current. None of those raise an error anywhere; a scenario watching glyphs
//! would report "✓" and pass.
//!
//! # Dependencies, deliberately minimal
//!
//! `stratum_proto`, `serde_json`, `rmp_serde` and `std`. Nothing from the e2e
//! harness — the same choice W26 made in `scenario_d.rs`, and for the same
//! reason: this file has to be includable with a one-line `#[path]` from
//! whichever crate ends up compiling it, without that crate inheriting a
//! dependency edge. `tests/e2e/mod.rs` is **W25's** file and the `mod scenario_b;`
//! line in it is W25's to add; W15 does not write a byte there (R0).

use std::path::PathBuf;

use stratum_proto::ids::{BlockId, DatasetStateId, DocumentId, ExecutionId};
use stratum_proto::status::{BlockStatus, BrokenReason, DepKey, StaleReason, Taint};

// ---------------------------------------------------------------------------
// Reading the two sides
// ---------------------------------------------------------------------------

/// The repository root, found by walking up to the `[workspace]` manifest.
///
/// Not `CARGO_MANIFEST_DIR` on its own: this file is compiled by whichever crate
/// `#[path]`-includes it, and those sit at different depths. Same shape as
/// `scenario_d.rs`'s `workspace_root`, on purpose — two acceptance files that
/// locate the repo differently is one more thing that can be subtly wrong.
fn workspace_root() -> PathBuf {
    let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    loop {
        let manifest = dir.join("Cargo.toml");
        if std::fs::read_to_string(&manifest)
            .unwrap_or_default()
            .lines()
            .any(|l| l.trim() == "[workspace]")
        {
            return dir;
        }
        assert!(
            dir.pop(),
            "no [workspace] Cargo.toml above {}",
            env!("CARGO_MANIFEST_DIR")
        );
    }
}

fn read(rel: &str) -> String {
    let path = workspace_root().join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

/// The body of a `function name(` … `\n}` block, so a `case "x"` in one switch is
/// never mistaken for a `case "x"` in another.
fn function_body<'a>(source: &'a str, name: &str) -> &'a str {
    let needle = format!("function {name}(");
    let start = source
        .find(&needle)
        .unwrap_or_else(|| panic!("{name} is gone from the file this test reads"));
    let rest = &source[start..];
    let end = rest
        .find("\n}\n")
        .unwrap_or_else(|| panic!("{name} has no closing brace at column 0"));
    &rest[..end]
}

/// The `state` tag serde writes for a `BlockStatus` — its wire identity.
fn wire_tag<T: serde::Serialize>(value: &T, tag: &str) -> String {
    let json = serde_json::to_value(value).expect("a proto type serialises");
    json.get(tag)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_else(|| panic!("no `{tag}` discriminant on {json}"))
        .to_owned()
}

// ---------------------------------------------------------------------------
// The nine
// ---------------------------------------------------------------------------

const E41: ExecutionId = ExecutionId(41);
const D17: DatasetStateId = DatasetStateId(17);

/// One value of every `BlockStatus` variant. Written out rather than derived,
/// because the point is to fail to compile when a tenth is added.
fn every_status() -> Vec<BlockStatus> {
    vec![
        BlockStatus::NeverRun,
        BlockStatus::Queued { position: 2 },
        BlockStatus::Running {
            exec: E41,
            started_ms: 1,
        },
        BlockStatus::Current {
            exec: E41,
            dataset: D17,
            duration_us: 80_000,
        },
        BlockStatus::CurrentUnverifiable {
            exec: E41,
            dataset: D17,
            duration_us: 900,
            taint: Taint::EXTERNAL,
        },
        BlockStatus::Stale {
            reason: StaleReason::CodeChanged,
            since: Some(E41),
        },
        BlockStatus::Failed { exec: E41, rc: 111 },
        BlockStatus::Interrupted {
            exec: E41,
            rolled_back: true,
        },
        BlockStatus::Broken {
            reason: BrokenReason::UnresolvedName {
                name: "incme".to_owned(),
                suggestion: Some("income".to_owned()),
            },
        },
    ]
}

fn every_stale_reason() -> Vec<StaleReason> {
    vec![
        StaleReason::CodeChanged,
        StaleReason::EpochReset,
        StaleReason::InputChanged {
            key: DepKey::Var {
                frame: "default".to_owned(),
                name: "income".to_owned(),
            },
            at: Some(ExecutionId(44)),
        },
        StaleReason::FileChanged {
            path: "data/wages.dta".into(),
        },
        StaleReason::UpstreamPending {
            block: BlockId(3),
            via: DepKey::RowMembership {
                frame: "default".to_owned(),
            },
        },
        StaleReason::UpstreamOpaque { block: BlockId(3) },
        StaleReason::RngShifted,
    ]
}

fn every_dep_key() -> Vec<DepKey> {
    let frame = || "default".to_owned();
    let name = || "x".to_owned();
    vec![
        DepKey::Var {
            frame: frame(),
            name: name(),
        },
        DepKey::RowMembership { frame: frame() },
        DepKey::RowOrder { frame: frame() },
        DepKey::VarLayout { frame: frame() },
        DepKey::Macro { name: name() },
        DepKey::Scalar { name: name() },
        DepKey::Matrix { name: name() },
        DepKey::Program { name: name() },
        DepKey::Estimates,
        DepKey::RClass,
        DepKey::SClass,
        DepKey::Rng,
        DepKey::Setting { name: name() },
        DepKey::Cwd,
        DepKey::File {
            path: "a.dta".into(),
        },
    ]
}

// ---------------------------------------------------------------------------
// B.1 — the statuses the wire has are the statuses the UI ranks
// ---------------------------------------------------------------------------

const HAND_TS: &str = "apps/desktop/src/ipc/hand.ts";
const EXEC_TS: &str = "apps/desktop/src/state/exec.ts";
const BANNER_TSX: &str = "apps/desktop/src/components/StaleBanner.tsx";
const RUNQUEUE_TSX: &str = "apps/desktop/src/components/RunQueue.tsx";
const EDITOR_COMMANDS_TS: &str = "apps/desktop/src/editor/commands.ts";

/// `STATUS_RANK`, parsed out of the frontend's own source.
///
/// Read rather than duplicated. A copy of the table here would agree with itself
/// forever, which is the failure mode this whole file exists to catch.
fn status_rank() -> Vec<(String, u32)> {
    let source = read(HAND_TS);
    let start = source
        .find("export const STATUS_RANK = {")
        .expect("STATUS_RANK is gone from ipc/hand.ts");
    let rest = &source[start..];
    let end = rest.find("} as const;").expect("STATUS_RANK is not closed");
    rest[..end]
        .lines()
        .filter_map(|line| {
            let line = line.trim().trim_end_matches(',');
            let (name, value) = line.split_once(": ")?;
            let name = name.trim();
            if !name.chars().all(|c| c.is_ascii_lowercase() || c == '_') {
                return None;
            }
            Some((name.to_owned(), value.trim().parse::<u32>().ok()?))
        })
        .collect()
}

#[test]
fn b_every_wire_status_has_a_rank_and_the_table_has_no_others() {
    let wire: Vec<String> = every_status()
        .iter()
        .map(|s| wire_tag(s, "state"))
        .collect();
    let ranked: Vec<String> = status_rank().into_iter().map(|(name, _)| name).collect();

    assert_eq!(wire.len(), 9, "CONTRACTS §3 has nine BlockStatus variants");

    for state in &wire {
        assert!(
            ranked.contains(state),
            "`{state}` is on the wire and has no entry in STATUS_RANK ({HAND_TS}).\n\
             `worseOf` would compare `undefined <= undefined`, which is false, so the \
             kernel's verdict would silently lose to the local one. Add the rank."
        );
    }
    for state in &ranked {
        assert!(
            wire.contains(state),
            "STATUS_RANK ranks `{state}`, which no BlockStatus variant serialises to. \
             Either the wire dropped a variant or the table has a typo; both make the \
             rank unreachable."
        );
    }
}

#[test]
fn b_the_rank_table_is_the_total_order_contracts_3_writes_down() {
    let ranks = status_rank();
    let rank = |name: &str| -> u32 {
        ranks
            .iter()
            .find(|(n, _)| n == name)
            .unwrap_or_else(|| panic!("no rank for {name}"))
            .1
    };

    // NeverRun < Broken < Failed < Interrupted < Stale < CurrentUnverifiable < Current
    let ladder = [
        "never_run",
        "broken",
        "failed",
        "interrupted",
        "stale",
        "current_unverifiable",
        "current",
    ];
    for pair in ladder.windows(2) {
        let (lo, hi) = (pair[0], pair[1]);
        assert!(
            rank(lo) < rank(hi),
            "CONTRACTS §3 orders {lo} < {hi}; the table says {} vs {}",
            rank(lo),
            rank(hi)
        );
    }

    // "…and Queued/Running always win, because they are facts, not judgements."
    for fact in ["queued", "running"] {
        assert!(
            rank(fact) >= 90,
            "{fact} must outrank every judgement, or a local stale verdict could hide \
             a block that is running right now"
        );
    }
}

// ---------------------------------------------------------------------------
// B.2 — every reason the engine can send has a sentence
// ---------------------------------------------------------------------------

#[test]
fn b_every_stale_reason_the_engine_can_send_renders_a_sentence() {
    let banner = read(BANNER_TSX);
    let body = function_body(&banner, "staleBecause");

    for reason in every_stale_reason() {
        let why = wire_tag(&reason, "why");
        assert!(
            body.contains(&format!("case \"{why}\":")),
            "`StaleReason::{why}` has no arm in `staleBecause` ({BANNER_TSX}).\n\
             Spec §12's whole promise is that a downstream block goes VISIBLY stale; a \
             reason with no sentence draws an empty strip, which reads as 'nothing is \
             wrong'."
        );
    }
}

#[test]
fn b_every_broken_reason_renders_a_sentence() {
    let banner = read(BANNER_TSX);
    let body = function_body(&banner, "brokenBecause");

    for reason in [
        BrokenReason::UnresolvedName {
            name: "x".to_owned(),
            suggestion: None,
        },
        BrokenReason::UnknownCommand {
            name: "x".to_owned(),
            suggestion: None,
        },
        BrokenReason::MissingFile {
            path: "a.dta".into(),
        },
    ] {
        let why = wire_tag(&reason, "why");
        assert!(
            body.contains(&format!("case \"{why}\":")),
            "`BrokenReason::{why}` has no arm in `brokenBecause` ({BANNER_TSX}). \
             Broken means re-running would ERROR; saying so is the entire difference \
             from Stale."
        );
    }
}

#[test]
fn b_every_dep_key_namespace_can_be_named_in_a_banner() {
    let banner = read(BANNER_TSX);
    let body = function_body(&banner, "depKeyLabel");

    let keys = every_dep_key();
    assert_eq!(
        keys.len(),
        15,
        "CONTRACTS §3 lists fifteen DepKey namespaces"
    );

    for key in keys {
        let ns = wire_tag(&key, "ns");
        assert!(
            body.contains(&format!("case \"{ns}\":")),
            "`DepKey::{ns}` has no arm in `depKeyLabel` ({BANNER_TSX}). CONTRACTS §3 \
             says a DepKey is 'rendered verbatim in stale banners'; an unhandled one \
             makes 'income was modified at E44' come out as 'was modified at E44'."
        );
    }
}

// ---------------------------------------------------------------------------
// B.3 — ✓⚠ depends on a bit position agreeing across two languages
// ---------------------------------------------------------------------------

#[test]
fn b_the_taint_bits_agree_with_the_frontends_table() {
    let exec = read(EXEC_TS);
    let start = exec
        .find("export const TAINT = {")
        .expect("the TAINT table is gone from state/exec.ts");
    let table = &exec[start..start + exec[start..].find("} as const;").expect("unclosed")];

    // `Taint`'s hand-written Serialize writes the raw u16 (stratum-proto reports
    // this as a contract deviation), so the bit POSITION is the wire and a shift
    // by one silently reclassifies every taint the UI names.
    for (name, flag) in [
        ("MACRO_VARLIST", Taint::MACRO_VARLIST),
        ("UNKNOWN_COMMAND", Taint::UNKNOWN_COMMAND),
        ("DYNAMIC_DISPATCH", Taint::DYNAMIC_DISPATCH),
        ("EXTERNAL", Taint::EXTERNAL),
        ("CLOCK", Taint::CLOCK),
        ("ENVIRONMENT", Taint::ENVIRONMENT),
        ("UNBOUNDED_LOOP", Taint::UNBOUNDED_LOOP),
        ("FILE_DYNAMIC", Taint::FILE_DYNAMIC),
    ] {
        let shift = flag.bits().trailing_zeros();
        let expected = format!("{name}: 1 << {shift},");
        assert!(
            table.contains(&expected),
            "state/exec.ts must declare `{expected}` — stratum-proto puts \
             Taint::{name} at bit {shift}. A disagreement here makes a \
             CurrentUnverifiable block explain itself with the wrong cause, or with none."
        );
    }
}

// ---------------------------------------------------------------------------
// B.4 — the §38-B sentence survives BOTH encodings
// ---------------------------------------------------------------------------

/// "model output marked stale" is an `EngineEvent::StatusChanged`, and it has to
/// arrive.
///
/// Both encodings on purpose. W00 found that the derived `bitflags` serde wrote
/// `73` through `rmp-serde`'s serializer (`is_human_readable() == false`) and
/// then refused to read it back through its deserializer (`== true`), which would
/// have made **every** `CurrentUnverifiable` and every `ExecutionRecord` fail to
/// decode on the desktop transport. That was fixed at the type; this is the
/// consumer-side regression test for it, written from the one scenario whose
/// acceptance depends on the payload surviving.
#[test]
fn b_the_stale_verdict_round_trips_through_json_and_messagepack() {
    let changed = vec![
        (
            BlockId(2),
            BlockStatus::Stale {
                reason: StaleReason::InputChanged {
                    key: DepKey::Var {
                        frame: "default".to_owned(),
                        name: "income".to_owned(),
                    },
                    at: Some(ExecutionId(44)),
                },
                since: Some(E41),
            },
        ),
        (
            BlockId(3),
            BlockStatus::CurrentUnverifiable {
                exec: E41,
                dataset: D17,
                duration_us: 900,
                taint: Taint::EXTERNAL | Taint::ENVIRONMENT,
            },
        ),
    ];
    let event = stratum_proto::engine::EngineEvent::StatusChanged {
        seq: 7,
        doc: DocumentId(1),
        changed,
    };

    let json = serde_json::to_vec(&event).expect("json encode");
    let from_json: stratum_proto::engine::EngineEvent =
        serde_json::from_slice(&json).expect("json decode");
    assert_eq!(from_json, event, "StatusChanged did not survive JSON");

    let mp = rmp_serde::to_vec_named(&event).expect("messagepack encode");
    let from_mp: stratum_proto::engine::EngineEvent =
        rmp_serde::from_slice(&mp).expect("messagepack decode — see W00's Taint deviation note");
    assert_eq!(from_mp, event, "StatusChanged did not survive MessagePack");
}

// ---------------------------------------------------------------------------
// B.5 — the frontend cannot un-stale a block, and can rerun from one
// ---------------------------------------------------------------------------

/// ADR-008's INV-1, asserted structurally rather than behaviourally.
///
/// The behavioural proof is exhaustive in
/// `apps/desktop/src/components/exec.state.test.ts`. This is the cheaper
/// complement, and it catches the thing a rule-level test cannot: a *second*
/// place in the module that constructs a healthy status directly and never goes
/// through the rule at all.
#[test]
fn b_the_frontends_only_local_verdict_is_stale() {
    let exec = read(EXEC_TS);
    let mut constructed: Vec<&str> = exec
        .match_indices("state: \"")
        .filter(|(at, _)| {
            // Skip the `readonly state: "current"` lines of the type declarations.
            !exec[..*at].ends_with("readonly ")
        })
        .filter_map(|(at, _)| {
            let rest = &exec[at + "state: \"".len()..];
            rest.find('"').map(|end| &rest[..end])
        })
        .collect();
    constructed.sort_unstable();
    constructed.dedup();

    assert_eq!(
        constructed,
        vec!["never_run", "stale"],
        "{EXEC_TS} constructs a status other than NeverRun or Stale. ADR-008: the local \
         check may only ever move a block TOWARD stale, and a frontend that can build a \
         healthy status has a path around `worseOf`."
    );
}

/// "rerun from the changed block" — §38-B's last clause.
#[test]
fn b_rerun_from_the_changed_block_is_an_offered_verb() {
    let banner = read(BANNER_TSX);
    let queue = read(RUNQUEUE_TSX);

    assert!(
        banner.contains("command: \"run.fromHere\""),
        "the stale strip must offer `run.fromHere` ({BANNER_TSX}); 06 §5.2 lists it \
         beside Rerun and Diff code"
    );
    // The verbs a button DISPATCHES and the verbs the editor REGISTERS are two
    // lists in two units, joined by a string. `runCommand` answers an id nobody
    // registered with "unknown" and no error — so a rename in W13 turns every
    // button here into a no-op that looks perfectly healthy. Nothing but this
    // notices.
    let registered = read(EDITOR_COMMANDS_TS);
    for verb in [
        "run.fromHere",
        "run.above",
        "run.toCursor",
        "run.section",
        "run.allStale",
    ] {
        assert!(
            queue.contains(&format!("command: \"{verb}\"")),
            "spec §14 names `{verb}`; it is not offered in {RUNQUEUE_TSX}"
        );
        assert!(
            registered.contains(&format!("\"{verb}\"")),
            "{RUNQUEUE_TSX} dispatches `{verb}`, which {EDITOR_COMMANDS_TS} does not \
             name. `runCommand` returns \"unknown\" for an unregistered id and throws \
             nothing, so the button would be dead and look fine."
        );
    }
}

// ---------------------------------------------------------------------------
// The paths this file reads are real
// ---------------------------------------------------------------------------

/// A source-reading test whose path has moved is green forever and reads as an
/// assertion. Five files, checked once, loudly.
#[test]
fn b_the_sources_this_scenario_reads_are_where_it_thinks() {
    let root = workspace_root();
    for rel in [
        HAND_TS,
        EXEC_TS,
        BANNER_TSX,
        RUNQUEUE_TSX,
        EDITOR_COMMANDS_TS,
    ] {
        assert!(
            root.join(rel).is_file(),
            "{rel} has moved. Every assertion in tests/e2e/scenario_b.rs that reads it \
             is now dead code; re-point them rather than deleting them."
        );
    }
}
