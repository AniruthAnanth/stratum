//! macOS implementation of the Stratum platform adapter layer — 08 §5.
//!
//! One `MacosPlatform` per process, obtained through `stratum-platform-host`.
//! Everything OS-specific in Stratum is reachable from here and from nowhere
//! else: `deny.toml` names this crate as the only permitted wrapper for
//! `objc2*`, `security-framework` and `rfd`, which makes 08 §5.0 rule 2 a
//! machine check rather than a review comment.
//!
//! # `Unsupported` is a normal answer here
//!
//! Three of these adapters are unavailable in an unbundled process, which is
//! every `cargo run`, every `cargo test` and every CI job: notifications need a
//! bundle identifier, the dock badge needs an `NSApplication`, and
//! `AppBundleSwap` needs a bundle to swap. They return
//! [`stratum_platform::PlatformError::Unsupported`] rather than panicking, and
//! `tests/unsupported.rs` asserts exactly that.
#![cfg(target_os = "macos")]

pub mod bundle;
pub mod credentials;
pub mod dialogs;
pub mod dock;
pub mod menus;
pub mod notify;
pub mod process;
pub mod shell;
pub mod updater;

use std::sync::Arc;

use camino::Utf8PathBuf;
use stratum_platform::{
    CredentialStore, FileDialogs, Keymap, MenuHost, MenuSink, Notifier, Paths, Platform,
    PlatformId, ProcessHost, Result, ShellIntegration, UpdateFeed, Updater,
};

pub use credentials::Keychain;
pub use dialogs::MacosFileDialogs;
pub use menus::MacosMenuHost;
pub use notify::MacosNotifier;
pub use process::MacosProcessHost;
pub use shell::MacosShell;
pub use updater::MacosUpdater;

/// The macOS [`Platform`].
#[derive(Debug)]
pub struct MacosPlatform {
    paths: Paths,
    credentials: Keychain,
    dialogs: MacosFileDialogs,
    menus: MacosMenuHost,
    updater: MacosUpdater,
    shell: MacosShell,
    processes: MacosProcessHost,
    notifier: MacosNotifier,
}

/// Everything the shell may inject at construction.
///
/// A struct rather than six constructor arguments because the shell fills in
/// two of them (the keymap, the menu sink) at startup and the other two
/// (the update feed, the version) come from the binary's own build metadata —
/// and a six-argument `new` is a six-argument `new` that someone will pass in
/// the wrong order.
#[derive(Default)]
pub struct MacosConfig {
    /// The version this build reports to the update feed.
    pub version: String,
    /// Where a staged update is downloaded. Defaults to the cache directory.
    pub staging_dir: Option<Utf8PathBuf>,
    /// The persisted keymap. Defaults to the built-in preset tables.
    pub keymap: Option<Arc<dyn Keymap>>,
    /// The toolkit half of the menu host. Absent in a headless process.
    pub menu_sink: Option<Arc<dyn MenuSink>>,
    /// The network half of the updater. Absent below the desktop.
    pub update_feed: Option<Arc<dyn UpdateFeed>>,
}

impl MacosPlatform {
    /// Build the platform.
    ///
    /// # Errors
    /// [`stratum_platform::PlatformError::BackendUnavailable`] when the home
    /// directory cannot be resolved, which is the only input with no defensible
    /// default.
    pub fn new(config: MacosConfig) -> Result<Self> {
        let paths = Paths::discover()?;
        let staging = config
            .staging_dir
            .unwrap_or_else(|| paths.cache_dir().join("updates"));

        let mut menus = match config.keymap {
            Some(k) => MacosMenuHost::with_keymap(k),
            None => MacosMenuHost::new(),
        };
        if let Some(sink) = config.menu_sink {
            menus = menus.with_sink(sink);
        }

        let mut updater = MacosUpdater::new(config.version, staging);
        if let Some(feed) = config.update_feed {
            updater = updater.with_feed(feed);
        }

        Ok(Self {
            paths,
            credentials: Keychain::new(),
            dialogs: MacosFileDialogs::new(),
            menus,
            updater,
            shell: MacosShell::new(),
            processes: MacosProcessHost::new(),
            notifier: MacosNotifier::new(),
        })
    }
}

impl Platform for MacosPlatform {
    fn id(&self) -> PlatformId {
        PlatformId::MacOs
    }
    fn paths(&self) -> &Paths {
        &self.paths
    }
    fn credentials(&self) -> &dyn CredentialStore {
        &self.credentials
    }
    fn dialogs(&self) -> &dyn FileDialogs {
        &self.dialogs
    }
    fn menus(&self) -> &dyn MenuHost {
        &self.menus
    }
    fn updater(&self) -> &dyn Updater {
        &self.updater
    }
    fn shell(&self) -> &dyn ShellIntegration {
        &self.shell
    }
    fn processes(&self) -> &dyn ProcessHost {
        &self.processes
    }
    fn notifier(&self) -> &dyn Notifier {
        &self.notifier
    }
}
