//! Layout persistence — spec §25's three presets, design 06 §8, CONTRACTS §12.
//!
//! `LayoutSpec` is our schema; it *wraps* dockview's own layout blob rather than
//! reimplementing it, so the `dock` field stays opaque
//! ([`serde_json::Value`]) and dockview can change its serialization without
//! changing ours.
//!
//! # Presets are read-only; the user's changes are an overlay
//!
//! Design 06 §8.1: presets ship as JSON in `resources/layouts/`, user edits are
//! written to `<config>/layouts/user/<id>.json`, **a preset is never mutated**,
//! and so `layout_reset` is a file delete rather than a re-derivation. That is
//! the property `layout_reset` deletes only the user overlay: after a reset, the
//! preset is byte-identical to what shipped, because nothing ever wrote to it.
//!
//! # The presets exist in Rust as well as on disk
//!
//! [`LayoutSpec::preset`] returns the four shipped layouts as data. This is not
//! a duplicate of the resource files — it is the floor beneath them.
//! `resources/layouts/*.json` belongs to the desktop app (W07/W13); this crate
//! must open a project when those files are missing, unreadable, or from a newer
//! version of the app, and "fall back to the preset" is meaningless if the
//! preset was also the thing that failed to load. Design 06 §8.5 requires
//! exactly this behaviour for a malformed *user* layout; the same argument
//! applies one level down.

use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};
use stratum_proto::InlineResultsMode;

use crate::write::write_bytes_atomic;

/// `LayoutSpec.schema`. CONTRACTS §12 pins it at 3, versioned independently of
/// the durable sidecar's schema (CONTRACTS §15).
pub const SCHEMA: u32 = 3;

/// The four layouts that ship with the product.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Preset {
    /// §25A — editor-centered hybrid notebook. The default.
    Modern,
    /// §25B / §7 — Stata 18 Widescreen: History, Results, Command, Variables,
    /// Properties. Inline results OFF.
    Classic,
    /// Stata 18 Sidebar: History joins the right stack.
    ClassicSidebar,
    /// §25C / §9 — code plus inline output, chrome auto-hidden.
    Focus,
}

impl Preset {
    /// The `id` this preset is stored under.
    pub const fn id(self) -> &'static str {
        match self {
            Preset::Modern => "modern",
            Preset::Classic => "classic",
            Preset::ClassicSidebar => "classic-sidebar",
            Preset::Focus => "focus",
        }
    }

    /// Parse an id back into a preset. `None` for a `user:` id.
    pub fn from_id(id: &str) -> Option<Self> {
        Some(match id {
            "modern" => Preset::Modern,
            "classic" => Preset::Classic,
            "classic-sidebar" => Preset::ClassicSidebar,
            "focus" => Preset::Focus,
            _ => return None,
        })
    }

    /// Every preset, in menu order.
    pub const ALL: [Preset; 4] = [
        Preset::Modern,
        Preset::Classic,
        Preset::ClassicSidebar,
        Preset::Focus,
    ];
}

/// Top-bar and status-bar chrome.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Chrome {
    /// How the top bar behaves.
    pub top_bar: TopBar,
    /// Whether the status bar is shown.
    pub status_bar: bool,
}

/// Top-bar mode.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TopBar {
    /// Modern's 38 px bar.
    Full,
    /// Classic's compact button strip.
    Compact,
    /// Focus: reveals on pointer near the top edge.
    AutoHide,
}

/// Where the command bar lives.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CommandBar {
    /// Pinned to the bottom of the editor pane.
    DockedBottom,
    /// Summoned with `Mod+L`, dismissed with Esc.
    Overlay,
    /// Its own dock pane (Classic).
    Pane,
}

/// Colour scheme.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    /// Force light.
    Light,
    /// Force dark.
    Dark,
    /// Follow the OS.
    System,
}

/// Defaults a layout imposes when it is applied.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Defaults {
    /// Inline results mode. A per-file override lives in the durable sidecar.
    pub inline_results: InlineResultsMode,
    /// Document View on by default (spec §24).
    pub doc_view: bool,
    /// Where the command bar sits.
    pub command_bar: CommandBar,
    /// `None` means "do not touch the user's theme choice".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub theme: Option<Theme>,
}

/// A native window in a layout. `windows[0]` is always the main window.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowSpec {
    /// What the window is for.
    pub role: WindowRole,
    /// Tauri window label, `${project}:${role}[:${instance}]`.
    pub label: String,
    /// Saved geometry, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bounds: Option<Bounds>,
    /// dockview's `SerializedDockview`. **Opaque to us on purpose** — see the
    /// module header.
    pub dock: serde_json::Value,
}

/// Window geometry.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Bounds {
    /// Left edge.
    pub x: f64,
    /// Top edge.
    pub y: f64,
    /// Width.
    pub w: f64,
    /// Height.
    pub h: f64,
    /// Monitor name, so a two-monitor layout restores to the right screen.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub monitor: Option<String>,
}

/// CONTRACTS §12's `WindowRole`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WindowRole {
    /// The project window.
    Main,
    /// A detached do-file editor.
    Editor,
    /// A detached Data Editor.
    Data,
    /// A detached graph window.
    Graph,
    /// A detached ordinary pane.
    Pane,
    /// The Viewer.
    Viewer,
    /// Settings / keymap editor.
    Prefs,
}

/// The serialized layout. Transcribed from CONTRACTS §12.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LayoutSpec {
    /// Always [`SCHEMA`].
    pub schema: u32,
    /// `"modern" | "classic" | "classic-sidebar" | "focus" | "user:<id>"`.
    pub id: String,
    /// User-visible name.
    pub name: String,
    /// The preset a user layout was derived from. Design 06 §8.5: a malformed
    /// layout falls back to this.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub based_on: Option<String>,
    /// Chrome.
    pub chrome: Chrome,
    /// Defaults.
    pub defaults: Defaults,
    /// Windows; `[0]` is the main window.
    pub windows: Vec<WindowSpec>,
    /// Per-pane options (columns, sort, filters), keyed by `PaneId`. Opaque
    /// here — each pane's owner defines its own shape.
    pub panes: serde_json::Map<String, serde_json::Value>,
}

impl LayoutSpec {
    /// The shipped definition of one preset.
    pub fn preset(p: Preset) -> Self {
        let (name, chrome, defaults) = match p {
            Preset::Modern => (
                "Modern",
                Chrome {
                    top_bar: TopBar::Full,
                    status_bar: true,
                },
                Defaults {
                    inline_results: InlineResultsMode::EditorRun,
                    doc_view: false,
                    command_bar: CommandBar::DockedBottom,
                    theme: Some(Theme::System),
                },
            ),
            // §25B and design 06 §8.3 both state it explicitly: inline results
            // are OFF in Classic. A traditional Stata user who sees cards appear
            // under their commands has not been given Classic.
            Preset::Classic | Preset::ClassicSidebar => (
                if p == Preset::Classic {
                    "Classic Stata"
                } else {
                    "Classic Stata (Sidebar)"
                },
                Chrome {
                    top_bar: TopBar::Compact,
                    status_bar: true,
                },
                Defaults {
                    inline_results: InlineResultsMode::Off,
                    doc_view: false,
                    command_bar: CommandBar::Pane,
                    theme: Some(Theme::System),
                },
            ),
            Preset::Focus => (
                "Focus",
                Chrome {
                    top_bar: TopBar::AutoHide,
                    status_bar: false,
                },
                Defaults {
                    inline_results: InlineResultsMode::Always,
                    doc_view: false,
                    command_bar: CommandBar::Overlay,
                    theme: Some(Theme::System),
                },
            ),
        };
        LayoutSpec {
            schema: SCHEMA,
            id: p.id().to_owned(),
            name: name.to_owned(),
            based_on: None,
            chrome,
            defaults,
            windows: vec![WindowSpec {
                role: WindowRole::Main,
                label: format!("main:{}", p.id()),
                bounds: None,
                // Empty rather than invented: the dock blob is dockview's, and
                // the frontend builds the preset's arrangement on first run.
                dock: serde_json::Value::Null,
            }],
            panes: serde_json::Map::new(),
        }
    }

    /// True if this is a user layout rather than a shipped preset.
    pub fn is_user(&self) -> bool {
        self.id.starts_with("user:")
    }

    /// Canonical bytes: two-space indent, LF, trailing newline. A layout file is
    /// documented as human-editable (design 06 §8.5), so it is written the way a
    /// human would want to read it.
    pub fn to_canonical_bytes(&self) -> Vec<u8> {
        let mut s = serde_json::to_string_pretty(self).expect("LayoutSpec is always encodable");
        s.push('\n');
        s.into_bytes()
    }
}

/// Reading and writing layouts.
///
/// Two directories: the read-only shipped presets and the user's overlay.
/// Nothing ever writes into `presets`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LayoutStore {
    /// `resources/layouts/` in the installed app. May not exist.
    pub presets: Utf8PathBuf,
    /// `<config>/layouts/user/`. Created on first save.
    pub user: Utf8PathBuf,
}

/// What went wrong.
#[derive(Debug, thiserror::Error)]
pub enum LayoutError {
    /// No preset and no overlay by that id.
    #[error("no layout {id}")]
    NotFound {
        /// The id that was asked for.
        id: String,
    },
    /// The filesystem said no.
    #[error("{path}: {source}")]
    Io {
        /// Path involved.
        path: Utf8PathBuf,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },
}

impl LayoutStore {
    /// A store rooted at two directories.
    pub fn new(presets: impl Into<Utf8PathBuf>, user: impl Into<Utf8PathBuf>) -> Self {
        LayoutStore {
            presets: presets.into(),
            user: user.into(),
        }
    }

    fn user_path(&self, id: &str) -> Utf8PathBuf {
        // `user:abc` → `user_abc.json`. A raw `:` is illegal in a Windows
        // filename, and a layout that cannot be saved on Windows is a layout
        // that does not exist (spec §27).
        self.user.join(format!("{}.json", id.replace(':', "_")))
    }

    /// `layout_load { id }`.
    ///
    /// Resolution order: the user overlay, then the shipped preset file, then
    /// the built-in preset. A malformed overlay is **skipped with the file left
    /// in place** and the layer below is used — design 06 §8.5's "falls back to
    /// its `basedOn` preset with a status-bar notice". Deleting a user's layout
    /// because we could not parse it would be the wrong repair.
    pub fn load(&self, id: &str) -> Result<LayoutSpec, LayoutError> {
        if let Some(spec) = read_spec(&self.user_path(id)) {
            return Ok(spec);
        }
        if let Some(spec) = read_spec(&self.presets.join(format!("{id}.json"))) {
            return Ok(spec);
        }
        // A malformed user layout names its parent; fall back to that.
        if let Some(based) = read_based_on(&self.user_path(id)) {
            if let Some(p) = Preset::from_id(&based) {
                return Ok(LayoutSpec::preset(p));
            }
        }
        Preset::from_id(id)
            .map(LayoutSpec::preset)
            .ok_or_else(|| LayoutError::NotFound { id: id.to_owned() })
    }

    /// `layout_save { spec }`. Always writes the **user overlay**, never a
    /// preset file — that is what makes [`LayoutStore::reset`] a delete.
    pub fn save(&self, spec: &LayoutSpec) -> Result<Utf8PathBuf, LayoutError> {
        let path = self.user_path(&spec.id);
        std::fs::create_dir_all(&self.user).map_err(|source| LayoutError::Io {
            path: self.user.clone(),
            source,
        })?;
        write_bytes_atomic(&path, &spec.to_canonical_bytes()).map_err(|source| {
            LayoutError::Io {
                path: path.clone(),
                source,
            }
        })?;
        Ok(path)
    }

    /// `layout_reset { id }` — **deletes only the user overlay**.
    ///
    /// Returns whether a file was removed. Resetting a layout that was never
    /// customised is a no-op, not an error.
    pub fn reset(&self, id: &str) -> Result<bool, LayoutError> {
        let path = self.user_path(id);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(source) => Err(LayoutError::Io { path, source }),
        }
    }

    /// Every layout the command palette should list: the four presets plus every
    /// user overlay, presets first.
    pub fn list(&self) -> Vec<String> {
        let mut out: Vec<String> = Preset::ALL.iter().map(|p| p.id().to_owned()).collect();
        if let Ok(dir) = std::fs::read_dir(&self.user) {
            let mut users: Vec<String> = dir
                .flatten()
                .filter_map(|e| {
                    let p = Utf8PathBuf::from_path_buf(e.path()).ok()?;
                    if p.extension() != Some("json") {
                        return None;
                    }
                    read_spec(&p).map(|s| s.id)
                })
                .filter(|id| Preset::from_id(id).is_none())
                .collect();
            users.sort();
            out.extend(users);
        }
        out
    }
}

fn read_spec(path: &Utf8Path) -> Option<LayoutSpec> {
    let raw = std::fs::read(path).ok()?;
    serde_json::from_slice(&raw).ok()
}

/// Pull `basedOn` out of a file we could not fully parse — the one field a
/// malformed layout still has to give us.
fn read_based_on(path: &Utf8Path) -> Option<String> {
    let raw = std::fs::read(path).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&raw).ok()?;
    v.get("basedOn")?.as_str().map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, LayoutStore) {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let s = LayoutStore::new(root.join("resources"), root.join("user"));
        (tmp, s)
    }

    #[test]
    fn classic_turns_inline_results_off() {
        assert_eq!(
            LayoutSpec::preset(Preset::Classic).defaults.inline_results,
            InlineResultsMode::Off
        );
        assert_eq!(
            LayoutSpec::preset(Preset::Focus).defaults.inline_results,
            InlineResultsMode::Always
        );
    }

    #[test]
    fn every_preset_loads_without_any_resource_files() {
        let (_t, s) = store();
        for p in Preset::ALL {
            let spec = s.load(p.id()).unwrap();
            assert_eq!(spec.id, p.id());
            assert_eq!(spec.schema, SCHEMA);
            assert_eq!(spec.windows[0].role, WindowRole::Main);
        }
    }

    #[test]
    fn an_unknown_id_is_not_found() {
        let (_t, s) = store();
        assert!(matches!(
            s.load("nope").unwrap_err(),
            LayoutError::NotFound { .. }
        ));
    }

    #[test]
    fn save_then_load_then_reset_returns_the_preset() {
        let (_t, s) = store();
        let mut spec = LayoutSpec::preset(Preset::Modern);
        spec.chrome.status_bar = false;
        spec.name = "My Modern".into();
        let file = s.save(&spec).unwrap();
        assert!(file.exists());

        assert_eq!(s.load("modern").unwrap().name, "My Modern");
        assert!(s.reset("modern").unwrap());
        assert!(!file.exists());
        assert_eq!(s.load("modern").unwrap().name, "Modern");
    }

    #[test]
    fn reset_deletes_only_the_user_overlay() {
        let (_t, s) = store();
        std::fs::create_dir_all(&s.presets).unwrap();
        let shipped = s.presets.join("modern.json");
        let mut on_disk = LayoutSpec::preset(Preset::Modern);
        on_disk.name = "Shipped Modern".into();
        std::fs::write(&shipped, on_disk.to_canonical_bytes()).unwrap();
        let before = std::fs::read(&shipped).unwrap();

        s.save(&LayoutSpec {
            name: "Mine".into(),
            ..LayoutSpec::preset(Preset::Modern)
        })
        .unwrap();
        assert_eq!(s.load("modern").unwrap().name, "Mine");

        assert!(s.reset("modern").unwrap());
        // The shipped file is byte-identical and the preset is back.
        assert_eq!(std::fs::read(&shipped).unwrap(), before);
        assert_eq!(s.load("modern").unwrap().name, "Shipped Modern");
    }

    #[test]
    fn resetting_an_uncustomised_layout_is_a_no_op() {
        let (_t, s) = store();
        assert!(!s.reset("focus").unwrap());
    }

    #[test]
    fn a_malformed_user_layout_falls_back_to_based_on_and_is_kept() {
        let (_t, s) = store();
        std::fs::create_dir_all(&s.user).unwrap();
        let path = s.user.join("user_x.json");
        std::fs::write(
            &path,
            br#"{"schema":3,"basedOn":"focus","windows":"broken"}"#,
        )
        .unwrap();
        let spec = s.load("user:x").unwrap();
        assert_eq!(spec.id, "focus");
        // The user's file is still there for them to fix by hand.
        assert!(path.exists());
    }

    #[test]
    fn a_user_id_becomes_a_windows_legal_filename() {
        let (_t, s) = store();
        let spec = LayoutSpec {
            id: "user:9f2".into(),
            name: "Wide".into(),
            based_on: Some("modern".into()),
            ..LayoutSpec::preset(Preset::Modern)
        };
        let path = s.save(&spec).unwrap();
        assert!(!path.as_str().contains(':') || path.as_str().starts_with("C:"));
        assert_eq!(s.load("user:9f2").unwrap().name, "Wide");
        assert!(s.load("user:9f2").unwrap().is_user());
    }

    #[test]
    fn list_puts_presets_first() {
        let (_t, s) = store();
        s.save(&LayoutSpec {
            id: "user:z".into(),
            ..LayoutSpec::preset(Preset::Focus)
        })
        .unwrap();
        assert_eq!(
            s.list(),
            vec!["modern", "classic", "classic-sidebar", "focus", "user:z"]
        );
    }
}
