//! **Scenario A — notebook-like analysis.** Spec §38-A, plan §2 row A, plan W17.
//!
//! > open do-file → cursor on `summarize` → Shift+Enter → result appears
//! > underneath → cursor moves to next executable block → execute regression →
//! > result appears underneath → no source-code corruption.
//!
//! # What this file is, and what it is not
//!
//! The *script* for Scenario A already exists and already runs: it is
//! `stratum_e2e::fixtures::scenario_a()`, driven by `tests/e2e/harness.rs` —
//! against the pre-host bridge since wave 1, and against the packaged
//! `stratum-desktop --features e2e` binary since W17 landed (8/8 steps, 0
//! blocked, `sleeps == 0`, `polls == 0`). W25's `mod.rs` says what a
//! `scenario_a.rs` adds on top: "an additional Rust-level home for assertions
//! the declarative script cannot express" — and this file contains only those.
//!
//! The script drives a host and observes glyphs and cards. What it cannot
//! observe is the set of *joints* Scenario A's keystroke travels through,
//! each spanning two artifacts no compiler checks against each other:
//!
//! | joint | the two sides |
//! |---|---|
//! | the command names | `invoke("…")` in `apps/desktop/src` ↔ `stratum_handler!` in `src-tauri/src/main.rs` |
//! | the run intents | the `{ intent: "…" }` literals in `boot/wire.tsx` ↔ `stratum_proto::exec::RunIntent`'s serde tags |
//! | the boot handshake | the `assetToken`/`e2e` keys `boot/wire.tsx` reads ↔ the reply `ipc::app_ready` builds |
//! | the packaged CSP | `tauri.conf.json` ↔ CONTRACTS §10.2 (A21) **and** the wasm allowance below |
//! | Shift+Enter | the fixture's `Chord::new("Shift+Enter")` ↔ `resources/keymaps/*.json` |
//!
//! A gap in any of them renders as *silence in the packaged build only*: an
//! unregistered command is "unknown command" swallowed by a bridge that treats
//! it as "no host", a respelled intent tag is a 400 from serde with the result
//! card never appearing, and a CSP omission is a broken image or an unsegmented
//! editor that dev mode (vite on :1420, no packaged CSP, no embedded dist)
//! never reproduces. Two of those were found by running the packaged binary,
//! not by reading it — see `wasm-unsafe-eval` below.
//!
//! # Dependencies, deliberately minimal
//!
//! `stratum_proto`, `serde_json` and `std` — the same set as `scenario_b.rs`
//! and for the same reason: this file is `#[path]`-included by whichever crate
//! compiles it and must not add a dependency edge. `tests/e2e/mod.rs` is
//! **W25's** file; the `mod scenario_a;` line is the one-line registration its
//! header prescribes.

use std::path::{Path, PathBuf};

use stratum_proto::exec::RunIntent;

// ---------------------------------------------------------------------------
// Reading the two sides
// ---------------------------------------------------------------------------

/// The repository root, found by walking up to the `[workspace]` manifest —
/// the same shape as `scenario_b.rs` and `scenario_d.rs`, on purpose.
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

/// Every `.ts`/`.tsx` source under `dir`, tests excluded — the frontend as the
/// packaged bundle sees it.
fn frontend_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
    for entry in entries {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            frontend_sources(&path, out);
            continue;
        }
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let is_source = name.ends_with(".ts") || name.ends_with(".tsx");
        if is_source && !name.contains(".test.") {
            out.push(path);
        }
    }
}

/// Every command name the frontend passes to `invoke(…)` / `invoke<T>(…)`.
///
/// A scanner, not a parser: find each `invoke` call and take the first string
/// literal after it, accepting only `[a-z_]` names so `invoke(command, args)`
/// pass-throughs (the bridge's own plumbing) are skipped rather than
/// misread.
fn invoked_commands(source: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (at, _) in source.match_indices("invoke") {
        let rest = &source[at..];
        // The literal must open within the call's argument head; 64 bytes is
        // generous for `invoke<SomeLongReply>("name"` and short enough not to
        // swallow a string from the next statement.
        let head = &rest[..rest.len().min(64)];
        let Some(open) = head.find('"') else { continue };
        let after = &rest[open + 1..];
        let Some(close) = after.find('"') else {
            continue;
        };
        let name = &after[..close];
        if !name.is_empty() && name.chars().all(|c| c.is_ascii_lowercase() || c == '_') {
            out.push(name.to_owned());
        }
    }
    out
}

// ---------------------------------------------------------------------------
// The command-name joint
// ---------------------------------------------------------------------------

/// Every command the frontend invokes exists in the host's one handler list.
///
/// `tauri::generate_handler!` registers by NAME, the webview invokes by NAME,
/// and neither side's compiler has ever seen the other. An unregistered
/// command is not an error anyone reads: the invoke rejects with "unknown
/// command", the bridge's callers treat rejection as "no host yet", and
/// Scenario A's Shift+Enter silently does nothing — in the packaged build
/// only, because the dev bridge answers everything.
#[test]
fn every_command_the_frontend_invokes_is_registered_by_the_host() {
    let root = workspace_root();
    let mut sources = Vec::new();
    frontend_sources(&root.join("apps/desktop/src"), &mut sources);
    assert!(
        sources.len() > 50,
        "the frontend scan found only {} sources — the walk is broken, not the app small",
        sources.len()
    );

    let main_rs = read("apps/desktop/src-tauri/src/main.rs");

    let mut invoked: Vec<String> = sources
        .iter()
        .flat_map(|p| {
            let text = std::fs::read_to_string(p).unwrap_or_default();
            invoked_commands(&text)
        })
        .collect();
    invoked.sort();
    invoked.dedup();
    assert!(
        invoked.iter().any(|c| c == "exec_submit") && invoked.iter().any(|c| c == "app_ready"),
        "the invoke scan no longer sees the run path ({invoked:?}) — fix the scanner, \
         not this assertion"
    );

    let missing: Vec<&String> = invoked
        .iter()
        .filter(|name| {
            let registered = main_rs.contains(&format!("ipc::{name}"));
            // The three fenced names are registered through the feature branch
            // of `stratum_handler!` (ADR-011); they are host commands too.
            let fenced = main_rs.contains(&format!("e2e_cmds::tauri_surface::{name}"));
            !(registered || fenced)
        })
        .collect();
    assert!(
        missing.is_empty(),
        "the frontend invokes commands the host never registers: {missing:?}. \
         In the packaged app each of these is a silent no-op."
    );
}

// ---------------------------------------------------------------------------
// The run-intent joint
// ---------------------------------------------------------------------------

/// Every `RunIntent` shape `boot/wire.tsx`'s `intentOf` can emit deserializes
/// into the contract type, and every tag it spells is really in the wiring.
///
/// This is the exact wire Scenario A's Shift+Enter travels: TypeScript writes
/// `{ intent: "run_and_advance", … }` as a literal, serde reads it through
/// `#[serde(tag = "intent", rename_all = "snake_case")]` — two spellings of
/// one enum, and a respelled variant fails as a serde error inside a rejected
/// promise nobody awaits.
#[test]
fn the_run_intent_tags_the_boot_wiring_writes_deserialize_into_the_contract() {
    let wire = read("apps/desktop/src/boot/wire.tsx");

    let shapes = [
        serde_json::json!({ "intent": "run_and_advance", "doc": 1, "cursor": 20 }),
        serde_json::json!({ "intent": "current_block", "doc": 1, "cursor": 20 }),
        serde_json::json!({ "intent": "current_section", "doc": 1, "cursor": 20 }),
        serde_json::json!({ "intent": "whole_file", "doc": 1 }),
        serde_json::json!({ "intent": "clean_run", "entry": 1, "isolation": "in_process" }),
        serde_json::json!({ "intent": "all_stale", "doc": 1 }),
        serde_json::json!({ "intent": "selection", "doc": 1, "span": { "start": 20, "end": 39 } }),
        serde_json::json!({ "intent": "command_bar", "text": "summarize price mpg" }),
    ];

    for shape in shapes {
        let tag = shape["intent"].as_str().expect("every shape is tagged");
        assert!(
            wire.contains(&format!("intent: \"{tag}\"")),
            "`{tag}` is asserted here but boot/wire.tsx no longer writes it — \
             update the pair together"
        );
        let parsed: Result<RunIntent, _> = serde_json::from_value(shape.clone());
        assert!(
            parsed.is_ok(),
            "boot/wire.tsx writes {shape} and stratum_proto::exec::RunIntent \
             cannot read it: {:?}",
            parsed.err()
        );
    }
}

// ---------------------------------------------------------------------------
// The boot-handshake joint
// ---------------------------------------------------------------------------

/// `app_ready`'s reply keys are the ones the boot wiring reads.
///
/// The reply carries the `X-Stratum-Token` (without which every
/// `stratum-asset://` fetch is 403 and every inline graph a broken image) and
/// the e2e flag (without which a harness-driven window wires the live sinks
/// and double-writes the stores the bridge snapshots). Both sides are string
/// keys in different languages.
#[test]
fn the_app_ready_reply_and_the_boot_wiring_agree_on_their_keys() {
    let ipc = read("apps/desktop/src-tauri/src/ipc.rs");
    let wire = read("apps/desktop/src/boot/wire.tsx");

    for key in ["assetToken", "e2e"] {
        assert!(
            ipc.contains(&format!("\"{key}\"")),
            "ipc::app_ready no longer answers `{key}`"
        );
    }
    assert!(
        wire.contains("assetToken: string"),
        "boot/wire.tsx no longer reads `assetToken` off app_ready's reply"
    );
    assert!(
        wire.contains("e2e?: boolean"),
        "boot/wire.tsx no longer reads `e2e` off app_ready's reply"
    );
}

// ---------------------------------------------------------------------------
// The packaged CSP
// ---------------------------------------------------------------------------

/// A21's both-spellings rule, plus the wasm allowance the packaged run found.
///
/// `xtask csp-check` already enforces CONTRACTS §10.2's REQUIRED entries; this
/// test additionally pins `'wasm-unsafe-eval'` in `script-src`, which §10.2
/// does not list. Found by running Scenario A against the packaged binary:
/// WebKit refuses `WebAssembly.instantiate` under a bare `script-src 'self'`,
/// so W11a's segmenter — the thing that turns the document into blocks —
/// loaded in dev and in vitest and failed ONLY in the packaged app, as
/// `SegmenterLoadError` behind a broken first keystroke. Escalated in W17's
/// return as a §10.2 amendment; until the contract says it, this pins it.
#[test]
fn the_packaged_csp_serves_assets_and_compiles_wasm() {
    let conf = read("apps/desktop/src-tauri/tauri.conf.json");
    let csp_line = conf
        .lines()
        .find(|l| l.contains("\"csp\""))
        .expect("tauri.conf.json carries a csp");

    for directive in ["img-src", "connect-src"] {
        let at = csp_line
            .find(directive)
            .unwrap_or_else(|| panic!("no {directive} directive in the CSP"));
        let clause = csp_line[at..].split(';').next().unwrap_or("");
        for spelling in ["stratum-asset:", "http://stratum-asset.localhost"] {
            assert!(
                clause.contains(spelling),
                "{directive} lost the `{spelling}` spelling (A21): in the packaged \
                 app every inline graph becomes a broken image on at least one OS"
            );
        }
    }

    let script = csp_line
        .find("script-src")
        .map(|at| csp_line[at..].split(';').next().unwrap_or(""))
        .expect("no script-src directive in the CSP");
    assert!(
        script.contains("'wasm-unsafe-eval'"),
        "script-src lost 'wasm-unsafe-eval': the wasm segmenter cannot compile \
         under the packaged CSP and every document renders unsegmented"
    );
}

// ---------------------------------------------------------------------------
// The keystroke
// ---------------------------------------------------------------------------

/// Shift+Enter — the scenario's chord — reaches `run.blockAndAdvance` in
/// every shipped keymap preset.
///
/// The live runner already verifies what the ACTIVE trie resolves per
/// dispatch (`chord_resolves_to`); this asserts the committed resource files,
/// so a preset that drops the binding fails here even in a run driven under a
/// different preset. The presets are DELTAS (`basedOn: "modern"`, later
/// binding wins the keystroke — 06 §12.3), so a preset satisfies this either
/// by carrying the binding itself or by inheriting modern's without shadowing
/// the key.
#[test]
fn shift_enter_runs_the_block_and_advances_in_every_preset() {
    for preset in ["modern", "stata", "vscode"] {
        let keymap = read(&format!("apps/desktop/resources/keymaps/{preset}.json"));
        let bound = keymap
            .lines()
            .any(|l| l.contains("\"run.blockAndAdvance\"") && l.contains("\"Shift+Enter\""));
        let inherited =
            keymap.contains("\"basedOn\": \"modern\"") && !keymap.contains("\"Shift+Enter\"");
        assert!(
            bound || inherited,
            "{preset}.json neither binds Shift+Enter to run.blockAndAdvance nor \
             inherits modern's binding unshadowed — Scenario A's premise (spec \
             §36's north-star flow) is gone from that preset"
        );
        if preset == "modern" {
            assert!(
                bound,
                "modern.json is the base and must carry the binding itself"
            );
        }
    }
}
