//! Windows updates — 08 §5.7, §7.4.
//!
//! [`UpdateStrategy::WindowsInstaller`](stratum_platform::UpdateStrategy::WindowsInstaller) when this build was installed, and
//! [`UpdateStrategy::Disabled`](stratum_platform::UpdateStrategy::Disabled) when it was not — a `cargo run` binary or a
//! portable unzip has no installer to re-run, and offering an update button
//! that cannot work is worse than not offering one. "Was it installed?" is
//! answered by `crate::aumid::is_registered`: the Start-menu shortcut is
//! something only the installer creates, and it is already load-bearing for
//! notifications, so the two capabilities agree about what an installation is
//! rather than each guessing separately.
//!
//! The download lives behind [`UpdateFeed`](stratum_platform::UpdateFeed), injected by the desktop: this
//! crate may not link a network crate (`deny.toml` restricts `reqwest` to
//! `stratum-ai`, `stratum-cli` and `stratum-desktop`).
//!
//! # Why the installer relaunches us and we do not relaunch ourselves
//!
//! macOS swaps a directory and calls `open -n`. Windows cannot: the running
//! `Stratum.exe` is **held open by the loader**, so the installer cannot
//! replace it while we are alive. The sequence is therefore spawn-then-exit —
//! we start the installer detached and leave immediately, and the installer
//! restarts the application when it is done. That is not a shortcut; it is the
//! only order that works, and it is why [`Updater::apply_and_relaunch`](stratum_platform::Updater::apply_and_relaunch) returns
//! `Infallible` by never returning rather than by relaunching.

use camino::{Utf8Path, Utf8PathBuf};
use stratum_platform::{PlatformError, Result};

/// How to run a staged Windows installer.
///
/// Pure, and separated from the spawn, because the argument vector is the part
/// that is easy to get wrong and impossible to test on a machine that cannot
/// boot Windows. A missing `/qb` turns an update into a silent hang behind an
/// invisible dialog; a missing `/norestart` lets an MSI reboot a researcher's
/// machine mid-analysis.
///
/// # Errors
/// [`PlatformError::Unsupported`] for an artifact that is neither an `.msi` nor
/// an installer `.exe`. Handing an arbitrary staged file to `CreateProcess` is
/// exactly the hole a signature check exists to close, so the extension is
/// checked even though the signature already was.
pub fn installer_command(artifact: &Utf8Path) -> Result<(Utf8PathBuf, Vec<String>)> {
    match artifact.extension().map(str::to_ascii_lowercase).as_deref() {
        Some("msi") => Ok((
            // Bare name, resolved through `System32`: an absolute path would
            // be wrong on a machine with a non-`C:` system drive.
            Utf8PathBuf::from("msiexec.exe"),
            vec![
                "/i".to_owned(),
                artifact.as_str().to_owned(),
                // A basic UI with a progress bar and no prompts. `/qn` would be
                // completely silent, and a researcher whose IDE vanished with
                // no explanation is a support ticket.
                "/qb".to_owned(),
                // Never reboot. An analysis is running in another window.
                "/norestart".to_owned(),
            ],
        )),
        // Tauri's Windows bundle is NSIS. `/S` is its silent switch; the
        // installer's own finish page is what relaunches us.
        Some("exe") => Ok((artifact.to_path_buf(), vec!["/S".to_owned()])),
        _ => Err(PlatformError::Unsupported(
            "the staged update is neither an .msi nor an installer .exe",
        )),
    }
}

/// The §7.4 refusal, named once so `apply_and_relaunch` and its test agree.
///
/// # Errors
/// Always. It exists to produce the one error.
#[must_use]
pub fn unverified() -> PlatformError {
    PlatformError::PermissionDenied(
        "the update's minisign signature was not verified; refusing to install".to_owned(),
    )
}

#[cfg(target_os = "windows")]
pub use sys::WindowsUpdater;

/// `DETACHED_PROCESS`. The installer must outlive us — we are about to exit —
/// and must not inherit our console.
pub const DETACHED_PROCESS: u32 = 0x0000_0008;

#[cfg(target_os = "windows")]
mod sys {
    use std::convert::Infallible;
    use std::sync::Arc;

    use camino::Utf8PathBuf;
    use stratum_platform::{
        Channel, PlatformError, ProgressFn, Result, StagedUpdate, UpdateFeed, UpdateInfo,
        UpdateStrategy, Updater,
    };

    use super::{installer_command, unverified, DETACHED_PROCESS};

    /// [`Updater`] for Windows.
    pub struct WindowsUpdater {
        feed: Option<Arc<dyn UpdateFeed>>,
        current_version: String,
        staging_dir: Utf8PathBuf,
        installed: bool,
    }

    impl std::fmt::Debug for WindowsUpdater {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("WindowsUpdater")
                .field("has_feed", &self.feed.is_some())
                .field("current_version", &self.current_version)
                .field("installed", &self.installed)
                .finish()
        }
    }

    impl WindowsUpdater {
        /// Construct. `staging_dir` should be under
        /// [`stratum_platform::Paths::cache_dir`]: a half-downloaded update is
        /// regenerable by definition.
        ///
        /// An empty `current_version` becomes this build's own version. A feed
        /// asked "what is newer than ``?" answers "everything", and the
        /// resulting update loop is invisible until a user is stuck in it.
        #[must_use]
        pub fn new(
            current_version: impl Into<String>,
            staging_dir: impl Into<Utf8PathBuf>,
        ) -> Self {
            let current_version = current_version.into();
            Self {
                feed: None,
                current_version: if current_version.is_empty() {
                    env!("CARGO_PKG_VERSION").to_owned()
                } else {
                    current_version
                },
                staging_dir: staging_dir.into(),
                installed: crate::aumid::is_registered(),
            }
        }

        /// Attach the network half.
        #[must_use]
        pub fn with_feed(mut self, feed: Arc<dyn UpdateFeed>) -> Self {
            self.feed = Some(feed);
            self
        }

        /// The version this updater reports to the feed. Never empty.
        #[must_use]
        pub fn current_version(&self) -> &str {
            &self.current_version
        }

        fn feed(&self) -> Result<&dyn UpdateFeed> {
            self.feed.as_deref().ok_or_else(|| {
                PlatformError::BackendUnavailable("no update feed is configured".to_owned())
            })
        }

        const NOT_INSTALLED: PlatformError = PlatformError::Unsupported(
            "this build was not installed by the Stratum installer, so there is no installation \
             to update",
        );
    }

    #[async_trait::async_trait]
    impl Updater for WindowsUpdater {
        fn strategy(&self) -> UpdateStrategy {
            if self.installed {
                UpdateStrategy::WindowsInstaller
            } else {
                UpdateStrategy::Disabled
            }
        }

        async fn check(&self, channel: Channel) -> Result<Option<UpdateInfo>> {
            if !self.installed {
                return Err(Self::NOT_INSTALLED);
            }
            self.feed()?.latest(channel, &self.current_version).await
        }

        async fn stage(&self, info: &UpdateInfo, on_progress: ProgressFn) -> Result<StagedUpdate> {
            if !self.installed {
                return Err(Self::NOT_INSTALLED);
            }
            std::fs::create_dir_all(&self.staging_dir)?;
            self.feed()?
                .download(info, &self.staging_dir, on_progress)
                .await
        }

        fn apply_and_relaunch(&self, staged: StagedUpdate) -> Result<Infallible> {
            // §7.4, enforced here — at the single point that can act on it —
            // rather than in the feed that produced the artifact: a feed that
            // forgot to verify still cannot install anything.
            if !staged.is_verified() {
                return Err(unverified());
            }
            if !self.installed {
                return Err(Self::NOT_INSTALLED);
            }

            let (program, args) = installer_command(staged.path())?;
            use std::os::windows::process::CommandExt as _;
            std::process::Command::new(program.as_std_path())
                .args(&args)
                // Detached, because we are about to exit and the installer
                // cannot replace `Stratum.exe` while the loader holds it open.
                .creation_flags(DETACHED_PROCESS)
                .spawn()?;
            // Not `abort`: a normal exit closes our stdio pipes, which is how
            // a supervised engine learns the supervisor is gone, and runs the
            // handlers that flush the log.
            std::process::exit(0)
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    /// `/qb` and `/norestart` are not decoration. Without the first the
    /// installer waits on a dialog nobody can see; without the second an MSI
    /// may reboot the machine while a two-hour regression is running.
    #[test]
    fn an_msi_is_run_through_msiexec_quietly_and_without_a_reboot() {
        let (prog, args) = installer_command(Utf8Path::new(r"C:\c\Stratum-0.4.2.msi")).unwrap();
        assert_eq!(prog, "msiexec.exe");
        assert_eq!(args[0], "/i");
        assert_eq!(args[1], r"C:\c\Stratum-0.4.2.msi");
        assert!(args.contains(&"/qb".to_owned()));
        assert!(args.contains(&"/norestart".to_owned()));
    }

    #[test]
    fn an_nsis_installer_runs_itself_silently() {
        let (prog, args) = installer_command(Utf8Path::new(r"C:\c\Stratum-setup.exe")).unwrap();
        assert_eq!(prog, r"C:\c\Stratum-setup.exe");
        assert_eq!(args, ["/S"]);
    }

    #[test]
    fn the_extension_is_matched_case_insensitively() {
        assert!(installer_command(Utf8Path::new(r"C:\c\S.MSI")).is_ok());
        assert!(installer_command(Utf8Path::new(r"C:\c\S.Exe")).is_ok());
    }

    /// A verified signature says the bytes are ours; it does not say the file
    /// is an installer. Handing an arbitrary staged artifact to `CreateProcess`
    /// is the hole the signature check exists to close, so both gates run.
    #[test]
    fn anything_that_is_not_an_installer_is_refused() {
        for bad in [
            r"C:\c\Stratum.app.tar.gz",
            r"C:\c\Stratum.AppImage",
            r"C:\c\payload.bat",
            r"C:\c\noextension",
        ] {
            let err = installer_command(Utf8Path::new(bad)).unwrap_err();
            assert!(err.is_unsupported(), "{bad}");
        }
    }

    #[test]
    fn the_signature_refusal_names_minisign_so_the_ui_can_explain_it() {
        let e = unverified();
        assert!(
            matches!(e, PlatformError::PermissionDenied(ref m) if m.contains("minisign")),
            "{e}"
        );
    }
}
