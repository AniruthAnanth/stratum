//! The `MenuHost::accelerator` acceptance bullet, Windows column.
//!
//! *"`MenuHost::accelerator(ActionId, KeymapPreset)` resolves `Mod` → `⌘` on
//! macOS and `Ctrl` elsewhere."* This file is the `elsewhere`, and it runs
//! through the real [`WindowsMenuHost`] on whichever machine CI happens to be
//! — the menu host has no syscall in it, so there is no reason for the Windows
//! menu policy to be untested until someone owns a Windows box.
//!
//! The other half of the bullet — *"a CI grep asserts no `⌘` or `Ctrl+` literal
//! exists anywhere under `apps/desktop/src`"* — is
//! `stratum-platform`'s `tests/frontend_accelerator_literals.rs` and is W10's.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use stratum_platform::{
    ActionId, Key, Keymap, KeymapPreset, MenuHost, MenuModel, MenuPlacement, MenuTarget, Modifiers,
    PlatformError, PlatformId, SettingsLocation, StaticKeymap, WindowHandle,
};
use stratum_platform_windows::WindowsMenuHost;

#[test]
fn the_host_hands_back_an_already_resolved_control_key() {
    let host = WindowsMenuHost::new();
    let a = host
        .accelerator(&ActionId::from("run.block"), KeymapPreset::Modern)
        .unwrap();

    // Resolved, not logical: the MOD bit is gone and CTRL is set, so a consumer
    // cannot forget to resolve it and render "Mod+Enter" into a menu.
    assert!(!a.mods.contains(Modifiers::MOD));
    assert!(a.mods.contains(Modifiers::CTRL));
    assert!(!a.mods.contains(Modifiers::META));
    assert_eq!(a.key, Key::Enter);
    assert_eq!(a.display(PlatformId::Windows), "Ctrl+Enter");
}

/// A binding that names Ctrl explicitly must not acquire a second Ctrl when it
/// is resolved for a platform where `Mod` already *is* Ctrl. `Modifiers` is a
/// bitset, so the two collapse — which is right, and is worth an assertion
/// because getting it wrong on macOS produces `⌃⌘` and is visible, while
/// getting it wrong here is invisible.
#[test]
fn mod_and_literal_ctrl_collapse_to_one_ctrl_on_windows() {
    let host = WindowsMenuHost::with_keymap(Arc::new(StaticKeymap::new().with(
        KeymapPreset::Modern,
        "x",
        "Mod+Ctrl+K",
    )));
    let a = host
        .accelerator(&ActionId::from("x"), KeymapPreset::Modern)
        .unwrap();
    assert_eq!(a.display(PlatformId::Windows), "Ctrl+K");
}

/// The literal Windows key renders as `Win+`, not `Super+` and not `⌘`.
#[test]
fn the_literal_meta_key_is_spelled_win() {
    let host = WindowsMenuHost::with_keymap(Arc::new(StaticKeymap::new().with(
        KeymapPreset::Modern,
        "x",
        "Meta+Shift+1",
    )));
    let a = host
        .accelerator(&ActionId::from("x"), KeymapPreset::Modern)
        .unwrap();
    assert_eq!(a.display(PlatformId::Windows), "Shift+Win+1");
}

#[test]
fn an_injected_keymap_is_the_whole_map_not_an_overlay() {
    let custom: Arc<dyn Keymap> =
        Arc::new(StaticKeymap::new().with(KeymapPreset::Custom, "run.block", "Alt+Enter"));
    let host = WindowsMenuHost::with_keymap(custom);
    let block = ActionId::from("run.block");

    assert_eq!(
        host.accelerator(&block, KeymapPreset::Custom)
            .unwrap()
            .display(PlatformId::Windows),
        "Alt+Enter"
    );
    // The workspace's persisted keymap is authoritative; an action it does not
    // mention is unbound rather than falling back to the built-in table.
    assert!(host.accelerator(&block, KeymapPreset::Modern).is_none());
}

#[test]
fn an_unbound_command_is_none_not_an_empty_string() {
    let host = WindowsMenuHost::new();
    assert!(host
        .accelerator(&ActionId::from("no.such.command"), KeymapPreset::Modern)
        .is_none());
}

/// 08 §5.4: Windows returns `PerWindow` and puts Settings under Edit, with
/// `Exit` rather than `Quit` and no Services or Hide.
#[test]
fn windows_menu_policy() {
    let host = WindowsMenuHost::new();
    assert_eq!(host.placement(), MenuPlacement::PerWindow);

    let items = host.system_items();
    assert!(!items.app_menu, "Windows has no application menu");
    assert!(!items.services);
    assert!(!items.hide);
    assert!(!items.window_menu);
    assert_eq!(items.settings_location, SettingsLocation::EditMenu);
    assert_eq!(items.settings_label, "Settings…");
    assert_eq!(items.quit_label, "Exit");
}

/// The mirror image of the macOS host's refusal. Installing an
/// "application" menu bar on Windows and quietly attaching it to whichever
/// window happened to be first would look like it worked until a second window
/// opened without a menu.
#[test]
fn an_application_wide_menu_bar_is_unsupported_not_silently_reinterpreted() {
    let host = WindowsMenuHost::new();
    let err = host
        .install(&MenuModel::default(), MenuTarget::Application)
        .unwrap_err();
    assert!(err.is_unsupported(), "{err}");
}

#[test]
fn a_menu_host_with_no_sink_reports_it_instead_of_pretending() {
    let host = WindowsMenuHost::new();
    let err = host
        .install(&MenuModel::default(), MenuTarget::Window(WindowHandle(1)))
        .unwrap_err();
    assert!(matches!(err, PlatformError::BackendUnavailable(_)), "{err}");
}
