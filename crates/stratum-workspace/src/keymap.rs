//! Keymap persistence — spec §33, design 06 §12.
//!
//! Three shipped presets (Modern, Stata-like, VS Code-like) plus a user overlay
//! at `<config>/keymaps/user.json`. Resolution is design 06 §12.1's: bindings are
//! collected preset-first, `source: "user"` sorts after presets, and for one
//! keystroke **the last one wins**. Conflicts are not errors — the keymap editor
//! shows `shadowed by <command>` — because a keymap that refuses to load because
//! two bindings collide is a keymap the user cannot repair.
//!
//! # `Mod` is resolved here, not in the frontend
//!
//! Spec §33 says respect platform conventions; CONTRACTS §11 makes it concrete
//! with `menu_accelerator { action, preset } -> string | null` and the note that
//! **the frontend never hardcodes ⌘/Ctrl**. [`accelerator`] is that function.
//! Keeping the resolution in Rust means the menu bar, the command palette and
//! the keymap editor cannot drift apart, and that a screenshot taken on Windows
//! never shows a ⌘.

use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};

use crate::write::write_bytes_atomic;

/// The shipped preset files, `resources/keymaps/{modern,stata,vscode}.json`.
///
/// Two shapes are accepted: a bare `KeyBinding[]`, and design 06 §12.1's
/// `{ schema, id, name, bindings: [...] }` wrapper, which is what the desktop
/// app ships. Accepting both is not indecision — the resource files belong to
/// the frontend unit and this crate is the persistence layer under them, so
/// tolerating their envelope is cheaper than forcing a cross-unit rename of a
/// file we do not own.
#[derive(Deserialize)]
#[serde(untagged)]
enum PresetFile {
    Wrapped { bindings: Vec<KeyBinding> },
    Bare(Vec<KeyBinding>),
}

impl PresetFile {
    fn into_bindings(self) -> Vec<KeyBinding> {
        let mut v = match self {
            PresetFile::Wrapped { bindings } => bindings,
            PresetFile::Bare(v) => v,
        };
        for b in &mut v {
            b.source = Source::Preset;
        }
        v
    }
}

/// The three shipped keymaps (spec §33: "Presets: Modern, Stata-like,
/// VS Code-like, custom").
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KeymapPreset {
    /// The default.
    #[default]
    Modern,
    /// Stata compatibility.
    Stata,
    /// VS Code muscle memory.
    Vscode,
}

impl KeymapPreset {
    /// The id this preset is stored and requested under.
    pub const fn id(self) -> &'static str {
        match self {
            KeymapPreset::Modern => "modern",
            KeymapPreset::Stata => "stata",
            KeymapPreset::Vscode => "vscode",
        }
    }

    /// Parse an id.
    pub fn from_id(id: &str) -> Option<Self> {
        Some(match id {
            "modern" => KeymapPreset::Modern,
            "stata" => KeymapPreset::Stata,
            "vscode" => KeymapPreset::Vscode,
            _ => return None,
        })
    }

    /// Every preset, in menu order.
    pub const ALL: [KeymapPreset; 3] = [
        KeymapPreset::Modern,
        KeymapPreset::Stata,
        KeymapPreset::Vscode,
    ];
}

/// Where a binding came from. Design 06 §12.1: `"user"` sorts after `"preset"`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// Shipped with the product.
    #[default]
    Preset,
    /// From `<config>/keymaps/user.json`.
    User,
}

/// One binding. Transcribed from design 06 §12.1.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyBinding {
    /// Command id, e.g. `"run.block"`.
    pub command: String,
    /// `"Mod+Enter"`; chords are space-separated: `"Mod+K Mod+S"`.
    pub key: String,
    /// Boolean expression over the context keys in design 06 §12.1.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub when: Option<String>,
    /// Command arguments.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args: Option<serde_json::Value>,
    /// Preset or user.
    ///
    /// Defaulted, because the shipped `resources/keymaps/*.json` files do not
    /// carry it — a binding in a preset file is a preset binding by virtue of
    /// which file it is in, and [`KeymapStore::preset`] stamps it as one.
    #[serde(default)]
    pub source: Source,
}

impl KeyBinding {
    fn preset(command: &str, key: &str) -> Self {
        KeyBinding {
            command: command.to_owned(),
            key: key.to_owned(),
            when: None,
            args: None,
            source: Source::Preset,
        }
    }
}

/// The OS a keystroke is being rendered for.
///
/// Passed in rather than read from `cfg!(target_os)` so the keymap editor can
/// show a colleague's Windows accelerators, and so the tests below assert both
/// platforms on one machine.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Platform {
    /// `Mod` renders as `⌘`.
    #[default]
    Mac,
    /// `Mod` renders as `Ctrl`.
    Other,
}

impl Platform {
    /// This build's platform.
    pub fn host() -> Self {
        if cfg!(target_os = "macos") {
            Platform::Mac
        } else {
            Platform::Other
        }
    }
}

/// The Modern preset — design 06 §12.2, transcribed.
fn modern() -> Vec<KeyBinding> {
    [
        ("run.block", "Mod+Enter"),
        ("run.blockAndAdvance", "Shift+Enter"),
        ("run.selection", "Alt+Enter"),
        ("run.fromHere", "Mod+Alt+Enter"),
        ("run.fileClean", "Mod+Shift+Enter"),
        ("run.above", "Mod+Alt+Up"),
        ("run.below", "Mod+Alt+Down"),
        ("run.allStale", "Mod+Shift+R"),
        ("run.break", "Mod+."),
        ("commandBar.focus", "Mod+L"),
        ("palette.quickOpen", "Mod+P"),
        ("palette.commands", "Mod+Shift+P"),
        ("assistant.toggle", "Mod+J"),
        ("inline.cycleMode", "Mod+Alt+I"),
        ("view.toggleDocument", "Mod+Shift+V"),
        ("compare.models", "Mod+Shift+M"),
        ("data.browse", "Mod+Shift+D"),
        ("layout.modern", "Mod+Alt+1"),
        ("layout.classic", "Mod+Alt+2"),
        ("layout.focus", "Mod+Alt+3"),
        ("edit.toggleComment", "Mod+/"),
        ("help.token", "F1"),
        ("results.clear", "Mod+Shift+K"),
        ("results.collapseAll", "Mod+Alt+C"),
        ("results.clearBlock", "Mod+Shift+Backspace"),
        ("keymap.edit", "Mod+K Mod+S"),
    ]
    .into_iter()
    .map(|(c, k)| KeyBinding::preset(c, k))
    .collect()
}

/// The Stata-like preset: Modern plus the deltas a Stata user expects.
///
/// It is expressed as an overlay on Modern rather than a separate table so that
/// a command added to Modern is not silently missing here — the failure mode
/// where "the Stata keymap is the one that does not have the new shortcut".
fn stata() -> Vec<KeyBinding> {
    let mut v = modern();
    for (command, key) in [
        // Stata's own Do-file Editor runs with Mod+D / Mod+Shift+D.
        ("run.block", "Mod+D"),
        ("run.selection", "Mod+Shift+D"),
        ("data.browse", "Mod+8"),
        ("data.edit", "Mod+7"),
        ("viewer.open", "Mod+5"),
        ("commandBar.focus", "Mod+1"),
    ] {
        match v.iter_mut().find(|b| b.command == command) {
            Some(b) => b.key = key.to_owned(),
            None => v.push(KeyBinding::preset(command, key)),
        }
    }
    v
}

/// The VS Code-like preset: Modern plus the deltas VS Code muscle memory wants.
fn vscode() -> Vec<KeyBinding> {
    let mut v = modern();
    for (command, key) in [
        ("palette.commands", "Mod+Shift+P"),
        ("palette.quickOpen", "Mod+P"),
        ("edit.toggleComment", "Mod+/"),
        ("run.block", "Mod+Enter"),
        ("run.fileClean", "Mod+F5"),
        ("sidebar.toggle", "Mod+B"),
        ("terminal.toggle", "Mod+`"),
    ] {
        match v.iter_mut().find(|b| b.command == command) {
            Some(b) => b.key = key.to_owned(),
            None => v.push(KeyBinding::preset(command, key)),
        }
    }
    v
}

/// The **built-in floor** for one preset.
///
/// `resources/keymaps/<id>.json` is the shipped authority (design 06 §12.1) and
/// [`KeymapStore::load`] prefers it. This table is what the product does when
/// that file is missing, unreadable, or from a newer version of the app: an
/// installation with a damaged resource bundle still has working shortcuts
/// rather than a keyboard that does nothing.
pub fn preset_bindings(p: KeymapPreset) -> Vec<KeyBinding> {
    match p {
        KeymapPreset::Modern => modern(),
        KeymapPreset::Stata => stata(),
        KeymapPreset::Vscode => vscode(),
    }
}

/// Reading and writing keymaps.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct KeymapStore {
    /// `resources/keymaps/` in the installed app. May not exist.
    pub presets: Utf8PathBuf,
    /// `<config>/keymaps/`. `user.json` lives here.
    pub dir: Utf8PathBuf,
}

/// What went wrong.
#[derive(Debug, thiserror::Error)]
pub enum KeymapError {
    /// The overlay exists but is not readable as `KeyBinding[]`.
    #[error("{path} is not a readable keymap: {source}")]
    Malformed {
        /// The overlay's path.
        path: Utf8PathBuf,
        /// The parse error.
        #[source]
        source: serde_json::Error,
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

impl KeymapStore {
    /// A store over the shipped preset directory and the user's config
    /// directory.
    pub fn new(presets: impl Into<Utf8PathBuf>, dir: impl Into<Utf8PathBuf>) -> Self {
        KeymapStore {
            presets: presets.into(),
            dir: dir.into(),
        }
    }

    /// The shipped bindings of `preset`: the resource file if it is readable,
    /// otherwise [`preset_bindings`].
    pub fn preset(&self, preset: KeymapPreset) -> Vec<KeyBinding> {
        read_preset(&self.presets.join(format!("{}.json", preset.id())))
            .unwrap_or_else(|| preset_bindings(preset))
    }

    /// `<config>/keymaps/user.json`.
    pub fn overlay_path(&self) -> Utf8PathBuf {
        self.dir.join("user.json")
    }

    /// The user's overlay, or an empty list if there is none.
    ///
    /// A *malformed* overlay is an error rather than a silent empty list: it is
    /// the file the user hand-edited, and telling them it did not parse is the
    /// only way they will fix it.
    pub fn overlay(&self) -> Result<Vec<KeyBinding>, KeymapError> {
        let path = self.overlay_path();
        let raw = match std::fs::read(&path) {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => return Err(KeymapError::Io { path, source }),
        };
        let mut v: Vec<KeyBinding> = serde_json::from_slice(&raw)
            .map_err(|source| KeymapError::Malformed { path, source })?;
        // A binding in this file is a user binding whatever it claims to be;
        // otherwise a hand-edit could sort itself ahead of a preset.
        for b in &mut v {
            b.source = Source::User;
        }
        Ok(v)
    }

    /// `keymap_load { preset }` — the preset with the user's overlay applied.
    ///
    /// Both layers are returned in resolution order, presets first, so the
    /// keymap editor can show what shadows what. Nothing is removed.
    pub fn load(&self, preset: KeymapPreset) -> Result<Vec<KeyBinding>, KeymapError> {
        let mut v = self.preset(preset);
        v.extend(self.overlay()?);
        Ok(v)
    }

    /// `keymap_save { bindings }` — writes the user layer only.
    ///
    /// Preset bindings in `bindings` are dropped rather than copied into the
    /// overlay: freezing today's preset into the user's file is how someone
    /// stops receiving keymap improvements without ever choosing to.
    pub fn save(&self, bindings: &[KeyBinding]) -> Result<Utf8PathBuf, KeymapError> {
        let user: Vec<&KeyBinding> = bindings
            .iter()
            .filter(|b| b.source == Source::User)
            .collect();
        std::fs::create_dir_all(&self.dir).map_err(|source| KeymapError::Io {
            path: self.dir.clone(),
            source,
        })?;
        let path = self.overlay_path();
        let mut s = serde_json::to_string_pretty(&user).expect("KeyBinding is always encodable");
        s.push('\n');
        write_bytes_atomic(&path, s.as_bytes()).map_err(|source| KeymapError::Io {
            path: path.clone(),
            source,
        })?;
        Ok(path)
    }
}

fn read_preset(path: &Utf8Path) -> Option<Vec<KeyBinding>> {
    let raw = std::fs::read(path).ok()?;
    let f: PresetFile = serde_json::from_slice(&raw).ok()?;
    let v = f.into_bindings();
    // An empty resource file is indistinguishable from a broken one and must not
    // beat the built-in floor.
    (!v.is_empty()).then_some(v)
}

/// Resolve `bindings` for one command into the accelerator the menu should show.
///
/// Design 06 §12.1: candidates are filtered and **the last one wins**, with user
/// bindings sorted after presets. Returns `None` when the command has no
/// binding — which the menu renders as no accelerator, not as an empty string.
pub fn accelerator(bindings: &[KeyBinding], command: &str, platform: Platform) -> Option<String> {
    bindings
        .iter()
        .filter(|b| b.command == command)
        .max_by_key(|b| b.source)
        .map(|b| render(&b.key, platform))
}

/// `menu_accelerator { action, preset }` over the shipped presets plus an
/// overlay.
pub fn menu_accelerator(
    store: &KeymapStore,
    action: &str,
    preset: KeymapPreset,
    platform: Platform,
) -> Result<Option<String>, KeymapError> {
    Ok(accelerator(&store.load(preset)?, action, platform))
}

/// Render a binding string for a platform: `Mod` becomes `⌘` on macOS and
/// `Ctrl` everywhere else, and macOS uses the conventional glyphs.
///
/// `Meta`, `Ctrl`, `Alt` and `Shift` written explicitly are left explicit —
/// design 06 §12.1 keeps them for bindings that must differ per platform, and
/// rewriting them would defeat that.
pub fn render(key: &str, platform: Platform) -> String {
    key.split(' ')
        .map(|chord| {
            chord
                .split('+')
                .map(|part| match (part, platform) {
                    ("Mod", Platform::Mac) => "⌘",
                    ("Mod", Platform::Other) => "Ctrl",
                    ("Meta", Platform::Mac) => "⌘",
                    ("Alt", Platform::Mac) => "⌥",
                    ("Shift", Platform::Mac) => "⇧",
                    ("Ctrl", Platform::Mac) => "⌃",
                    (other, _) => other,
                })
                .collect::<Vec<_>>()
                .join(match platform {
                    // macOS accelerators are glyphs run together; Windows and
                    // Linux spell them out with `+`.
                    Platform::Mac => "",
                    Platform::Other => "+",
                })
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, KeymapStore) {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let s = KeymapStore::new(root.join("resources"), root.join("keymaps"));
        (tmp, s)
    }

    #[test]
    fn mod_resolves_per_platform_and_the_frontend_never_sees_a_raw_mod() {
        assert_eq!(render("Mod+Enter", Platform::Mac), "⌘Enter");
        assert_eq!(render("Mod+Enter", Platform::Other), "Ctrl+Enter");
        assert_eq!(render("Mod+K Mod+S", Platform::Mac), "⌘K ⌘S");
        assert_eq!(render("Mod+K Mod+S", Platform::Other), "Ctrl+K Ctrl+S");
        assert_eq!(render("Mod+Shift+P", Platform::Mac), "⌘⇧P");
    }

    #[test]
    fn every_preset_binds_the_core_run_commands() {
        for p in KeymapPreset::ALL {
            let v = preset_bindings(p);
            for c in [
                "run.block",
                "run.selection",
                "run.fileClean",
                "palette.commands",
            ] {
                assert!(
                    v.iter().any(|b| b.command == c),
                    "{} is missing {c}",
                    p.id()
                );
            }
        }
    }

    #[test]
    fn the_stata_preset_uses_stata_run_keys() {
        let v = preset_bindings(KeymapPreset::Stata);
        assert_eq!(
            accelerator(&v, "run.block", Platform::Other).as_deref(),
            Some("Ctrl+D")
        );
    }

    #[test]
    fn a_user_binding_shadows_a_preset_binding() {
        let mut v = preset_bindings(KeymapPreset::Modern);
        v.push(KeyBinding {
            command: "run.block".into(),
            key: "Mod+R".into(),
            when: None,
            args: None,
            source: Source::User,
        });
        assert_eq!(
            accelerator(&v, "run.block", Platform::Other).as_deref(),
            Some("Ctrl+R")
        );
    }

    #[test]
    fn an_unbound_command_has_no_accelerator() {
        let v = preset_bindings(KeymapPreset::Modern);
        assert_eq!(accelerator(&v, "nothing.bound", Platform::Mac), None);
    }

    #[test]
    fn presets_persist_and_reload_with_the_user_overlay() {
        let (_t, s) = store();
        assert!(s.overlay().unwrap().is_empty());

        let mut bindings = s.load(KeymapPreset::Modern).unwrap();
        bindings.push(KeyBinding {
            command: "run.block".into(),
            key: "F9".into(),
            when: Some("editorFocus".into()),
            args: None,
            source: Source::User,
        });
        let path = s.save(&bindings).unwrap();
        assert!(path.exists());

        // Only the user layer was written…
        let overlay = s.overlay().unwrap();
        assert_eq!(overlay.len(), 1);
        assert_eq!(overlay[0].when.as_deref(), Some("editorFocus"));
        // …and it wins on reload.
        let reloaded = s.load(KeymapPreset::Modern).unwrap();
        assert_eq!(
            accelerator(&reloaded, "run.block", Platform::Mac).as_deref(),
            Some("F9")
        );
        // Switching preset keeps the overlay.
        let stata = s.load(KeymapPreset::Stata).unwrap();
        assert_eq!(
            accelerator(&stata, "run.block", Platform::Mac).as_deref(),
            Some("F9")
        );
    }

    #[test]
    fn a_hand_edited_overlay_cannot_claim_to_be_a_preset() {
        let (_t, s) = store();
        std::fs::create_dir_all(&s.dir).unwrap();
        std::fs::write(
            s.overlay_path(),
            br#"[{"command":"run.block","key":"F5","source":"preset"}]"#,
        )
        .unwrap();
        assert_eq!(s.overlay().unwrap()[0].source, Source::User);
        assert_eq!(
            menu_accelerator(&s, "run.block", KeymapPreset::Modern, Platform::Other)
                .unwrap()
                .as_deref(),
            Some("F5")
        );
    }

    #[test]
    fn a_shipped_resource_file_beats_the_built_in_floor() {
        let (_t, s) = store();
        std::fs::create_dir_all(&s.presets).unwrap();
        // Design 06 §12.1's envelope, which is what the desktop app ships.
        std::fs::write(
            s.presets.join("modern.json"),
            br#"{"schema":1,"id":"modern","name":"Modern",
                 "bindings":[{"command":"run.block","key":"Mod+G"}]}"#,
        )
        .unwrap();
        assert_eq!(
            accelerator(
                &s.preset(KeymapPreset::Modern),
                "run.block",
                Platform::Other
            )
            .as_deref(),
            Some("Ctrl+G")
        );
        // A bare array is accepted too.
        std::fs::write(
            s.presets.join("stata.json"),
            br#"[{"command":"run.block","key":"Mod+H"}]"#,
        )
        .unwrap();
        assert_eq!(
            accelerator(&s.preset(KeymapPreset::Stata), "run.block", Platform::Other).as_deref(),
            Some("Ctrl+H")
        );
        // A broken one falls back to the floor rather than leaving a dead keyboard.
        std::fs::write(s.presets.join("vscode.json"), b"[]").unwrap();
        assert_eq!(
            s.preset(KeymapPreset::Vscode),
            preset_bindings(KeymapPreset::Vscode)
        );
    }

    #[test]
    fn a_malformed_overlay_is_reported_not_swallowed() {
        let (_t, s) = store();
        std::fs::create_dir_all(&s.dir).unwrap();
        std::fs::write(s.overlay_path(), b"{ not an array }").unwrap();
        assert!(matches!(
            s.overlay().unwrap_err(),
            KeymapError::Malformed { .. }
        ));
    }
}
