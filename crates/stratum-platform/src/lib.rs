//! The platform adapter layer — spec §28, `docs/design/08-platform-packaging-ci.md` §5.
//!
//! This crate is **traits and plain data only**. It contains no `cfg(target_os)`
//! branch that changes behaviour, no OS crate, and no I/O beyond the small
//! amount of `std::fs` that [`paths`] needs to create a directory. The three
//! implementation crates (`stratum-platform-{macos,windows,linux}`) are the only
//! places in the repository allowed to reach an OS API; `stratum-platform-host`
//! picks one of them at compile time and hands back a `&'static dyn Platform`.
//!
//! # What must never leak upward
//!
//! File dialogs, keychains, menus, `NSApplication`, D-Bus, toasts, `%APPDATA%`,
//! `~/Library`, `xdg-open`, the Windows registry, notification permissions,
//! update channels — **and path resolution**. `stratum-runtime` never asks
//! "where is the cache directory?"; it is handed a [`Paths`] at construction.
//! That is what makes spec §28's "statistical semantics are platform
//! independent" a structural property rather than a review comment.
//!
//! # [`PlatformError::Unsupported`] and [`PlatformError::Cancelled`] are answers
//!
//! Not errors to be logged and swallowed, and not conditions to `unwrap` away.
//! A Linux box with no portal, a `.deb` install where updating is `apt`'s job, a
//! headless CI runner with no notification daemon, a user who pressed Escape in
//! a save panel — all of these are *expected* outcomes of a correct program, and
//! every caller has to render them. `Cancelled` in particular is why
//! [`FileDialogs::open_files`] returns `Result<Vec<_>>` and never an empty
//! `Vec`: "the user cancelled" and "the user selected nothing" are different
//! events with different consequences for an unsaved buffer.

pub mod credentials;
pub mod dialogs;
pub mod menus;
pub mod notify;
pub mod paths;
pub mod process;
pub mod shell;
pub mod updater;

pub use credentials::{CredentialBackend, CredentialStore};
pub use dialogs::{
    ExternalUrl, FileDialogs, FileFilter, FolderRequest, OpenRequest, SaveRequest, WindowHandle,
};
pub use menus::{
    Accelerator, ActionId, Key, Keymap, KeymapPreset, MenuHandle, MenuHost, MenuItem, MenuModel,
    MenuPatch, MenuPlacement, MenuRole, MenuSink, MenuTarget, Modifiers, SettingsLocation,
    StaticKeymap, SystemMenuItems,
};
pub use notify::{
    Badge, Notification, NotificationAction, NotificationId, Notifier, NotifierCaps,
    PermissionState,
};
pub use paths::{Env, Paths, SystemEnv};
pub use process::{EnvPolicy, ExitStatus, ProcessHost, ProcessSpec, QosClass, SupervisedChild};
pub use shell::{
    Association, HandlerInfo, HandlerRole, InstallScope, ShellIntegration, ShellKind, ShimStatus,
};
pub use updater::{
    Channel, Progress, ProgressFn, StagedUpdate, UpdateFeed, UpdateInfo, UpdateStrategy, Updater,
};

/// `secrecy::SecretString`, re-exported so that no consumer has to name the
/// `secrecy` version we settled on (08 §5.3). Zeroizes on drop; its `Debug`
/// prints `SecretBox<str>([REDACTED])`, never the value.
pub use secrecy::{ExposeSecret, SecretBox, SecretString};

use serde::{Deserialize, Serialize};

/// Which OS this build is adapting. Surfaced to the frontend so that
/// `platform`-keyed `when` clauses in the keymap (06 §12.1) and the
/// Settings/Preferences split (08 §5.4) are decided in Rust rather than sniffed
/// from a user agent string.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum PlatformId {
    /// Apple platforms. `Mod` renders as `⌘`; menus are a global menu bar.
    MacOs,
    /// Windows 10 1809 and later. `Mod` renders as `Ctrl`; menus are per-window.
    Windows,
    /// Linux (any desktop). `Mod` renders as `Ctrl`; menus are per-window.
    Linux,
}

impl PlatformId {
    /// The platform this crate was compiled for.
    ///
    /// This is the ONE `cfg(target_os)` in the trait crate, and it selects a
    /// value rather than a behaviour: everything that varies takes a
    /// `PlatformId` argument so it can be exercised for all three from any one
    /// of them. `menus::tests` asserts the Windows and Linux accelerator
    /// rendering from a macOS test run for precisely that reason.
    pub const HOST: Self = {
        #[cfg(target_os = "macos")]
        {
            Self::MacOs
        }
        #[cfg(target_os = "windows")]
        {
            Self::Windows
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            Self::Linux
        }
    };

    /// Stable lowercase identifier: `"macos"`, `"windows"`, `"linux"`. Matches
    /// the `platform` context key the keymap's `when` expressions test.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MacOs => "macos",
            Self::Windows => "windows",
            Self::Linux => "linux",
        }
    }
}

/// Every fallible platform call returns this.
///
/// Transcribed from 08 §5.1. The first two variants are the load-bearing ones:
/// see the crate docs for why they are answers rather than failures.
#[derive(Debug, thiserror::Error)]
pub enum PlatformError {
    /// The user dismissed a dialog, panel or permission prompt. Distinct from
    /// an empty selection.
    #[error("cancelled by user")]
    Cancelled,
    /// The capability does not exist on this OS, in this packaging, or in this
    /// session. `&'static str` on purpose: the reason is a property of the
    /// build, so it never needs to be formatted at runtime.
    #[error("not supported on this platform: {0}")]
    Unsupported(&'static str),
    /// The OS refused. Elevation, a sandbox, a locked keychain, a read-only
    /// `/usr/local/bin`.
    #[error("permission denied: {0}")]
    PermissionDenied(String),
    /// The backend exists but is not answering — no Secret Service on the bus,
    /// no notification daemon, no update feed wired up yet.
    #[error("backend unavailable: {0}")]
    BackendUnavailable(String),
    /// A raw OS status code we could not classify. `i64` holds an `OSStatus`,
    /// an `HRESULT` and an `errno` without truncation.
    #[error("os error {code}: {message}")]
    Os {
        /// The raw status: `OSStatus` on macOS, `HRESULT`/`GetLastError` on
        /// Windows, `errno` on Linux.
        code: i64,
        /// The OS-provided description, already localised by the OS.
        message: String,
    },
    /// Filesystem and pipe failures, unchanged.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl PlatformError {
    /// True for [`PlatformError::Cancelled`]. Callers branch on this constantly
    /// — a cancelled Save is a no-op, not a message in the status bar.
    #[must_use]
    pub const fn is_cancelled(&self) -> bool {
        matches!(self, Self::Cancelled)
    }

    /// True for [`PlatformError::Unsupported`]. A UI that hides an affordance
    /// rather than showing a broken one asks this.
    #[must_use]
    pub const fn is_unsupported(&self) -> bool {
        matches!(self, Self::Unsupported(_))
    }
}

/// Shorthand used throughout the layer.
pub type Result<T> = std::result::Result<T, PlatformError>;

/// The aggregate every consumer is handed (08 §5.1, CONTRACTS §13).
///
/// Borrowed accessors, not owned clones: the host is a process-lifetime
/// singleton, so there is exactly one of each adapter and no reason to hand out
/// copies of it.
pub trait Platform: Send + Sync + 'static {
    /// Which OS this is. See [`PlatformId::HOST`].
    fn id(&self) -> PlatformId;
    /// Resolved directories. A struct, not a trait — see [`paths`].
    fn paths(&self) -> &Paths;
    /// Keychain / Credential Manager / Secret Service / encrypted fallback.
    fn credentials(&self) -> &dyn CredentialStore;
    /// Open, Save and folder panels, plus Reveal and Open-in-browser.
    fn dialogs(&self) -> &dyn FileDialogs;
    /// Menu bar placement, system items and accelerator resolution.
    fn menus(&self) -> &dyn MenuHost;
    /// Update strategy, check, stage, apply.
    fn updater(&self) -> &dyn Updater;
    /// CLI shim, file associations, login-shell environment.
    fn shell(&self) -> &dyn ShellIntegration;
    /// Supervised child processes and CPU topology.
    fn processes(&self) -> &dyn ProcessHost;
    /// User notifications and the dock/taskbar badge.
    fn notifier(&self) -> &dyn Notifier;
}
