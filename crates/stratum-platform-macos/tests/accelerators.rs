//! The `MenuHost::accelerator` acceptance bullet.
//!
//! `Mod` → `⌘` on macOS and `Ctrl` elsewhere. The macOS half is asserted
//! through the real [`MacosMenuHost`]; the other two are asserted through the
//! pure resolver, because a CI job that only runs on macOS still has to be able
//! to fail when someone breaks the Windows rendering.
#![cfg(target_os = "macos")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use stratum_platform::{
    Accelerator, ActionId, Key, Keymap, KeymapPreset, MenuHost, MenuPlacement, Modifiers,
    PlatformId, SettingsLocation, StaticKeymap,
};
use stratum_platform_macos::MacosMenuHost;

#[test]
fn the_host_hands_back_an_already_resolved_command_key() {
    let host = MacosMenuHost::new();
    let a = host
        .accelerator(&ActionId::from("run.block"), KeymapPreset::Modern)
        .unwrap();

    // Resolved, not logical: the MOD bit is gone and META is set, so a consumer
    // cannot forget to resolve it and render "Mod+Enter" into a menu.
    assert!(!a.mods.contains(Modifiers::MOD));
    assert!(a.mods.contains(Modifiers::META));
    assert_eq!(a.key, Key::Enter);
    assert_eq!(a.display(PlatformId::MacOs), "\u{2318}\u{21a9}");
}

/// The same binding on the other two platforms, from a macOS test run.
#[test]
fn the_same_binding_renders_as_ctrl_off_the_mac() {
    let logical = Accelerator::parse("Mod+Shift+K").unwrap();
    assert_eq!(logical.display(PlatformId::MacOs), "\u{21e7}\u{2318}K");
    assert_eq!(logical.display(PlatformId::Windows), "Ctrl+Shift+K");
    assert_eq!(logical.display(PlatformId::Linux), "Ctrl+Shift+K");
}

#[test]
fn an_injected_keymap_wins_over_the_builtin_table() {
    let custom: Arc<dyn Keymap> =
        Arc::new(StaticKeymap::new().with(KeymapPreset::Custom, "run.block", "Ctrl+Alt+Enter"));
    let host = MacosMenuHost::with_keymap(custom);
    let block = ActionId::from("run.block");

    let a = host.accelerator(&block, KeymapPreset::Custom).unwrap();
    assert_eq!(a.display(PlatformId::MacOs), "\u{2303}\u{2325}\u{21a9}");
    // The injected map is the whole map, not an overlay: an action it does not
    // mention is unbound, which is what lets the workspace's persisted keymap
    // be authoritative.
    assert!(host.accelerator(&block, KeymapPreset::Modern).is_none());
}

#[test]
fn an_unbound_command_is_none_not_an_empty_string() {
    let host = MacosMenuHost::new();
    assert!(host
        .accelerator(&ActionId::from("no.such.command"), KeymapPreset::Modern)
        .is_none());
}

#[test]
fn macos_menu_policy() {
    let host = MacosMenuHost::new();
    assert_eq!(host.placement(), MenuPlacement::GlobalMenuBar);
    let items = host.system_items();
    assert!(items.app_menu && items.services && items.hide && items.window_menu);
    assert_eq!(items.settings_location, SettingsLocation::AppMenu);
    assert_eq!(items.quit_label, "Quit Stratum");
}
