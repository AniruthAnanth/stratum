//! The [`Platform`] aggregate — 08 §5.1, CONTRACTS §13.
//!
//! Linux-only, because it wires up the three adapters that issue syscalls and
//! D-Bus calls. Everything it composes is testable on any host; this file is
//! the wiring, and `tests/unsupported.rs` exercises the same wiring through the
//! parts that are not gated.

use std::sync::Arc;

use camino::Utf8PathBuf;
use stratum_platform::{
    CredentialStore, Env, FileDialogs, Keymap, MenuHost, MenuSink, Notifier, Paths, Platform,
    PlatformId, ProcessHost, Result, ShellIntegration, SystemEnv, UpdateFeed, Updater,
};

use crate::credentials::{self, LinuxCredentials};
use crate::dialogs::{GtkFallback, LinuxFileDialogs};
use crate::menus::LinuxMenuHost;
use crate::notify::LinuxNotifier;
use crate::packaging::Packaging;
use crate::portal::XdgPortal;
use crate::process::LinuxProcessHost;
use crate::secretfile::EncryptedFileStore;
use crate::secretservice::SecretServiceClient;
use crate::shell::{LinuxShell, XdgDirs};
use crate::updater::LinuxUpdater;

/// The Linux [`Platform`].
#[derive(Debug)]
pub struct LinuxPlatform {
    paths: Paths,
    credentials: LinuxCredentials,
    dialogs: LinuxFileDialogs,
    menus: LinuxMenuHost,
    updater: LinuxUpdater,
    shell: LinuxShell,
    processes: LinuxProcessHost,
    notifier: LinuxNotifier,
}

/// Everything the shell may inject at construction.
///
/// A struct rather than seven constructor arguments, for the same reason as
/// `MacosConfig`: the shell fills in the keymap, the menu sink and the GTK
/// chooser at startup, the binary's build metadata supplies the rest, and a
/// seven-argument `new` is a seven-argument `new` that someone will pass in the
/// wrong order.
#[derive(Default)]
pub struct LinuxConfig {
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
    /// The GTK `FileChooserNative` fallback, used when no portal answers within
    /// [`crate::dialogs::PORTAL_DEADLINE`]. Supplied by `stratum-desktop`,
    /// which already owns a GTK main loop; see [`crate::dialogs`] for why this
    /// crate does not link one.
    pub gtk_fallback: Option<Arc<dyn GtkFallback>>,
}

impl LinuxPlatform {
    /// Build the platform.
    ///
    /// # Errors
    /// [`stratum_platform::PlatformError::BackendUnavailable`] when the home
    /// directory cannot be resolved, which is the only input with no defensible
    /// default. Notably NOT when there is no D-Bus, no keyring, no portal and
    /// no notification daemon: all four are expected states this layer reports
    /// rather than fails on, and a headless CI runner has none of them.
    pub fn new(config: LinuxConfig) -> Result<Self> {
        Self::with_env(config, &SystemEnv)
    }

    /// [`LinuxPlatform::new`] over an injected environment.
    ///
    /// # Errors
    /// As [`LinuxPlatform::new`].
    pub fn with_env(config: LinuxConfig, env: &dyn Env) -> Result<Self> {
        let paths = Paths::resolve(PlatformId::Linux, env)?;
        let staging = config
            .staging_dir
            .unwrap_or_else(|| paths.cache_dir().join("updates"));
        let exe = std::env::current_exe()
            .ok()
            .and_then(|p| Utf8PathBuf::from_path_buf(p).ok());
        let packaging = Packaging::detect(env, exe.as_deref());

        let mut menus = match config.keymap {
            Some(k) => LinuxMenuHost::with_keymap(k),
            None => LinuxMenuHost::new(),
        };
        if let Some(sink) = config.menu_sink {
            menus = menus.with_sink(sink);
        }

        let mut updater = LinuxUpdater::new(config.version, staging, packaging.clone());
        if let Some(feed) = config.update_feed {
            updater = updater.with_feed(feed);
        }

        let mut dialogs = LinuxFileDialogs::new(Arc::new(XdgPortal::new()));
        if let Some(gtk) = config.gtk_fallback {
            dialogs = dialogs.with_fallback(gtk);
        }

        // Nothing here touches the bus: `SecretServiceClient::new` is a `const
        // fn` and the demotion happens on the first real call. Constructing a
        // platform must never block on D-Bus, because `stratum serve` does it
        // at startup on machines that have none.
        let credentials = LinuxCredentials::new(
            Some(Arc::new(SecretServiceClient::new())),
            EncryptedFileStore::new(
                credentials::fallback_path(paths.state_dir()),
                credentials::machine_secret(env),
            ),
        );

        Ok(Self {
            paths,
            credentials,
            dialogs,
            menus,
            updater,
            shell: LinuxShell::new(XdgDirs::resolve(env)?, packaging, exe),
            processes: LinuxProcessHost::new(),
            notifier: LinuxNotifier::new(),
        })
    }

    /// The dialog attempt counters, for the acceptance gate in
    /// `tests/dialogs.rs`. See [`crate::dialogs::DialogAttempts`].
    #[must_use]
    pub fn dialog_attempts(&self) -> &crate::dialogs::DialogAttempts {
        self.dialogs.attempts()
    }

    /// The credential store, concretely, so the Settings pane can render
    /// "one probe, then the encrypted file" without downcasting.
    #[must_use]
    pub fn linux_credentials(&self) -> &LinuxCredentials {
        &self.credentials
    }
}

impl Platform for LinuxPlatform {
    fn id(&self) -> PlatformId {
        PlatformId::Linux
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
