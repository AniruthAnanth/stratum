//! macOS menu policy — 08 §5.4, spec §33.
//!
//! What this owns is the platform's *policy*: a global menu bar, an application
//! menu with Services and Hide, Settings under it rather than under Edit, and
//! `Mod` resolving to `⌘`. What it does not own is the `NSMenu` itself; see
//! [`MenuSink`] for why that is the application shell's, and
//! [`MacosMenuHost::with_sink`] for how the shell supplies it.

use std::sync::Arc;

use stratum_platform::{
    Accelerator, ActionId, Keymap, KeymapPreset, MenuHandle, MenuHost, MenuModel, MenuPatch,
    MenuPlacement, MenuSink, MenuTarget, PlatformError, PlatformId, Result, StaticKeymap,
    SystemMenuItems,
};

/// [`MenuHost`] for macOS.
pub struct MacosMenuHost {
    keymap: Arc<dyn Keymap>,
    sink: Option<Arc<dyn MenuSink>>,
}

impl std::fmt::Debug for MacosMenuHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MacosMenuHost")
            .field("has_sink", &self.sink.is_some())
            .finish()
    }
}

impl Default for MacosMenuHost {
    fn default() -> Self {
        Self::new()
    }
}

impl MacosMenuHost {
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

impl MenuHost for MacosMenuHost {
    fn install(&self, model: &MenuModel, target: MenuTarget) -> Result<MenuHandle> {
        // macOS has ONE menu bar. Installing "for a window" is not a thing the
        // OS can do, and silently installing it application-wide instead would
        // make a per-window menu look like it worked.
        if !matches!(target, MenuTarget::Application) {
            return Err(PlatformError::Unsupported(
                "macOS has a single application menu bar; install with MenuTarget::Application",
            ));
        }
        self.sink()?.install(model, target)
    }

    fn update(&self, handle: MenuHandle, patch: &MenuPatch) -> Result<()> {
        self.sink()?.update(handle, patch)
    }

    fn placement(&self) -> MenuPlacement {
        MenuPlacement::GlobalMenuBar
    }

    fn accelerator(&self, action: &ActionId, preset: KeymapPreset) -> Option<Accelerator> {
        // The one line the whole "no ⌘ in the frontend" rule rests on.
        self.keymap
            .accelerator(action, preset)
            .map(|a| a.resolve(PlatformId::MacOs))
    }

    fn system_items(&self) -> SystemMenuItems {
        SystemMenuItems::for_platform(PlatformId::MacOs)
    }
}
