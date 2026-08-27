//! Windows menu policy — 08 §5.4, spec §33.
//!
//! A menu bar **per window**, `Settings…` under Edit, `Exit` rather than
//! `Quit`, no Services and no Hide — and `Mod` resolving to `Ctrl`. That last
//! line is the whole mechanism behind "the frontend never writes `⌘` or
//! `Ctrl+`": the renderer asks [`MenuHost::accelerator`] and gets back a
//! keystroke whose logical [`stratum_platform::Modifiers::MOD`] bit is already
//! gone.
//!
//! Nothing here calls Win32. Building the `HMENU` belongs to the application
//! shell — Tauri already owns the window's menu bar, and a second authority
//! setting it is a race whose winner depends on startup order — so this host
//! owns the *policy* and delegates the toolkit call to a
//! [`MenuSink`] the shell installs. That is also why this module compiles and
//! is tested on every host: it is the half of the Windows menu story that has
//! no syscall in it.

use std::sync::Arc;

use stratum_platform::{
    Accelerator, ActionId, Keymap, KeymapPreset, MenuHandle, MenuHost, MenuModel, MenuPatch,
    MenuPlacement, MenuSink, MenuTarget, PlatformError, PlatformId, Result, StaticKeymap,
    SystemMenuItems,
};

/// [`MenuHost`] for Windows.
pub struct WindowsMenuHost {
    keymap: Arc<dyn Keymap>,
    sink: Option<Arc<dyn MenuSink>>,
}

impl std::fmt::Debug for WindowsMenuHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WindowsMenuHost")
            .field("has_sink", &self.sink.is_some())
            .finish()
    }
}

impl Default for WindowsMenuHost {
    fn default() -> Self {
        Self::new()
    }
}

impl WindowsMenuHost {
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
    /// editor's key handler cannot disagree about what a command is bound to.
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
                "no menu sink is installed; this process has no window to hang a menu bar on"
                    .to_owned(),
            )
        })
    }
}

impl MenuHost for WindowsMenuHost {
    fn install(&self, model: &MenuModel, target: MenuTarget) -> Result<MenuHandle> {
        // The mirror image of the macOS host's refusal. There is no
        // application-wide menu bar on Windows, and installing into "the
        // first window we happen to find" instead would make an application
        // install look like it worked right up until a second window opened
        // without a menu.
        if matches!(target, MenuTarget::Application) {
            return Err(PlatformError::Unsupported(
                "Windows has one menu bar per window; install with MenuTarget::Window",
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
        // The one line the whole "no Ctrl+ literal in the frontend" rule rests
        // on. Resolved here, so a consumer cannot forget to.
        self.keymap
            .accelerator(action, preset)
            .map(|a| a.resolve(PlatformId::Windows))
    }

    fn system_items(&self) -> SystemMenuItems {
        SystemMenuItems::for_platform(PlatformId::Windows)
    }
}
