//! Linux menu policy — 08 §5.4, spec §33.
//!
//! What this owns is the platform's *policy*: a menu bar per window, no
//! application menu, Preferences under File rather than under an app menu, and
//! `Mod` resolving to `Ctrl`. What it does not own is the `GtkMenuBar`; see
//! [`MenuSink`] for why that belongs to the application shell, which on Linux
//! is doubly true — Tauri's window already owns a GTK widget hierarchy, and a
//! second authority attaching a menu bar to the same `GtkApplicationWindow` is
//! a race whose winner depends on startup order.
//!
//! # Why there is no global-menu special case
//!
//! Unity's `appmenu`, KDE's `plasma-workspace` global menu and GNOME's
//! `gnome-shell-extension-appmenu` all export a `com.canonical.dbusmenu` tree
//! and then *render it wherever the desktop wants*. From the application's side
//! that is still one menu per window: the desktop moves it, we do not. Reporting
//! [`MenuPlacement::GlobalMenuBar`] because the user happens to run KDE would
//! make the shell build a single application menu that no window owns.

use std::sync::Arc;

use stratum_platform::{
    Accelerator, ActionId, Keymap, KeymapPreset, MenuHandle, MenuHost, MenuModel, MenuPatch,
    MenuPlacement, MenuSink, MenuTarget, PlatformError, PlatformId, Result, StaticKeymap,
    SystemMenuItems,
};

/// [`MenuHost`] for Linux.
pub struct LinuxMenuHost {
    keymap: Arc<dyn Keymap>,
    sink: Option<Arc<dyn MenuSink>>,
}

impl std::fmt::Debug for LinuxMenuHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LinuxMenuHost")
            .field("has_sink", &self.sink.is_some())
            .finish()
    }
}

impl Default for LinuxMenuHost {
    fn default() -> Self {
        Self::new()
    }
}

impl LinuxMenuHost {
    /// With the built-in preset table and no sink. Accelerator resolution works
    /// immediately; installing a menu bar reports
    /// [`PlatformError::BackendUnavailable`] until a shell supplies a sink.
    #[must_use]
    pub fn new() -> Self {
        Self {
            keymap: Arc::new(StaticKeymap::builtin()),
            sink: None,
        }
    }

    /// With the keymap the workspace persisted, so the menu bar and the
    /// editor's key handler cannot disagree.
    #[must_use]
    pub fn with_keymap(keymap: Arc<dyn Keymap>) -> Self {
        Self { keymap, sink: None }
    }

    /// Attach the toolkit sink.
    #[must_use]
    pub fn with_sink(mut self, sink: Arc<dyn MenuSink>) -> Self {
        self.sink = Some(sink);
        self
    }

    fn sink(&self) -> Result<&dyn MenuSink> {
        self.sink.as_deref().ok_or_else(|| {
            PlatformError::BackendUnavailable(
                "no menu sink is installed; this process has no menu bar".to_owned(),
            )
        })
    }
}

impl MenuHost for LinuxMenuHost {
    fn install(&self, model: &MenuModel, target: MenuTarget) -> Result<MenuHandle> {
        // The mirror image of the macOS rule. There is no application-wide menu
        // bar to install into on Linux, and quietly attaching the model to some
        // arbitrary window instead would look like it worked until the user
        // opened a second one (§26).
        if matches!(target, MenuTarget::Application) {
            return Err(PlatformError::Unsupported(
                "Linux menus are per-window; install with MenuTarget::Window(handle)",
            ));
        }
        self.sink()?.install(model, target)
    }

    fn update(&self, handle: MenuHandle, patch: &MenuPatch) -> Result<()> {
        self.sink()?.update(handle, patch)
    }

    fn placement(&self) -> MenuPlacement {
        MenuPlacement::PerWindow
    }

    fn accelerator(&self, action: &ActionId, preset: KeymapPreset) -> Option<Accelerator> {
        // The one line the whole "no `⌘` or `Ctrl+` literal in the frontend"
        // rule rests on, on this platform.
        self.keymap
            .accelerator(action, preset)
            .map(|a| a.resolve(PlatformId::Linux))
    }

    fn system_items(&self) -> SystemMenuItems {
        SystemMenuItems::for_platform(PlatformId::Linux)
    }
}
