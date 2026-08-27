//! Linux implementation of the Stratum platform adapter layer — 08 §5.
//!
//! One `LinuxPlatform` per process, obtained through `stratum-platform-host`.
//! Everything OS-specific in Stratum is reachable from here and from nowhere
//! else: `deny.toml` names this crate as the only permitted wrapper for `zbus`
//! and `secret-service`, which makes 08 §5.0 rule 2 a machine check rather than
//! a review comment.
//!
//! # Absence is the common case here, and every one of them is an answer
//!
//! Linux is the platform where "the capability is simply not present" is
//! ordinary rather than exotic, and W24's acceptance makes one of them
//! explicit: **Secret Service absence is a first-class expected state, never a
//! crash.** A plain i3 or sway session, a server, an SSH-forwarded session and
//! a container each run without `org.freedesktop.secrets`, often without
//! `org.freedesktop.Notifications`, and often without an `xdg-desktop-portal`.
//! Each is answered rather than reported as a failure:
//!
//! | absent | answer |
//! |---|---|
//! | Secret Service | demote **once** to [`secretfile::EncryptedFileStore`]; `backend()` then says `EncryptedFile`, so §22's privacy statement stays true |
//! | FileChooser portal | the injected GTK fallback ([`dialogs::GtkFallback`]), then `Unsupported` naming both halves |
//! | notification daemon | `Unsupported`; `capabilities()` still reports the launcher badge, which is a broadcast signal and does not go through the daemon |
//! | any session bus at all | the same, with `BackendUnavailable` naming the missing bus, probed once per process |
//!
//! `Packaging` is the other half of that: [`packaging::RUNTIME_DEPENDENCIES`]
//! records which of these backends may be a hard package dependency and which
//! may not, for W22's `.deb`/`.rpm` metadata to transcribe.
//!
//! # Why this crate is not `#![cfg(target_os = "linux")]` as a whole
//!
//! Its sibling `stratum-platform-macos` is, and can afford to be: its CI job
//! and its maintainer's machine both run macOS, so a gated crate is still an
//! exercised crate. This one was written on a machine that runs no Linux at
//! all, where a fully gated crate compiles to an empty library and proves none
//! of its acceptance bullets locally. So every module that is *policy* rather
//! than syscall — [`packaging`], [`menus`], [`procfs`], [`secretfile`],
//! [`mime`], [`uri`], the demotion in [`credentials`], the portal/GTK decision
//! in [`dialogs`], and the XDG resolution in [`shell`] — compiles and is tested
//! on every host, while the modules that issue a syscall or a D-Bus call are
//! `cfg(target_os = "linux")` and are covered by
//! `cargo check --target x86_64-unknown-linux-gnu`.

pub mod credentials;
pub mod dialogs;
pub mod menus;
pub mod mime;
pub mod packaging;
pub mod procfs;
pub mod secretfile;
pub mod shell;
pub mod updater;
pub mod uri;

#[cfg(target_os = "linux")]
pub mod bus;
#[cfg(target_os = "linux")]
pub mod notify;
#[cfg(target_os = "linux")]
pub mod portal;
#[cfg(target_os = "linux")]
pub mod process;
#[cfg(target_os = "linux")]
pub mod secretservice;

#[cfg(target_os = "linux")]
mod platform;

pub use credentials::{LinuxCredentials, SecretStore};
pub use dialogs::{DesktopPortal, DialogAttempts, GtkFallback, LinuxFileDialogs, NoPortal};
pub use menus::LinuxMenuHost;
pub use packaging::{Packaging, RuntimeDependency, RUNTIME_DEPENDENCIES};
pub use secretfile::EncryptedFileStore;
pub use shell::{LinuxShell, XdgDirs};
pub use updater::LinuxUpdater;

#[cfg(target_os = "linux")]
pub use notify::LinuxNotifier;
#[cfg(target_os = "linux")]
pub use platform::{LinuxConfig, LinuxPlatform};
#[cfg(target_os = "linux")]
pub use portal::XdgPortal;
#[cfg(target_os = "linux")]
pub use process::LinuxProcessHost;
#[cfg(target_os = "linux")]
pub use secretservice::SecretServiceClient;
