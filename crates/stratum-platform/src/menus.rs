//! Menus and keyboard accelerators — 08 §5.4, spec §33.
//!
//! [`MenuModel`] is a pure data tree; the desktop builds it with no OS call and
//! hands it to [`MenuHost::install`]. macOS reports
//! [`MenuPlacement::GlobalMenuBar`] and injects the application menu; Windows
//! and Linux report [`MenuPlacement::PerWindow`] and move Settings under Edit
//! and File → Preferences respectively.
//!
//! # The frontend never writes `⌘` or `Ctrl+`
//!
//! It asks [`MenuHost::accelerator`], which returns an accelerator with its
//! logical [`Modifiers::MOD`] already **resolved** for the host — `⌘` on macOS,
//! `Ctrl` everywhere else — and renders it with [`Accelerator::display`]. A CI
//! grep asserts that no `⌘` or `Ctrl+` literal exists anywhere under
//! `apps/desktop/src`; `tests/accelerators.rs` asserts the other half, that the
//! resolution is right for all three platforms, from whichever one CI happens
//! to be running on.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{PlatformId, Result};

/// A logical command id: `"run.block"`, `"view.toggleAssistant"`.
///
/// Owned rather than `&'static str` because the `menu_accelerator` command
/// (CONTRACTS §11) is called with a string that came from the webview and from
/// the user's own keymap overlay, neither of which can be `'static`.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ActionId(pub String);

impl ActionId {
    /// Borrow the id.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for ActionId {
    fn from(s: &str) -> Self {
        Self(s.to_owned())
    }
}

impl std::fmt::Display for ActionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The four keymap presets of spec §33.
#[derive(
    Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default, Serialize, Deserialize,
)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum KeymapPreset {
    /// IDE-conventional. The default.
    #[default]
    Modern,
    /// Stata's own bindings, additive over Modern (06 §12.3).
    StataLike,
    /// VS Code's bindings, additive over Modern (06 §12.4).
    VsCodeLike,
    /// A user overlay on one of the three. The base is stored with the overlay
    /// by `stratum-workspace`; this layer only ever sees the resolved base.
    Custom,
}

/// Modifier keys. `MOD` is the *logical* primary modifier of spec §33 —
/// Command on macOS, Control elsewhere — and is what a preset table stores.
/// `CTRL` and `META` are the literal keys, for the rare binding that must
/// differ per platform.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct Modifiers(u8);

impl Modifiers {
    /// No modifiers.
    pub const NONE: Self = Self(0);
    /// The platform's primary modifier: `⌘` on macOS, `Ctrl` elsewhere.
    pub const MOD: Self = Self(1 << 0);
    /// Literal Control, on every platform.
    pub const CTRL: Self = Self(1 << 1);
    /// Option / Alt.
    pub const ALT: Self = Self(1 << 2);
    /// Shift.
    pub const SHIFT: Self = Self(1 << 3);
    /// Literal Command / Windows / Super.
    pub const META: Self = Self(1 << 4);

    /// True when every bit of `other` is set here.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// True when nothing is set.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Set the bits of `other`.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Clear the bits of `other`.
    #[must_use]
    pub const fn without(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }
}

impl std::ops::BitOr for Modifiers {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        self.union(rhs)
    }
}

/// The non-modifier half of an accelerator.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Key {
    /// A printable character. Letters are stored lowercase; the OS decides how
    /// to draw them.
    Char(char),
    /// Return / Enter.
    Enter,
    /// Escape.
    Escape,
    /// Tab.
    Tab,
    /// Space.
    Space,
    /// Backspace / Delete-left.
    Backspace,
    /// Forward delete.
    Delete,
    /// Arrow up.
    Up,
    /// Arrow down.
    Down,
    /// Arrow left.
    Left,
    /// Arrow right.
    Right,
    /// Home.
    Home,
    /// End.
    End,
    /// Page up.
    PageUp,
    /// Page down.
    PageDown,
    /// A function key, 1-based.
    F(u8),
}

/// A single keystroke bound to a command.
///
/// Chords (`Mod+K Mod+S`) are deliberately not representable: no OS menu bar
/// can display or dispatch one, so [`Keymap::accelerator`] returns `None` for a
/// chorded binding and the chord is handled entirely by the frontend's trie
/// (06 §12.1).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Accelerator {
    /// Which modifiers are held.
    pub mods: Modifiers,
    /// The key itself.
    pub key: Key,
}

impl Accelerator {
    /// Construct.
    #[must_use]
    pub const fn new(mods: Modifiers, key: Key) -> Self {
        Self { mods, key }
    }

    /// Rewrite the logical [`Modifiers::MOD`] into the literal key this
    /// platform uses: `META` (Command) on macOS, `CTRL` everywhere else.
    ///
    /// Idempotent — an already-resolved accelerator has no `MOD` bit left.
    #[must_use]
    pub const fn resolve(self, platform: PlatformId) -> Self {
        if !self.mods.contains(Modifiers::MOD) {
            return self;
        }
        let literal = match platform {
            PlatformId::MacOs => Modifiers::META,
            PlatformId::Windows | PlatformId::Linux => Modifiers::CTRL,
        };
        Self {
            mods: self.mods.without(Modifiers::MOD).union(literal),
            key: self.key,
        }
    }

    /// Render for display, in the platform's own convention.
    ///
    /// macOS uses the Apple modifier order `⌃⌥⇧⌘` with no separators; Windows
    /// and Linux use `Ctrl+Alt+Shift+Key`. An unresolved [`Modifiers::MOD`] is
    /// resolved first, so `display` is total.
    #[must_use]
    pub fn display(self, platform: PlatformId) -> String {
        let a = self.resolve(platform);
        let mut out = String::new();
        match platform {
            PlatformId::MacOs => {
                if a.mods.contains(Modifiers::CTRL) {
                    out.push('\u{2303}'); // ⌃
                }
                if a.mods.contains(Modifiers::ALT) {
                    out.push('\u{2325}'); // ⌥
                }
                if a.mods.contains(Modifiers::SHIFT) {
                    out.push('\u{21e7}'); // ⇧
                }
                if a.mods.contains(Modifiers::META) {
                    out.push('\u{2318}'); // ⌘
                }
                out.push_str(&key_symbol_macos(a.key));
            }
            PlatformId::Windows | PlatformId::Linux => {
                if a.mods.contains(Modifiers::CTRL) {
                    out.push_str("Ctrl+");
                }
                if a.mods.contains(Modifiers::ALT) {
                    out.push_str("Alt+");
                }
                if a.mods.contains(Modifiers::SHIFT) {
                    out.push_str("Shift+");
                }
                if a.mods.contains(Modifiers::META) {
                    out.push_str(if platform == PlatformId::Windows {
                        "Win+"
                    } else {
                        "Super+"
                    });
                }
                out.push_str(&key_name(a.key));
            }
        }
        out
    }

    /// Parse the `"Mod+Shift+Enter"` spelling used by the preset tables and by
    /// `resources/keymaps/*.json` (06 §12.1).
    ///
    /// # Errors
    /// [`crate::PlatformError::Unsupported`] for an unknown modifier or key
    /// name, and for a chord (a spelling containing a space).
    pub fn parse(spec: &str) -> Result<Self> {
        use crate::PlatformError::Unsupported;
        if spec.contains(char::is_whitespace) {
            return Err(Unsupported("a chord cannot be a menu accelerator"));
        }
        let mut mods = Modifiers::NONE;
        let mut key = None;
        // Split on '+' but keep a trailing '+' as the Plus key: "Mod++".
        let parts = split_spec(spec);
        let n = parts.len();
        for (i, part) in parts.into_iter().enumerate() {
            let last = i + 1 == n;
            match part.to_ascii_lowercase().as_str() {
                "mod" if !last => mods = mods | Modifiers::MOD,
                "ctrl" | "control" if !last => mods = mods | Modifiers::CTRL,
                "alt" | "option" | "opt" if !last => mods = mods | Modifiers::ALT,
                "shift" if !last => mods = mods | Modifiers::SHIFT,
                "meta" | "cmd" | "command" | "super" | "win" if !last => {
                    mods = mods | Modifiers::META;
                }
                _ if last => key = Some(parse_key(&part)?),
                _ => return Err(Unsupported("unknown modifier in an accelerator")),
            }
        }
        let key = key.ok_or(Unsupported("accelerator has no key"))?;
        Ok(Self { mods, key })
    }
}

fn split_spec(spec: &str) -> Vec<String> {
    if spec == "+" {
        return vec!["+".to_owned()];
    }
    let mut parts: Vec<String> = spec.split('+').map(str::to_owned).collect();
    // "Mod++" splits to ["Mod", "", ""]: the key is the literal plus.
    if parts.last().is_some_and(String::is_empty) {
        parts.pop();
        if let Some(last) = parts.last_mut() {
            if last.is_empty() {
                *last = "+".to_owned();
            }
        }
    }
    parts
}

fn parse_key(name: &str) -> Result<Key> {
    use crate::PlatformError::Unsupported;
    let lower = name.to_ascii_lowercase();
    if let Some(n) = lower.strip_prefix('f') {
        if let Ok(n) = n.parse::<u8>() {
            if (1..=24).contains(&n) {
                return Ok(Key::F(n));
            }
        }
    }
    Ok(match lower.as_str() {
        "enter" | "return" => Key::Enter,
        "escape" | "esc" => Key::Escape,
        "tab" => Key::Tab,
        "space" => Key::Space,
        "backspace" => Key::Backspace,
        "delete" | "del" => Key::Delete,
        "up" => Key::Up,
        "down" => Key::Down,
        "left" => Key::Left,
        "right" => Key::Right,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" | "pgup" => Key::PageUp,
        "pagedown" | "pgdn" => Key::PageDown,
        _ => {
            let mut chars = lower.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => Key::Char(c),
                _ => return Err(Unsupported("unknown key name in an accelerator")),
            }
        }
    })
}

fn key_symbol_macos(key: Key) -> String {
    match key {
        Key::Char(c) => c.to_uppercase().to_string(),
        Key::Enter => "\u{21a9}".to_owned(),     // ↩
        Key::Escape => "\u{238b}".to_owned(),    // ⎋
        Key::Tab => "\u{21e5}".to_owned(),       // ⇥
        Key::Space => "\u{2423}".to_owned(),     // ␣
        Key::Backspace => "\u{232b}".to_owned(), // ⌫
        Key::Delete => "\u{2326}".to_owned(),    // ⌦
        Key::Up => "\u{2191}".to_owned(),
        Key::Down => "\u{2193}".to_owned(),
        Key::Left => "\u{2190}".to_owned(),
        Key::Right => "\u{2192}".to_owned(),
        Key::Home => "\u{2196}".to_owned(), // ↖
        Key::End => "\u{2198}".to_owned(),  // ↘
        Key::PageUp => "\u{21de}".to_owned(),
        Key::PageDown => "\u{21df}".to_owned(),
        Key::F(n) => format!("F{n}"),
    }
}

fn key_name(key: Key) -> String {
    match key {
        Key::Char(c) => c.to_uppercase().to_string(),
        Key::Enter => "Enter".to_owned(),
        Key::Escape => "Esc".to_owned(),
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
    }
}

/// Resolves a command id and a preset to a keystroke.
///
/// The implementation the desktop installs is backed by
/// `stratum-workspace`'s persisted preset + user overlay (ARCHITECTURE §5), so
/// the menu bar and the editor's key handler can never disagree about what a
/// command is bound to. [`StaticKeymap`] is the in-crate implementation and
/// carries the tables of 06 §12.2–12.4 for the commands that appear in a menu.
pub trait Keymap: Send + Sync {
    /// The binding, unresolved (it may still carry [`Modifiers::MOD`]).
    /// `None` for an unbound command and for a chord.
    fn accelerator(&self, action: &ActionId, preset: KeymapPreset) -> Option<Accelerator>;
}

/// A [`Keymap`] over a fixed table.
#[derive(Clone, Debug, Default)]
pub struct StaticKeymap {
    by_preset: BTreeMap<KeymapPreset, BTreeMap<ActionId, Accelerator>>,
}

impl StaticKeymap {
    /// Empty.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a binding, replacing any existing one for the same pair.
    ///
    /// # Panics
    /// Never at runtime for a valid `spec`; the `expect` is the table's own
    /// self-check and only fires on a typo in a `const` table in this file,
    /// which is a compile-time-shaped bug caught by this crate's own tests.
    #[must_use]
    pub fn with(mut self, preset: KeymapPreset, action: &str, spec: &str) -> Self {
        let accel = Accelerator::parse(spec).expect("built-in keymap table has a malformed spec");
        self.by_preset
            .entry(preset)
            .or_default()
            .insert(ActionId::from(action), accel);
        self
    }

    /// The built-in menu-bar bindings of 06 §12.2 (Modern) with the §12.3 and
    /// §12.4 deltas layered on top of it.
    ///
    /// This covers only the commands that appear in a **menu**. The full preset
    /// — pane toggles, command-bar keys, chords — lives in
    /// `apps/desktop/resources/keymaps/*.json` and is the frontend's; a menu
    /// item and its editor binding are kept in step by the desktop installing a
    /// [`Keymap`] backed by that file at startup, at which point this table is
    /// the fallback for a build with no resources.
    #[must_use]
    pub fn builtin() -> Self {
        let mut k = Self::new();
        for (action, spec) in MODERN {
            // Every preset inherits Modern; the deltas below overwrite.
            for preset in [
                KeymapPreset::Modern,
                KeymapPreset::StataLike,
                KeymapPreset::VsCodeLike,
                KeymapPreset::Custom,
            ] {
                k = k.with(preset, action, spec);
            }
        }
        for (action, spec) in STATA_LIKE {
            k = k.with(KeymapPreset::StataLike, action, spec);
        }
        for (action, spec) in VS_CODE_LIKE {
            k = k.with(KeymapPreset::VsCodeLike, action, spec);
        }
        k
    }
}

impl Keymap for StaticKeymap {
    fn accelerator(&self, action: &ActionId, preset: KeymapPreset) -> Option<Accelerator> {
        self.by_preset.get(&preset)?.get(action).copied()
    }
}

/// 06 §12.2, the Modern preset, restricted to menu-bar commands.
const MODERN: &[(&str, &str)] = &[
    ("run.block", "Mod+Enter"),
    ("run.blockAndAdvance", "Shift+Enter"),
    ("run.selection", "Alt+Enter"),
    ("run.fromHere", "Mod+Alt+Enter"),
    ("run.fileClean", "Mod+Shift+Enter"),
    ("run.above", "Mod+Alt+Up"),
    ("run.below", "Mod+Alt+Down"),
    ("run.allStale", "Mod+Shift+R"),
    ("run.break", "Mod+."),
    ("focus.commandBar", "Mod+L"),
    ("nav.quickOpen", "Mod+P"),
    ("nav.commandPalette", "Mod+Shift+P"),
    ("view.toggleAssistant", "Mod+J"),
    ("view.cycleInlineMode", "Mod+Alt+I"),
    ("view.toggleDocument", "Mod+Shift+V"),
    ("view.modelComparison", "Mod+Shift+M"),
    ("data.editorBrowse", "Mod+Shift+D"),
    ("layout.modern", "Mod+Alt+1"),
    ("layout.classic", "Mod+Alt+2"),
    ("layout.focus", "Mod+Alt+3"),
    ("edit.toggleComment", "Mod+/"),
    ("help.contextual", "F1"),
    ("results.clear", "Mod+Shift+K"),
    ("results.collapseAll", "Mod+Alt+C"),
    ("results.clearBlock", "Mod+Shift+Backspace"),
];

/// 06 §12.3. `Mod+Enter` is explicitly additive and is therefore absent here.
const STATA_LIKE: &[(&str, &str)] = &[
    ("run.doFile", "Mod+D"),
    ("run.doFileQuietly", "Mod+Shift+D"),
    ("data.editorEdit", "Mod+8"),
    ("view.doFileEditor", "Mod+9"),
    ("view.variables", "Mod+4"),
    ("view.graphWindow", "Mod+G"),
];

/// 06 §12.4. The two chords in that list (`Mod+K Mod+0`) are unrepresentable in
/// a menu and are the frontend's alone.
const VS_CODE_LIKE: &[(&str, &str)] = &[
    ("run.file", "F5"),
    ("run.fileClean", "Mod+F5"),
    ("run.block", "Shift+Alt+Enter"),
    ("view.toggleSideBar", "Mod+B"),
    ("view.toggleBottomPanel", "Mod+`"),
    ("view.explorer", "Mod+Shift+E"),
    ("nav.searchInProject", "Mod+Shift+F"),
    ("nav.goToSymbol", "Mod+Shift+O"),
];

/// Where the menu bar lives.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MenuPlacement {
    /// One menu bar for the application, owned by the OS. macOS.
    GlobalMenuBar,
    /// A menu bar per window. Windows, Linux.
    PerWindow,
}

/// Which window a menu is being installed for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MenuTarget {
    /// The application menu bar. Only meaningful with
    /// [`MenuPlacement::GlobalMenuBar`].
    Application,
    /// One window's menu bar.
    Window(crate::WindowHandle),
}

/// An installed menu. Opaque; only [`MenuHost::update`] consumes it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct MenuHandle(pub u64);

/// A standard role the OS renders itself (localised, with the right position
/// and the right default binding).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MenuRole {
    /// About Stratum.
    About,
    /// Settings… / Preferences.
    Settings,
    /// macOS Services submenu.
    Services,
    /// Hide Stratum.
    Hide,
    /// Hide Others.
    HideOthers,
    /// Show All.
    ShowAll,
    /// Quit / Exit.
    Quit,
    /// Close window.
    Close,
    /// Minimise.
    Minimize,
    /// Zoom.
    Zoom,
    /// The Window menu.
    Window,
    /// The Help menu.
    Help,
    /// Standard editing verbs, which the OS wires to the focused text field.
    Undo,
    /// See [`MenuRole::Undo`].
    Redo,
    /// See [`MenuRole::Undo`].
    Cut,
    /// See [`MenuRole::Undo`].
    Copy,
    /// See [`MenuRole::Undo`].
    Paste,
    /// See [`MenuRole::Undo`].
    SelectAll,
}

/// One node of the menu tree.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum MenuItem {
    /// A command.
    Action {
        /// The command this dispatches. The frontend routes it through the same
        /// dispatcher the keymap and the command palette use.
        id: ActionId,
        /// The label, already localised by the caller.
        label: String,
        /// `None` when the command has no accelerator in the active preset.
        accel: Option<Accelerator>,
        /// Greyed out when false.
        enabled: bool,
        /// `Some` makes it a checkbox item.
        checked: Option<bool>,
        /// A standard role, when this is one.
        role: Option<MenuRole>,
    },
    /// A nested menu.
    Submenu {
        /// The title.
        label: String,
        /// Children.
        items: Vec<MenuItem>,
        /// A standard role, when this is one (Window, Help, Services).
        role: Option<MenuRole>,
    },
    /// A separator line.
    Separator,
}

/// The whole menu bar as data.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct MenuModel {
    /// Top-level menus, in order.
    pub items: Vec<MenuItem>,
}

/// One change in a [`MenuPatch`].
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum MenuChange {
    /// Enable or disable an item.
    Enabled {
        /// Target command.
        action: ActionId,
        /// New state.
        value: bool,
    },
    /// Set or clear a checkmark.
    Checked {
        /// Target command.
        action: ActionId,
        /// `None` turns the item back into a plain command.
        value: Option<bool>,
    },
    /// Relabel — "Run Block" becomes "Run Selection" when there is one.
    Label {
        /// Target command.
        action: ActionId,
        /// New label.
        value: String,
    },
    /// Rebind, after the user changes preset.
    Accelerator {
        /// Target command.
        action: ActionId,
        /// New binding, already resolved.
        value: Option<Accelerator>,
    },
}

/// An incremental menu update. Rebuilding the whole bar on every selection
/// change makes the macOS menu bar flicker, so enable/disable travels as a
/// patch.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct MenuPatch {
    /// Applied in order.
    pub changes: Vec<MenuChange>,
}

/// Where the OS expects Settings, Quit and friends to be.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SettingsLocation {
    /// macOS: the application menu.
    AppMenu,
    /// Windows: Edit → Preferences.
    EditMenu,
    /// Linux: File → Preferences.
    FilePreferences,
}

/// The platform's own menu conventions.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SystemMenuItems {
    /// macOS injects an application menu named after the app.
    pub app_menu: bool,
    /// Where Settings goes.
    pub settings_location: SettingsLocation,
    /// "Settings…" on macOS 13+ and Windows; "Preferences" on Linux.
    pub settings_label: &'static str,
    /// "Quit Stratum" vs "Exit".
    pub quit_label: &'static str,
    /// macOS Services submenu.
    pub services: bool,
    /// macOS Hide / Hide Others / Show All.
    pub hide: bool,
    /// A Window menu listing open windows.
    pub window_menu: bool,
}

impl SystemMenuItems {
    /// The conventions for a platform. Pure, so all three are asserted from one
    /// test run.
    #[must_use]
    pub const fn for_platform(id: PlatformId) -> Self {
        match id {
            PlatformId::MacOs => Self {
                app_menu: true,
                settings_location: SettingsLocation::AppMenu,
                settings_label: "Settings…",
                quit_label: "Quit Stratum",
                services: true,
                hide: true,
                window_menu: true,
            },
            PlatformId::Windows => Self {
                app_menu: false,
                settings_location: SettingsLocation::EditMenu,
                settings_label: "Settings…",
                quit_label: "Exit",
                services: false,
                hide: false,
                window_menu: false,
            },
            PlatformId::Linux => Self {
                app_menu: false,
                settings_location: SettingsLocation::FilePreferences,
                settings_label: "Preferences",
                quit_label: "Quit",
                services: false,
                hide: false,
                window_menu: false,
            },
        }
    }
}

/// The toolkit half of [`MenuHost`]: turning a [`MenuModel`] into a real
/// `NSMenu` / `HMENU` / `GtkMenuBar`.
///
/// Separate from [`MenuHost`] because the application shell — not this layer —
/// owns the window toolkit. `stratum-desktop` runs on Tauri, which already owns
/// `NSApp.mainMenu`; a second authority setting the same menu bar is a race
/// whose winner depends on startup order. So the platform impl owns the parts
/// that are genuinely OS *policy* — placement, system items, and the
/// accelerator resolution the whole §33 keymap rests on — and delegates the
/// toolkit call to a sink the shell installs.
///
/// A [`MenuHost`] with no sink returns
/// [`crate::PlatformError::BackendUnavailable`] from `install`/`update`, which
/// is the correct description of a headless process: there is no menu bar to
/// install into.
pub trait MenuSink: Send + Sync {
    /// Build and attach the native menu.
    ///
    /// # Errors
    /// Whatever the toolkit reports.
    fn install(&self, model: &MenuModel, target: MenuTarget) -> Result<MenuHandle>;

    /// Apply an incremental change to an attached menu.
    ///
    /// # Errors
    /// Whatever the toolkit reports.
    fn update(&self, handle: MenuHandle, patch: &MenuPatch) -> Result<()>;
}

/// Menu installation, placement and accelerator resolution.
pub trait MenuHost: Send + Sync {
    /// Install a menu bar.
    ///
    /// # Errors
    /// [`crate::PlatformError::Unsupported`] when `target` does not match
    /// [`MenuHost::placement`], and [`crate::PlatformError::BackendUnavailable`]
    /// when no window system is attached.
    fn install(&self, model: &MenuModel, target: MenuTarget) -> Result<MenuHandle>;

    /// Apply an incremental change.
    ///
    /// # Errors
    /// [`crate::PlatformError::BackendUnavailable`] for a handle that is no
    /// longer installed.
    fn update(&self, handle: MenuHandle, patch: &MenuPatch) -> Result<()>;

    /// Global menu bar or per-window.
    fn placement(&self) -> MenuPlacement;

    /// Resolve a command to a keystroke **for this platform**: the returned
    /// accelerator's [`Modifiers::MOD`] has already become `⌘` on macOS and
    /// `Ctrl` elsewhere. `None` for an unbound command or a chord.
    fn accelerator(&self, action: &ActionId, preset: KeymapPreset) -> Option<Accelerator>;

    /// The platform's Settings/Quit/Services conventions.
    fn system_items(&self) -> SystemMenuItems;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mod_resolves_to_command_on_macos_and_control_elsewhere() {
        let a = Accelerator::parse("Mod+Enter").unwrap();
        assert_eq!(a.display(PlatformId::MacOs), "\u{2318}\u{21a9}");
        assert_eq!(a.display(PlatformId::Windows), "Ctrl+Enter");
        assert_eq!(a.display(PlatformId::Linux), "Ctrl+Enter");
    }

    #[test]
    fn resolve_is_idempotent() {
        let a = Accelerator::parse("Mod+Shift+R").unwrap();
        let once = a.resolve(PlatformId::MacOs);
        assert_eq!(once, once.resolve(PlatformId::MacOs));
        assert!(!once.mods.contains(Modifiers::MOD));
        assert!(once.mods.contains(Modifiers::META));
    }

    /// A binding that names Ctrl explicitly stays Ctrl on macOS — that is the
    /// whole point of having both bits (06 §12.1).
    #[test]
    fn literal_ctrl_survives_on_macos() {
        let a = Accelerator::parse("Ctrl+Alt+Shift+K").unwrap();
        assert_eq!(a.display(PlatformId::MacOs), "\u{2303}\u{2325}\u{21e7}K");
        assert_eq!(a.display(PlatformId::Windows), "Ctrl+Alt+Shift+K");
    }

    #[test]
    fn apple_modifier_order_is_control_option_shift_command() {
        let a = Accelerator::parse("Mod+Ctrl+Alt+Shift+Space").unwrap();
        assert_eq!(
            a.display(PlatformId::MacOs),
            "\u{2303}\u{2325}\u{21e7}\u{2318}\u{2423}"
        );
    }

    #[test]
    fn punctuation_and_function_keys_round_trip() {
        assert_eq!(
            Accelerator::parse("Mod+.")
                .unwrap()
                .display(PlatformId::MacOs),
            "\u{2318}."
        );
        assert_eq!(
            Accelerator::parse("Mod+/")
                .unwrap()
                .display(PlatformId::Windows),
            "Ctrl+/"
        );
        assert_eq!(
            Accelerator::parse("F1").unwrap().display(PlatformId::Linux),
            "F1"
        );
        assert_eq!(
            Accelerator::parse("Mod+`")
                .unwrap()
                .display(PlatformId::MacOs),
            "\u{2318}`"
        );
        assert_eq!(Accelerator::parse("Mod++").unwrap().key, Key::Char('+'));
    }

    #[test]
    fn a_chord_is_not_a_menu_accelerator() {
        let err = Accelerator::parse("Mod+K Mod+S").unwrap_err();
        assert!(err.is_unsupported());
    }

    #[test]
    fn builtin_table_parses_and_layers_the_deltas() {
        let k = StaticKeymap::builtin();
        let block = ActionId::from("run.block");
        assert_eq!(
            k.accelerator(&block, KeymapPreset::Modern)
                .unwrap()
                .display(PlatformId::MacOs),
            "\u{2318}\u{21a9}"
        );
        // 12.4 overrides run.block; 12.3 leaves it alone (it is "additive").
        // Rendered in the canonical Ctrl-Alt-Shift order, not the source
        // spelling: `display` is the single authority on how a binding looks.
        assert_eq!(
            k.accelerator(&block, KeymapPreset::VsCodeLike)
                .unwrap()
                .display(PlatformId::Windows),
            "Alt+Shift+Enter"
        );
        assert_eq!(
            k.accelerator(&block, KeymapPreset::StataLike)
                .unwrap()
                .display(PlatformId::Windows),
            "Ctrl+Enter"
        );
        assert!(k
            .accelerator(&ActionId::from("run.doFile"), KeymapPreset::Modern)
            .is_none());
    }

    #[test]
    fn system_items_follow_the_platform() {
        let mac = SystemMenuItems::for_platform(PlatformId::MacOs);
        assert!(mac.app_menu && mac.services && mac.hide);
        assert_eq!(mac.settings_location, SettingsLocation::AppMenu);
        assert_eq!(
            SystemMenuItems::for_platform(PlatformId::Windows).quit_label,
            "Exit"
        );
        assert_eq!(
            SystemMenuItems::for_platform(PlatformId::Linux).settings_location,
            SettingsLocation::FilePreferences
        );
    }
}
