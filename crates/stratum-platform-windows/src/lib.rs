//! Windows implementation of the Stratum platform adapter layer — 08 §5.
//!
//! One `WindowsPlatform` per process, obtained through `stratum-platform-host`.
//! Everything OS-specific in Stratum is reachable from here and from nowhere
//! else: `deny.toml` names this crate as the only permitted wrapper for
//! `windows` and `windows-sys`, which makes 08 §5.0 rule 2 a machine check
//! rather than a review comment.
//!
//! # This crate is not `#![cfg(target_os = "windows")]`
//!
//! Its macOS sibling is, and the difference is deliberate. That crate is
//! developed and tested on the machine it targets; this one is not, and a crate
//! that compiles to an empty library everywhere except Windows would make
//! `cargo test -p stratum-platform-windows` a test suite of zero tests reported
//! as green. So the split runs along a different line:
//!
//! * **Policy is always compiled.** The accelerator resolution, the `Path` list
//!   algebra behind the CLI shim, the `%VAR%` expansion, the two-registry-block
//!   environment merge, the toast XML and its escaping, the Credential Manager
//!   target grammar and blob encoding, the
//!   `GetLogicalProcessorInformationEx` record walk, the installer argument
//!   vector, the AUMID rules and the HRESULT taxonomy are all pure functions
//!   over their inputs. They carry this unit's real bug surface, and they are
//!   asserted on every host — including for a heterogeneous P/E CPU and a
//!   domain user's roaming `Path`, neither of which the development machine
//!   has.
//! * **Syscalls live in `mod sys`** inside each module, behind
//!   `#[cfg(target_os = "windows")]`, and are verified with
//!   `cargo check --target x86_64-pc-windows-msvc`.
//!
//! This is the same technique `stratum_platform::Paths::resolve` uses to assert
//! the Windows column of 08 §5.2's table from a macOS test run, applied to a
//! whole implementation crate rather than one function.
//!
//! # `Unsupported` is a normal answer here
//!
//! Three capabilities are unavailable in a build that was not put on the
//! machine by the installer, which is every `cargo run`, every `cargo test`,
//! every CI job and every portable unzip: toasts need a Start-menu shortcut
//! carrying our AppUserModelID (see [`aumid`]), the updater needs an
//! installation to re-install over, and Windows has no application badge at
//! all. They return [`stratum_platform::PlatformError::Unsupported`] rather
//! than panicking, and `tests/unsupported.rs` asserts exactly that.

pub mod aumid;
pub mod credentials;
pub mod dialogs;
pub mod menus;
pub mod notify;
pub mod process;
pub mod shell;
pub mod updater;
pub mod win;

#[cfg(target_os = "windows")]
pub mod registry;

pub use menus::WindowsMenuHost;

#[cfg(target_os = "windows")]
pub use {
    credentials::CredentialManager, dialogs::WindowsFileDialogs, notify::WindowsNotifier,
    process::WindowsProcessHost, shell::WindowsShell, updater::WindowsUpdater,
};

#[cfg(target_os = "windows")]
pub use platform::{WindowsConfig, WindowsPlatform};

#[cfg(target_os = "windows")]
mod platform {
    use std::sync::Arc;

    use camino::Utf8PathBuf;
    use stratum_platform::{
        CredentialStore, FileDialogs, Keymap, MenuHost, MenuSink, Notifier, Paths, Platform,
        PlatformId, ProcessHost, Result, ShellIntegration, UpdateFeed, Updater,
    };

    use crate::{
        CredentialManager, WindowsFileDialogs, WindowsMenuHost, WindowsNotifier,
        WindowsProcessHost, WindowsShell, WindowsUpdater,
    };

    /// The Windows [`Platform`].
    #[derive(Debug)]
    pub struct WindowsPlatform {
        paths: Paths,
        credentials: CredentialManager,
        dialogs: WindowsFileDialogs,
        menus: WindowsMenuHost,
        updater: WindowsUpdater,
        shell: WindowsShell,
        processes: WindowsProcessHost,
        notifier: WindowsNotifier,
    }

    /// Everything the shell may inject at construction.
    ///
    /// Field-for-field the same shape as `MacosConfig`, so that
    /// `stratum-platform-host` can alias one of them to `HostConfig` per target
    /// and the desktop's startup code is not written twice.
    #[derive(Default)]
    pub struct WindowsConfig {
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

    impl WindowsPlatform {
        /// Build the platform.
        ///
        /// Registers our AppUserModelID as a side effect, because it has to
        /// happen before the first window is shown and before the first toast —
        /// the shell decides a process's taskbar identity once. A failure is
        /// *not* fatal: an id the shell refuses means notifications will report
        /// [`stratum_platform::PlatformError::Unsupported`] later, which is a
        /// state the UI renders, and a Stratum that will not start because a
        /// toast could not be registered would be a strictly worse product.
        ///
        /// # Errors
        /// [`stratum_platform::PlatformError::BackendUnavailable`] when the home
        /// directory cannot be resolved, which is the only input with no
        /// defensible default.
        pub fn new(config: WindowsConfig) -> Result<Self> {
            let paths = Paths::discover()?;
            let staging = config
                .staging_dir
                .unwrap_or_else(|| paths.cache_dir().join("updates"));

            let _ = crate::aumid::register_for_process();

            let mut menus = match config.keymap {
                Some(k) => WindowsMenuHost::with_keymap(k),
                None => WindowsMenuHost::new(),
            };
            if let Some(sink) = config.menu_sink {
                menus = menus.with_sink(sink);
            }

            let mut updater = WindowsUpdater::new(config.version, staging);
            if let Some(feed) = config.update_feed {
                updater = updater.with_feed(feed);
            }

            Ok(Self {
                paths,
                credentials: CredentialManager::new(),
                dialogs: WindowsFileDialogs::new(),
                menus,
                updater,
                shell: WindowsShell::new(),
                processes: WindowsProcessHost::new(),
                notifier: WindowsNotifier::new(),
            })
        }
    }

    impl Platform for WindowsPlatform {
        fn id(&self) -> PlatformId {
            PlatformId::Windows
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
}
