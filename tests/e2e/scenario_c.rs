//! **Scenario C — the classic workflow.** Spec §38-C, plan §2 row C, plan W16.
//!
//! > switch to Classic layout → hide inline results → enter commands in Command
//! > pane → view output in Results → use Review history → open Data Editor →
//! > run an ordinary do-file. Must feel natural.
//!
//! # What this file is, and what it is not
//!
//! The *script* for Scenario C already exists and already runs: it is
//! `stratum_e2e::fixtures::scenario_c()`, driven by `tests/e2e/harness.rs`. W25's
//! `mod.rs` says exactly what a `scenario_c.rs` adds on top of it — "an
//! additional Rust-level home for assertions the declarative script cannot
//! express" — and this file contains only those.
//!
//! The script drives a host and observes a snapshot. Every assertion below is
//! about a **joint between two artifacts that no compiler checks against each
//! other**, and every one of them fails as *silence* rather than as an error:
//!
//! | joint | the two sides |
//! |---|---|
//! | the Classic preset's views | `resources/layouts/classic.json` ↔ `PANE_IDS` in `ipc/hand.ts` |
//! | each docked pane has an owner | that same view list ↔ `panes/<id>/index.tsx` exporting `register…Pane` |
//! | the chords the script types | `resources/keymaps/{modern,stata}.json` ↔ the ids W16 registers |
//! | inline results OFF in Classic | the preset's `defaults` ↔ `Expect::InlineResultsIs("off")` |
//! | §9.9's "usable for a week" | the preset's view list ↔ the notebook-only panes |
//! | A16, no linesize control | ARCHITECTURE C44 ↔ every file W16 owns |
//! | the Results text | the script's `PaneContains` strings ↔ `tests/golden/stata18/` |
//!
//! A typo in one view name renders as a pane that is simply not there; a chord
//! bound to an id nobody registered resolves to nothing and the keystroke falls
//! through to the platform; a `defaults.inlineResults` someone "improved" to
//! `compact` makes Classic a notebook again. None of those raise an error
//! anywhere, and a scenario watching a snapshot it built from the same files
//! would report them as success.
//!
//! # Dependencies, deliberately minimal
//!
//! `serde_json` and `std`, and nothing from the e2e harness — the same choice
//! `scenario_b.rs` and `scenario_d.rs` made, and for the same reason: this file
//! has to be includable with a one-line `#[path]` from whichever crate ends up
//! compiling it, without that crate inheriting a dependency edge.
//! `tests/e2e/mod.rs` is **W25's** file and the `mod scenario_c;` line in it is
//! W25's to add; W16 does not write a byte there (R0). Verified before landing
//! by compiling this file from a scratch harness outside the tree.

use std::collections::BTreeSet;
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// Reading the two sides
// ---------------------------------------------------------------------------

/// The repository root, found by walking up to the `[workspace]` manifest.
///
/// Not `CARGO_MANIFEST_DIR` on its own: this file is compiled by whichever crate
/// `#[path]`-includes it, and those sit at different depths. Same shape as
/// `scenario_b.rs`'s and `scenario_d.rs`'s, on purpose.
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

fn exists(rel: &str) -> bool {
    workspace_root().join(rel).exists()
}

/// A layout preset, parsed. `_source` and the dockview blob stay opaque.
fn layout(id: &str) -> serde_json::Value {
    let text = read(&format!("apps/desktop/resources/layouts/{id}.json"));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{id}.json is not JSON: {e}"))
}

/// Every `views` entry in a preset's dock, at any depth of the grid.
///
/// Walks the JSON rather than modelling dockview's schema: the grid shape is
/// dockview-core's own and CONTRACTS §12 calls it "opaque to us", so a struct
/// here would be a second definition of somebody else's format.
fn docked_views(spec: &serde_json::Value) -> BTreeSet<String> {
    fn walk(node: &serde_json::Value, out: &mut BTreeSet<String>) {
        match node {
            serde_json::Value::Object(map) => {
                if let Some(serde_json::Value::Array(views)) = map.get("views") {
                    for view in views {
                        if let Some(name) = view.as_str() {
                            out.insert(name.to_owned());
                        }
                    }
                }
                for value in map.values() {
                    walk(value, out);
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    walk(item, out);
                }
            }
            _ => {}
        }
    }
    let mut out = BTreeSet::new();
    walk(spec, &mut out);
    out
}

/// The `PANE_IDS` array in `ipc/hand.ts`, as the frontend actually declares it.
fn pane_ids() -> BTreeSet<String> {
    let source = read("apps/desktop/src/ipc/hand.ts");
    let start = source
        .find("export const PANE_IDS = [")
        .expect("PANE_IDS is gone from ipc/hand.ts");
    let rest = &source[start..];
    let end = rest.find(']').expect("PANE_IDS has no closing bracket");
    rest[..end]
        .split('"')
        .skip(1)
        .step_by(2)
        .map(str::to_owned)
        .collect()
}

/// Every `command` a keymap preset binds, with its key and its `args.id`.
fn bindings(preset: &str) -> Vec<(String, String, Option<String>)> {
    let text = read(&format!("apps/desktop/resources/keymaps/{preset}.json"));
    let json: serde_json::Value =
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("{preset}.json is not JSON: {e}"));
    json["bindings"]
        .as_array()
        .unwrap_or_else(|| panic!("{preset}.json has no bindings array"))
        .iter()
        .map(|b| {
            (
                b["command"].as_str().unwrap_or_default().to_owned(),
                b["key"].as_str().unwrap_or_default().to_owned(),
                b["args"]["id"].as_str().map(str::to_owned),
            )
        })
        .collect()
}

/// The command ids W16 registers in the Command window's descriptor table.
fn command_bar_ids() -> BTreeSet<String> {
    let source = read("apps/desktop/src/commandbar/commands.ts");
    let body = source
        .split_once("const COMMAND_BAR_COMMANDS")
        .expect("COMMAND_BAR_COMMANDS is gone from commandbar/commands.ts")
        .1;
    body.match_indices("id: \"")
        .filter_map(|(at, _)| {
            let rest = &body[at + 5..];
            rest.find('"').map(|end| rest[..end].to_owned())
        })
        .collect()
}

// ---------------------------------------------------------------------------
// The Classic preset is the layout §7 describes
// ---------------------------------------------------------------------------

/// 06 §8.3's Widescreen layout: History left, Results centre with Command
/// beneath it, Variables over Properties right.
#[test]
fn classic_docks_the_seven_windows_spec_7_names() {
    let views = docked_views(&layout("classic"));
    for required in [
        "history",
        "results",
        "commandbar",
        "variables",
        "properties",
    ] {
        assert!(
            views.contains(required),
            "the Classic preset does not dock `{required}`; it docks {views:?}. \
             Spec §7 names Results, Command, Review, Variables and Properties, and \
             a view missing from the preset is a pane that is simply not there."
        );
    }
}

/// A view name that is not a `PaneId` builds a panel nothing can register into.
#[test]
fn every_docked_view_is_a_pane_id_or_the_documented_exception() {
    let ids = pane_ids();
    for preset in ["classic", "classic-sidebar", "modern", "focus"] {
        for view in docked_views(&layout(preset)) {
            // `commandbar` is the one dock component that is not a `PaneId`:
            // CONTRACTS §12's list does not name it, because the command bar is
            // a `defaults` setting present in every layout rather than a pane
            // you can close. `dock/panes.ts` documents exactly this exception.
            assert!(
                view == "commandbar" || ids.contains(&view),
                "{preset}.json docks `{view}`, which is neither a PaneId nor the \
                 documented `commandbar` exception. PANE_IDS is {ids:?}."
            );
        }
    }
}

/// Each docked pane has a module that registers it. Absent = an empty panel.
#[test]
fn every_pane_classic_docks_has_a_registrar() {
    for view in docked_views(&layout("classic")) {
        let module = if view == "commandbar" {
            "apps/desktop/src/commandbar/index.tsx".to_owned()
        } else {
            format!("apps/desktop/src/panes/{view}/index.tsx")
        };
        assert!(
            exists(&module),
            "Classic docks `{view}` but {module} does not exist, so the panel \
             mounts an empty host (dock/panes.ts). Owners are in docs/ownership.toml."
        );

        // W14's `results` pane deliberately exports a props-taking component
        // rather than a registrar (its header says so, and `e2e/bridge.ts`
        // records it); every other pane Classic docks ships `register…Pane`.
        if view == "results" {
            continue;
        }
        let source = read(&module);
        assert!(
            source.contains("Pane(") && source.contains("export function register"),
            "{module} exports no `register…Pane`, so `e2e/bridge.ts`'s generic \
             mount cannot bring `{view}` up and Scenario C's step on it is owed."
        );
    }
}

// ---------------------------------------------------------------------------
// Inline results are OFF, and Classic stays classic (§25B, 06 §9.9)
// ---------------------------------------------------------------------------

/// Spec §25B: "inline results OFF by default". The script asserts the *snapshot*
/// says `off`; this asserts the *preset* is why.
#[test]
fn classic_defaults_inline_results_off() {
    let spec = layout("classic");
    assert_eq!(
        spec["defaults"]["inlineResults"].as_str(),
        Some("off"),
        "the Classic preset must default inline results to off (spec §25B, 06 §8.3). \
         Switching to Classic is what hides them, so this field IS the feature."
    );
    assert_eq!(
        spec["defaults"]["docView"].as_bool(),
        Some(false),
        "docView is a notebook affordance; Classic must not start in it."
    );
    assert_eq!(
        spec["defaults"]["commandBar"].as_str(),
        Some("pane"),
        "in Classic the command bar IS the Command pane (06 §10)."
    );
}

/// 06 §9.9: "Classic must be possible to use for a week without ever discovering
/// the notebook features. That is the acceptance bar for §38-C."
#[test]
fn classic_docks_no_notebook_only_pane() {
    let views = docked_views(&layout("classic"));
    for notebook_only in ["sections", "repro", "compare", "assistant"] {
        assert!(
            !views.contains(notebook_only),
            "the Classic preset docks `{notebook_only}`, a notebook-only pane. \
             06 §9.9: Classic must be usable for a week without discovering the \
             notebook features, and a docked pane is a discovered one."
        );
    }
}

/// The sidebar variant is Stata's other shipped arrangement, not a third product.
#[test]
fn classic_sidebar_is_the_same_panes_in_stata_s_other_arrangement() {
    let wide = docked_views(&layout("classic"));
    let sidebar = docked_views(&layout("classic-sidebar"));
    assert_eq!(
        wide, sidebar,
        "06 §8.3: `classic-sidebar` matches Stata's Sidebar layout — History \
         becomes a third tab beside Variables/Properties. Same panes, different \
         geometry; a different SET of panes would be a different product."
    );
    assert_eq!(
        layout("classic-sidebar")["defaults"]["inlineResults"].as_str(),
        Some("off"),
        "both Classic arrangements are Classic."
    );
}

// ---------------------------------------------------------------------------
// The chords Scenario C types resolve to commands somebody registered
// ---------------------------------------------------------------------------

/// `Mod+Alt+2` is the step-one chord of the script, and `Mod+L` is step three.
#[test]
fn the_scripted_chords_are_bound_to_the_ids_the_script_names() {
    let modern = bindings("modern");
    let has = |command: &str, key: &str, id: Option<&str>| {
        modern
            .iter()
            .any(|(c, k, a)| c == command && k == key && a.as_deref() == id)
    };
    assert!(
        has("layout.apply", "Mod+Alt+2", Some("classic")),
        "Scenario C step 1 types Mod+Alt+2 expecting the Classic layout; \
         modern.json binds {modern:?}"
    );
    assert!(
        has("commandbar.focus", "Mod+L", None),
        "Scenario C step 3 types Mod+L expecting the Command window to take focus"
    );
}

/// PgUp/PgDn/Tab are the Stata preset's, and W16 is what makes them real.
#[test]
fn the_stata_preset_binds_only_ids_the_command_window_registers() {
    let registered = command_bar_ids();
    for (command, key, _) in bindings("stata") {
        if !command.starts_with("history.")
            && !command.starts_with("commandbar.")
            && !command.starts_with("stata.")
        {
            continue; // W13's run.*, W12's pane.*/window.* — not this unit's.
        }
        assert!(
            registered.contains(&command),
            "stata.json binds {key} to `{command}`, which `commandbar/commands.ts` \
             does not register. An unregistered id resolves to nothing and the \
             keystroke falls through to the platform — silently."
        );
    }

    // The three the manual makes non-negotiable, by key rather than by id.
    let stata = bindings("stata");
    for (key, command) in [
        ("PageUp", "history.previous"),
        ("PageDown", "history.next"),
        ("Tab", "commandbar.complete"),
    ] {
        assert!(
            stata.iter().any(|(c, k, _)| c == command && k == key),
            "[U] 10.5/10.6: {key} must be {command} in the Stata preset"
        );
    }
}

// ---------------------------------------------------------------------------
// A16 — no linesize control is offered
// ---------------------------------------------------------------------------

/// Plan W16: "**No linesize control is offered** (A16). v1 renders classic output
/// at 80 columns and rejects any other `set linesize` with rc 10; a Classic
/// preset that exposed a width control the engine ignores would be worse than not
/// having one."
///
/// The check is a grep over every file this unit owns, because the failure mode
/// is a helpful person adding a width slider to the Results pane's preferences.
#[test]
fn no_pane_this_unit_owns_offers_a_linesize_control() {
    let owned = [
        "apps/desktop/src/commandbar",
        "apps/desktop/src/log",
        "apps/desktop/src/panes/history",
        "apps/desktop/src/panes/variables",
        "apps/desktop/src/panes/properties",
        "apps/desktop/src/panes/project",
        "apps/desktop/src/panes/viewer",
    ];
    let root = workspace_root();
    for dir in owned {
        let Ok(entries) = std::fs::read_dir(root.join(dir)) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("md") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            // The word may appear in a comment explaining why there is no
            // control; what may not appear is a control.
            for line in text.lines() {
                let lower = line.to_ascii_lowercase();
                if !lower.contains("linesize") {
                    continue;
                }
                let is_prose = line.trim_start().starts_with("//")
                    || line.trim_start().starts_with('*')
                    || line.trim_start().starts_with("/*");
                assert!(
                    is_prose,
                    "{} offers `linesize` in code: {line}\nA16 pins classic output \
                     to 80 columns and `set linesize n != 80` is rc 10, so a width \
                     control here would be a setting the engine refuses.",
                    path.display()
                );
            }
        }
    }
}

/// And the decision it depends on is still the decision.
#[test]
fn a16_still_says_eighty_and_rc_ten() {
    // The design documents are not part of the published source tree; where
    // they are absent there is nothing to check against, and saying so beats a
    // panic that reads as a product failure.
    if !exists("docs/ARCHITECTURE.md") {
        eprintln!("skipped: docs/ARCHITECTURE.md is not in this tree");
        return;
    }
    let architecture = read("docs/ARCHITECTURE.md");
    assert!(
        architecture.contains("STRATUM0010"),
        "C44/A16's diagnostic id is gone from ARCHITECTURE.md; if the linesize \
         rule changed, the Classic preset's missing width control becomes a \
         missing feature rather than a deliberate omission."
    );
}

// ---------------------------------------------------------------------------
// The Results text the script asserts on is the golden's own text
// ---------------------------------------------------------------------------

/// The script asserts `Expect::PaneContains("results", "6165.257")` and
/// `("results", "1978 automobile data")`. Those two strings are only meaningful
/// if they are quotations of the oracle — the Stata licence on this machine has
/// expired, so the committed log is the only authority left.
#[test]
fn the_strings_scenario_c_expects_in_results_come_from_the_golden_log() {
    let golden = read("tests/golden/stata18/core_surface.log");
    for quoted in ["1978 automobile data", "6165.257"] {
        assert!(
            golden.contains(quoted),
            "Scenario C expects `{quoted}` in the Results pane, but it does not \
             appear in tests/golden/stata18/core_surface.log. An expectation that \
             is not a quotation of the golden is an invention."
        );
    }
}

/// 06 §9.2 renders the echo as `. summarize mpg` — the leading `. ` is Stata's
/// own and the golden is where it is written down.
#[test]
fn the_command_echo_in_the_golden_is_the_form_results_reproduces() {
    let golden = read("tests/golden/stata18/core_surface.log");
    assert!(
        golden.contains("\n. describe"),
        "the golden's command echo is `. <command>` at column 0; the Results \
         pane's classic mode reproduces exactly that (06 §9.2)."
    );
    // And the scrollback is 80-column classic text, not a re-rendered view: a
    // `regress` table in the golden is the width A16 pins.
    assert!(
        golden.lines().any(|l| l.starts_with("------")),
        "the golden contains classic rule lines; if it does not, the file being \
         read is not the classic-text oracle this pane is built against."
    );
}
