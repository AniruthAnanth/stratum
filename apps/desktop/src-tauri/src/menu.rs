//! Native menus, built from W10's platform model (spec §33, 08 §5.4).
//!
//! The division of labour is the one `stratum_platform::menus` documents:
//! the platform layer owns *policy* — placement, system-item conventions and
//! accelerator resolution — and the application shell owns the toolkit. Tauri
//! already owns `NSApp.mainMenu`, so this module is the toolkit half: it builds
//! a [`MenuModel`] with accelerators resolved through
//! `stratum_platform_host::host().menus()`, converts it to a `tauri::menu::Menu`,
//! and routes clicks back to the focused window as a `stratum://menu-action`
//! event that the frontend's command registry dispatches — the same registry
//! the keymap and the palette use, which is what keeps a menu item and its
//! keystroke from ever meaning different things.

use stratum_platform::menus::{
    ActionId, KeymapPreset, MenuItem, MenuModel, MenuRole, SystemMenuItems,
};
use stratum_platform::PlatformId;
use tauri::menu::{
    Menu, MenuBuilder, MenuItemBuilder, PredefinedMenuItem, Submenu, SubmenuBuilder,
};
use tauri::{AppHandle, Emitter, Manager, Runtime};

/// The event the frontend listens on for menu clicks.
pub const MENU_EVENT: &str = "stratum://menu-action";

/// One menu action: id + label. The accelerator is resolved per platform at
/// build time, so the table never spells `⌘` or `Ctrl+`.
const FILE: &[(&str, &str)] = &[
    ("file.newDo", "New Do-file"),
    ("file.open", "Open…"),
    ("file.save", "Save"),
    ("file.saveAs", "Save As…"),
];

const RUN: &[(&str, &str)] = &[
    ("run.block", "Run Block"),
    ("run.blockAndAdvance", "Run Block and Advance"),
    ("run.selection", "Run Selection"),
    ("run.fromHere", "Run From Here"),
    ("run.fileClean", "Run File From Clean State"),
    ("run.allStale", "Run All Stale"),
    ("run.break", "Break"),
];

const VIEW: &[(&str, &str)] = &[
    ("layout.modern", "Modern Layout"),
    ("layout.classic", "Classic Layout"),
    ("layout.focus", "Focus Mode"),
    ("view.toggleAssistant", "Toggle Assistant"),
    ("view.cycleInlineMode", "Cycle Inline Results"),
    ("view.toggleDocument", "Toggle Document View"),
    ("view.modelComparison", "Model Comparison"),
];

const DATA: &[(&str, &str)] = &[
    ("data.editorBrowse", "Data Editor (Browse)"),
    ("data.editorEdit", "Data Editor (Edit)"),
];

/// Build the pure model. Separate from the toolkit conversion so a test can
/// assert the §33 invariants — accelerators resolved per platform, Settings in
/// the platform's own place — without a display server.
#[must_use]
pub fn model(preset: KeymapPreset) -> MenuModel {
    let platform = stratum_platform_host::host();
    let menus = platform.menus();
    let system = SystemMenuItems::for_platform(platform.id());

    let action = |id: &str, label: &str| -> MenuItem {
        MenuItem::Action {
            id: ActionId::from(id),
            label: label.to_owned(),
            accel: menus.accelerator(&ActionId::from(id), preset),
            enabled: true,
            checked: None,
            role: None,
        }
    };
    let submenu = |label: &str, table: &[(&str, &str)]| -> MenuItem {
        MenuItem::Submenu {
            label: label.to_owned(),
            items: table.iter().map(|(id, l)| action(id, l)).collect(),
            role: None,
        }
    };

    let mut items = Vec::new();
    let mut file_items: Vec<MenuItem> = FILE.iter().map(|(id, l)| action(id, l)).collect();
    if system.settings_location == stratum_platform::menus::SettingsLocation::FilePreferences {
        file_items.push(MenuItem::Separator);
        file_items.push(MenuItem::Action {
            id: ActionId::from("app.settings"),
            label: system.settings_label.to_owned(),
            accel: None,
            enabled: true,
            checked: None,
            role: Some(MenuRole::Settings),
        });
    }
    items.push(MenuItem::Submenu {
        label: "File".to_owned(),
        items: file_items,
        role: None,
    });
    items.push(submenu("Run", RUN));
    items.push(submenu("View", VIEW));
    items.push(submenu("Data", DATA));
    MenuModel { items }
}

/// Convert the model into Tauri's menu and install it as the app menu.
/// On macOS Tauri owns the global bar (`MenuPlacement::GlobalMenuBar`); on
/// Windows/Linux the same menu attaches per window, which is the
/// `MenuPlacement::PerWindow` policy the platform reports.
pub fn install<R: Runtime>(app: &AppHandle<R>, preset: KeymapPreset) -> tauri::Result<()> {
    let platform = stratum_platform_host::host();
    let menu = build_tauri_menu(app, &model(preset), platform.id())?;
    app.set_menu(menu)?;
    app.on_menu_event(|app, event| {
        let id = event.id().0.clone();
        // Route to the focused window; the frontend's registry dispatches it.
        let target = app
            .webview_windows()
            .into_iter()
            .find(|(_, w)| w.is_focused().unwrap_or(false))
            .map(|(label, _)| label)
            .unwrap_or_else(|| "main".to_owned());
        let _ = app.emit_to(target, MENU_EVENT, id);
    });
    Ok(())
}

fn build_tauri_menu<R: Runtime>(
    app: &AppHandle<R>,
    model: &MenuModel,
    platform: PlatformId,
) -> tauri::Result<Menu<R>> {
    let mut builder = MenuBuilder::new(app);

    // macOS: the application menu with its localised system items comes first.
    if platform == PlatformId::MacOs {
        let app_menu = SubmenuBuilder::new(app, "Stratum")
            .about(None)
            .separator()
            .item(&PredefinedMenuItem::hide(app, None)?)
            .item(&PredefinedMenuItem::hide_others(app, None)?)
            .item(&PredefinedMenuItem::show_all(app, None)?)
            .separator()
            .item(&PredefinedMenuItem::quit(app, None)?)
            .build()?;
        builder = builder.item(&app_menu);
    }

    for item in &model.items {
        if let MenuItem::Submenu { label, items, .. } = item {
            let sub = build_submenu(app, label, items, platform)?;
            builder = builder.item(&sub);
        }
    }

    // The OS-wired editing verbs: without these, ⌘C/⌘V do nothing in a webview
    // text field on macOS.
    let edit = SubmenuBuilder::new(app, "Edit")
        .undo()
        .redo()
        .separator()
        .cut()
        .copy()
        .paste()
        .select_all()
        .build()?;
    builder = builder.item(&edit);

    if SystemMenuItems::for_platform(platform).window_menu {
        let window = SubmenuBuilder::new(app, "Window")
            .minimize()
            .fullscreen()
            .build()?;
        builder = builder.item(&window);
    }

    builder.build()
}

fn build_submenu<R: Runtime>(
    app: &AppHandle<R>,
    label: &str,
    items: &[MenuItem],
    platform: PlatformId,
) -> tauri::Result<Submenu<R>> {
    let mut builder = SubmenuBuilder::new(app, label);
    for item in items {
        match item {
            MenuItem::Action {
                id,
                label,
                accel,
                enabled,
                ..
            } => {
                let mut b = MenuItemBuilder::with_id(id.as_str(), label).enabled(*enabled);
                if let Some(spelled) = accel.and_then(|a| tauri_accelerator(a, platform)) {
                    b = b.accelerator(spelled);
                }
                builder = builder.item(&b.build(app)?);
            }
            MenuItem::Separator => {
                builder = builder.separator();
            }
            MenuItem::Submenu { label, items, .. } => {
                let sub = build_submenu(app, label, items, platform)?;
                builder = builder.item(&sub);
            }
        }
    }
    builder.build()
}

/// Tauri accelerator syntax (`CmdOrCtrl+Shift+Enter`) from the platform's
/// resolved accelerator. `Modifiers::MOD` is spelled `CmdOrCtrl`, which Tauri
/// resolves the same way §33 does — so the two layers cannot disagree.
/// `None` for a key Tauri's parser has no spelling for; the item then ships
/// without a menu accelerator and the keystroke stays the frontend trie's.
fn tauri_accelerator(
    accel: stratum_platform::menus::Accelerator,
    _platform: PlatformId,
) -> Option<String> {
    use stratum_platform::menus::{Key, Modifiers};
    let mut parts: Vec<&str> = Vec::new();
    let m = accel.mods;
    if m.contains(Modifiers::MOD) {
        parts.push("CmdOrCtrl");
    }
    if m.contains(Modifiers::CTRL) {
        parts.push("Ctrl");
    }
    if m.contains(Modifiers::ALT) {
        parts.push("Alt");
    }
    if m.contains(Modifiers::SHIFT) {
        parts.push("Shift");
    }
    if m.contains(Modifiers::META) {
        parts.push("Super");
    }
    let key = match accel.key {
        Key::Char(c) if c.is_ascii_alphanumeric() => c.to_ascii_uppercase().to_string(),
        Key::Char('.') => ".".to_owned(),
        Key::Char('/') => "/".to_owned(),
        Key::Char('`') => "`".to_owned(),
        Key::Char(_) => return None,
        Key::Enter => "Enter".to_owned(),
        Key::Escape => "Escape".to_owned(),
        Key::Tab => "Tab".to_owned(),
        Key::Space => "Space".to_owned(),
        Key::Backspace => "Backspace".to_owned(),
        Key::Delete => "Delete".to_owned(),
        Key::Up => "Up".to_owned(),
        Key::Down => "Down".to_owned(),
        Key::Left => "Left".to_owned(),
        Key::Right => "Right".to_owned(),
        Key::Home => "Home".to_owned(),
        Key::End => "End".to_owned(),
        Key::PageUp => "PageUp".to_owned(),
        Key::PageDown => "PageDown".to_owned(),
        Key::F(n) => format!("F{n}"),
    };
    let mut out = parts.join("+");
    if !out.is_empty() {
        out.push('+');
    }
    out.push_str(&key);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// §33: the frontend (and this table) never hardcodes ⌘/Ctrl — the
    /// accelerator comes from the platform layer already resolved.
    #[test]
    fn run_block_and_advance_is_shift_enter_from_the_platform_tables() {
        let m = model(KeymapPreset::Modern);
        let run = m.items.iter().find_map(|item| match item {
            MenuItem::Submenu { label, items, .. } if label == "Run" => Some(items),
            _ => None,
        });
        let advance = run
            .expect("a Run menu")
            .iter()
            .find_map(|item| match item {
                MenuItem::Action { id, accel, .. } if id.as_str() == "run.blockAndAdvance" => {
                    Some(*accel)
                }
                _ => None,
            })
            .expect("run.blockAndAdvance is in the menu");
        let accel = advance.expect("it has an accelerator");
        // Shift+Enter (06 §12.2). The display form is platform-specific; the
        // logical form is not.
        assert!(accel
            .mods
            .contains(stratum_platform::menus::Modifiers::SHIFT));
    }

    #[test]
    fn the_tauri_spelling_uses_cmdorctrl_for_the_logical_modifier() {
        let accel =
            stratum_platform::menus::Accelerator::parse("Mod+Shift+Enter").expect("a valid spec");
        let spelled = tauri_accelerator(accel, PlatformId::MacOs);
        assert_eq!(spelled.as_deref(), Some("CmdOrCtrl+Shift+Enter"));
    }
}
