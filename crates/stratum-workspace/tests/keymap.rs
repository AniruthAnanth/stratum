//! Keymap presets — spec §33, design 06 §12, plan W26's sixth acceptance
//! bullet.
//!
//! Two things are being asserted. That presets and the user's overlay persist
//! and reload; and that **the frontend never has to hardcode ⌘ or Ctrl** — every
//! accelerator a menu draws comes out of `menu_accelerator`, resolved for the
//! platform in Rust, so the menu bar, the command palette and the keymap editor
//! cannot disagree.

use stratum_workspace::keymap::{
    accelerator, menu_accelerator, preset_bindings, KeyBinding, KeymapPreset, KeymapStore,
    Platform, Source,
};

mod common;
use common::{project_at, tmp};

fn store(root: &camino::Utf8Path) -> KeymapStore {
    KeymapStore::new(root.join("resources/keymaps"), root.join("config/keymaps"))
}

fn user(command: &str, key: &str) -> KeyBinding {
    KeyBinding {
        command: command.to_owned(),
        key: key.to_owned(),
        when: None,
        args: None,
        source: Source::User,
    }
}

#[test]
fn mod_resolves_to_the_platform_convention() {
    // Spec §33: "Respect platform conventions (Cmd on macOS, Ctrl elsewhere)".
    for (raw, mac, other) in [
        ("Mod+Enter", "⌘Enter", "Ctrl+Enter"),
        ("Mod+Shift+P", "⌘⇧P", "Ctrl+Shift+P"),
        ("Mod+Alt+1", "⌘⌥1", "Ctrl+Alt+1"),
        ("Mod+K Mod+S", "⌘K ⌘S", "Ctrl+K Ctrl+S"),
        ("F1", "F1", "F1"),
    ] {
        assert_eq!(stratum_workspace::keymap::render(raw, Platform::Mac), mac);
        assert_eq!(
            stratum_workspace::keymap::render(raw, Platform::Other),
            other
        );
    }
}

#[test]
fn all_three_presets_of_spec_33_exist_and_bind_the_core_commands() {
    assert_eq!(KeymapPreset::ALL.len(), 3);
    for p in KeymapPreset::ALL {
        let v = preset_bindings(p);
        for command in [
            "run.block",
            "run.selection",
            "run.fileClean",
            "run.break",
            "palette.commands",
            "commandBar.focus",
        ] {
            assert!(
                accelerator(&v, command, Platform::Mac).is_some(),
                "{} does not bind {command}",
                p.id()
            );
        }
    }
    // The Stata-like preset is a *delta*, so it differs where it should and
    // agrees where it should.
    assert_ne!(
        accelerator(
            &preset_bindings(KeymapPreset::Stata),
            "run.block",
            Platform::Other
        ),
        accelerator(
            &preset_bindings(KeymapPreset::Modern),
            "run.block",
            Platform::Other
        ),
    );
}

#[test]
fn presets_persist_and_reload_with_the_users_overlay_on_top() {
    let (_t, root) = tmp();
    let s = store(&root);

    // Nothing customised yet.
    assert!(s.overlay().unwrap().is_empty());
    let stock = accelerator(
        &s.load(KeymapPreset::Modern).unwrap(),
        "run.block",
        Platform::Other,
    );
    assert_eq!(stock.as_deref(), Some("Ctrl+Enter"));

    // The user rebinds one command and adds one of their own.
    let mut bindings = s.load(KeymapPreset::Modern).unwrap();
    bindings.push(KeyBinding {
        when: Some("editorFocus && !running".into()),
        ..user("run.block", "F9")
    });
    bindings.push(user("repro.check", "Mod+Shift+U"));
    let path = s.save(&bindings).unwrap();
    assert!(path.ends_with("user.json"));

    // Only the user layer is on disk — freezing today's preset into the user's
    // file is how somebody stops receiving keymap improvements by accident.
    let overlay = s.overlay().unwrap();
    assert_eq!(overlay.len(), 2);
    assert!(overlay.iter().all(|b| b.source == Source::User));
    assert_eq!(overlay[0].when.as_deref(), Some("editorFocus && !running"));

    // Reload: the user's binding wins, the rest of the preset is intact.
    let reloaded = s.load(KeymapPreset::Modern).unwrap();
    assert_eq!(
        accelerator(&reloaded, "run.block", Platform::Other).as_deref(),
        Some("F9")
    );
    assert_eq!(
        accelerator(&reloaded, "repro.check", Platform::Mac).as_deref(),
        Some("⌘⇧U")
    );
    assert_eq!(
        accelerator(&reloaded, "palette.commands", Platform::Other).as_deref(),
        Some("Ctrl+Shift+P")
    );

    // Switching preset keeps the overlay: it is the user's, not the preset's.
    let stata = s.load(KeymapPreset::Stata).unwrap();
    assert_eq!(
        accelerator(&stata, "run.block", Platform::Other).as_deref(),
        Some("F9")
    );
}

#[test]
fn menu_accelerator_is_the_only_thing_that_needs_to_know_about_cmd() {
    let (_t, root) = tmp();
    let s = store(&root);

    assert_eq!(
        menu_accelerator(&s, "run.block", KeymapPreset::Modern, Platform::Mac)
            .unwrap()
            .as_deref(),
        Some("⌘Enter")
    );
    assert_eq!(
        menu_accelerator(&s, "run.block", KeymapPreset::Modern, Platform::Other)
            .unwrap()
            .as_deref(),
        Some("Ctrl+Enter")
    );
    // An unbound command draws no accelerator — `None`, not an empty string that
    // renders as a stray separator in a menu.
    assert_eq!(
        menu_accelerator(&s, "nothing.bound", KeymapPreset::Modern, Platform::Mac).unwrap(),
        None
    );
}

#[test]
fn the_shipped_resource_file_is_the_authority_when_it_is_there() {
    // Design 06 §12.1 ships the presets as JSON in `resources/keymaps/`. This
    // crate is the persistence layer under them, so a resource file beats the
    // built-in floor, and a missing or empty one falls back to it rather than
    // leaving the user with a dead keyboard.
    let (_t, root) = tmp();
    let s = store(&root);
    std::fs::create_dir_all(&s.presets).unwrap();
    std::fs::write(
        s.presets.join("modern.json"),
        br#"{ "schema": 1, "id": "modern", "name": "Modern",
              "bindings": [ { "command": "run.block", "key": "Mod+Alt+R" } ] }"#,
    )
    .unwrap();

    assert_eq!(
        menu_accelerator(&s, "run.block", KeymapPreset::Modern, Platform::Other)
            .unwrap()
            .as_deref(),
        Some("Ctrl+Alt+R")
    );
    // stata.json is absent, so that preset still resolves.
    assert!(
        menu_accelerator(&s, "run.block", KeymapPreset::Stata, Platform::Other)
            .unwrap()
            .is_some()
    );
}

#[test]
fn a_malformed_overlay_is_reported_rather_than_silently_discarded() {
    let (_t, root) = tmp();
    let s = store(&root);
    std::fs::create_dir_all(&s.dir).unwrap();
    std::fs::write(s.overlay_path(), b"{}").unwrap();
    assert!(s.overlay().is_err());
    assert!(s.load(KeymapPreset::Modern).is_err());
}

#[test]
fn the_command_surface_reaches_the_same_store() {
    let (_t, root) = tmp();
    let ws = project_at(&root);

    let mut bindings = ws.keymap_load(KeymapPreset::Vscode).unwrap();
    bindings.push(user("assistant.toggle", "Mod+Shift+A"));
    ws.keymap_save(&bindings).unwrap();

    let reloaded = ws.keymap_load(KeymapPreset::Vscode).unwrap();
    assert!(reloaded
        .iter()
        .any(|b| b.command == "assistant.toggle" && b.key == "Mod+Shift+A"));
    assert!(ws
        .menu_accelerator("assistant.toggle", KeymapPreset::Vscode)
        .unwrap()
        .is_some());
}
