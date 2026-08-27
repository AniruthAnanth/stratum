//! The `MenuHost::accelerator` acceptance bullet, Linux half.
//!
//! `Mod` → `Ctrl` here and `⌘` on macOS, resolved in Rust so that the CI grep
//! asserting no `⌘` or `Ctrl+` literal exists under `apps/desktop/src` has
//! something correct to point people at.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use stratum_platform::{
    ActionId, Key, Keymap, KeymapPreset, MenuHost, MenuModel, MenuPlacement, MenuTarget, Modifiers,
    PlatformError, PlatformId, SettingsLocation, StaticKeymap, WindowHandle,
};
use stratum_platform_linux::LinuxMenuHost;

#[test]
fn the_host_hands_back_an_already_resolved_control_key() {
    let host = LinuxMenuHost::new();
    let a = host
        .accelerator(&ActionId::from("run.block"), KeymapPreset::Modern)
        .unwrap();

    // Resolved, not logical: the MOD bit is gone and CTRL is set, so a consumer
    // cannot forget to resolve it and render "Mod+Enter" into a menu.
    assert!(!a.mods.contains(Modifiers::MOD));
    assert!(a.mods.contains(Modifiers::CTRL));
    assert!(
        !a.mods.contains(Modifiers::META),
        "Linux has no Command key"
    );
    assert_eq!(a.key, Key::Enter);
    assert_eq!(a.display(PlatformId::Linux), "Ctrl+Enter");
}

/// A binding that names Ctrl explicitly must not become a double Ctrl once
/// `Mod` has also resolved to it — that is the one way the two-bit design can
/// go wrong on this platform specifically.
#[test]
fn mod_and_an_explicit_ctrl_collapse_to_one_ctrl() {
    let custom: Arc<dyn Keymap> =
        Arc::new(StaticKeymap::new().with(KeymapPreset::Custom, "x", "Mod+Ctrl+K"));
    let host = LinuxMenuHost::with_keymap(custom);
    let a = host
        .accelerator(&ActionId::from("x"), KeymapPreset::Custom)
        .unwrap();
    assert_eq!(a.display(PlatformId::Linux), "Ctrl+K");
}

#[test]
fn an_injected_keymap_is_the_whole_map_not_an_overlay() {
    let custom: Arc<dyn Keymap> =
        Arc::new(StaticKeymap::new().with(KeymapPreset::Custom, "run.block", "Alt+Enter"));
    let host = LinuxMenuHost::with_keymap(custom);
    let block = ActionId::from("run.block");

    assert_eq!(
        host.accelerator(&block, KeymapPreset::Custom)
            .unwrap()
            .display(PlatformId::Linux),
        "Alt+Enter"
    );
    // What lets the workspace's persisted keymap be authoritative.
    assert!(host.accelerator(&block, KeymapPreset::Modern).is_none());
}

#[test]
fn an_unbound_command_is_none_not_an_empty_string() {
    let host = LinuxMenuHost::new();
    assert!(host
        .accelerator(&ActionId::from("no.such.command"), KeymapPreset::Modern)
        .is_none());
}

#[test]
fn linux_menu_policy() {
    let host = LinuxMenuHost::new();
    assert_eq!(host.placement(), MenuPlacement::PerWindow);
    let items = host.system_items();
    assert!(!items.app_menu && !items.services && !items.hide && !items.window_menu);
    assert_eq!(items.settings_location, SettingsLocation::FilePreferences);
    assert_eq!(items.settings_label, "Preferences");
    assert_eq!(items.quit_label, "Quit");
}

/// The mirror of the macOS rule. There is no application-wide menu bar here,
/// and quietly attaching the model to some arbitrary window would look like it
/// worked until the user opened a second one (§26).
#[test]
fn an_application_scoped_install_is_refused_rather_than_guessed_at() {
    let host = LinuxMenuHost::new();
    let err = host
        .install(&MenuModel::default(), MenuTarget::Application)
        .unwrap_err();
    assert!(err.is_unsupported(), "{err}");
}

#[test]
fn a_menu_host_with_no_sink_reports_it_instead_of_pretending() {
    let host = LinuxMenuHost::new();
    let err = host
        .install(&MenuModel::default(), MenuTarget::Window(WindowHandle(1)))
        .unwrap_err();
    assert!(matches!(err, PlatformError::BackendUnavailable(_)), "{err}");
}
