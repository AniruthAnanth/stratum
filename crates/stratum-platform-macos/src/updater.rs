//! macOS updates — 08 §5.7, §7.4.
//!
//! [`UpdateStrategy::AppBundleSwap`] when we are running from a `.app`, and
//! [`UpdateStrategy::Disabled`] when we are not — a `cargo run` binary has no
//! bundle to replace, and offering an update button that cannot work is worse
//! than not offering one.
//!
//! The download lives behind [`UpdateFeed`], injected by the desktop: this
//! crate may not link a network crate (`deny.toml` restricts `reqwest` to
//! `stratum-ai`, `stratum-cli` and `stratum-desktop`). What is genuinely
//! macOS-specific, and is here, is the swap: unpack beside the installed
//! bundle, rename the old one out of the way, rename the new one in, roll back
//! if the second rename fails, then relaunch with `open -n` and exit.

use std::convert::Infallible;
use std::sync::Arc;

use camino::{Utf8Path, Utf8PathBuf};
use stratum_platform::{
    Channel, PlatformError, ProgressFn, Result, StagedUpdate, UpdateFeed, UpdateInfo,
    UpdateStrategy, Updater,
};

/// [`Updater`] for macOS.
pub struct MacosUpdater {
    feed: Option<Arc<dyn UpdateFeed>>,
    current_version: String,
    staging_dir: Utf8PathBuf,
    app_bundle: Option<Utf8PathBuf>,
}

impl std::fmt::Debug for MacosUpdater {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MacosUpdater")
            .field("has_feed", &self.feed.is_some())
            .field("current_version", &self.current_version)
            .field("app_bundle", &self.app_bundle)
            .finish()
    }
}

impl MacosUpdater {
    /// Construct. `staging_dir` should be under
    /// [`stratum_platform::Paths::cache_dir`]: a half-downloaded update is
    /// regenerable by definition.
    ///
    /// An empty `current_version` becomes this build's own version. A feed
    /// asked "what is newer than ``?" answers "everything", and the resulting
    /// update loop is invisible until a user is stuck in it.
    #[must_use]
    pub fn new(current_version: impl Into<String>, staging_dir: impl Into<Utf8PathBuf>) -> Self {
        let current_version = current_version.into();
        Self {
            feed: None,
            current_version: if current_version.is_empty() {
                env!("CARGO_PKG_VERSION").to_owned()
            } else {
                current_version
            },
            staging_dir: staging_dir.into(),
            app_bundle: crate::bundle::app_bundle_path(),
        }
    }

    /// Attach the network half.
    #[must_use]
    pub fn with_feed(mut self, feed: Arc<dyn UpdateFeed>) -> Self {
        self.feed = Some(feed);
        self
    }

    /// The version this updater reports to the feed. Never empty: an empty
    /// current version makes every release look newer.
    #[must_use]
    pub fn current_version(&self) -> &str {
        &self.current_version
    }

    fn feed(&self) -> Result<&dyn UpdateFeed> {
        self.feed.as_deref().ok_or_else(|| {
            PlatformError::BackendUnavailable("no update feed is configured".to_owned())
        })
    }
}

#[async_trait::async_trait]
impl Updater for MacosUpdater {
    fn strategy(&self) -> UpdateStrategy {
        if self.app_bundle.is_some() {
            UpdateStrategy::AppBundleSwap
        } else {
            UpdateStrategy::Disabled
        }
    }

    async fn check(&self, channel: Channel) -> Result<Option<UpdateInfo>> {
        if self.strategy() == UpdateStrategy::Disabled {
            return Err(PlatformError::Unsupported(
                "this build has no app bundle to update",
            ));
        }
        self.feed()?.latest(channel, &self.current_version).await
    }

    async fn stage(&self, info: &UpdateInfo, on_progress: ProgressFn) -> Result<StagedUpdate> {
        if self.strategy() == UpdateStrategy::Disabled {
            return Err(PlatformError::Unsupported(
                "this build has no app bundle to update",
            ));
        }
        std::fs::create_dir_all(&self.staging_dir)?;
        self.feed()?
            .download(info, &self.staging_dir, on_progress)
            .await
    }

    fn apply_and_relaunch(&self, staged: StagedUpdate) -> Result<Infallible> {
        // §7.4. The check is here, at the single point that can act on it,
        // rather than in the feed that produced the artifact: a feed that
        // forgot to verify still cannot install anything.
        if !staged.is_verified() {
            return Err(PlatformError::PermissionDenied(
                "the update's minisign signature was not verified; refusing to install".to_owned(),
            ));
        }
        let Some(installed) = self.app_bundle.clone() else {
            return Err(PlatformError::Unsupported(
                "this build has no app bundle to update",
            ));
        };

        let unpacked = unpack(staged.path(), &self.staging_dir)?;
        swap_bundle(&unpacked, &installed)?;
        relaunch(&installed)
    }
}

/// Unpack `Stratum.app.tar.gz` and return the `.app` it contained.
///
/// `/usr/bin/tar` rather than a tar crate: it is present on every macOS, it
/// preserves the symlinks and the extended attributes a signed bundle depends
/// on, and a Rust tar implementation that drops one of those produces an app
/// that Gatekeeper then refuses with no explanation.
fn unpack(archive: &Utf8Path, into: &Utf8Path) -> Result<Utf8PathBuf> {
    let dest = into.join("unpacked");
    if dest.exists() {
        std::fs::remove_dir_all(&dest)?;
    }
    std::fs::create_dir_all(&dest)?;

    let status = std::process::Command::new("/usr/bin/tar")
        .arg("-xzf")
        .arg(archive.as_str())
        .arg("-C")
        .arg(dest.as_str())
        .status()?;
    if !status.success() {
        return Err(PlatformError::Os {
            code: i64::from(status.code().unwrap_or(-1)),
            message: format!("tar could not unpack {archive}"),
        });
    }

    let app = std::fs::read_dir(&dest)?
        .filter_map(std::result::Result::ok)
        .map(|e| e.path())
        .filter_map(|p| Utf8PathBuf::from_path_buf(p).ok())
        .find(|p| p.extension() == Some("app"))
        .ok_or(PlatformError::Unsupported(
            "the staged archive contains no .app bundle",
        ))?;
    if !app.join("Contents/MacOS").is_dir() {
        return Err(PlatformError::Unsupported(
            "the staged .app has no Contents/MacOS; it is not an application",
        ));
    }
    Ok(app)
}

/// Move `new_app` into `installed`'s place, keeping the old bundle until the
/// move has succeeded.
///
/// Two renames within one volume, which are atomic individually; the rollback
/// covers the window between them. `/Applications` needs admin rights, so
/// `EACCES` is [`PlatformError::PermissionDenied`] and the UI can offer to
/// authenticate rather than showing a filesystem error.
fn swap_bundle(new_app: &Utf8Path, installed: &Utf8Path) -> Result<()> {
    let parent = installed.parent().ok_or(PlatformError::Unsupported(
        "the installed app has no parent directory",
    ))?;
    let backup = parent.join(format!(
        ".{}.replaced",
        installed.file_name().unwrap_or("Stratum.app")
    ));
    if backup.exists() {
        std::fs::remove_dir_all(&backup)?;
    }

    // Onto the same volume first, or the rename below is a cross-device copy.
    let landing = parent.join(format!(
        ".{}.incoming",
        installed.file_name().unwrap_or("Stratum.app")
    ));
    if landing.exists() {
        std::fs::remove_dir_all(&landing)?;
    }
    copy_tree(new_app, &landing).map_err(|e| perm(e, parent))?;

    std::fs::rename(installed, &backup).map_err(|e| perm(e, parent))?;
    if let Err(e) = std::fs::rename(&landing, installed) {
        // Put the user's application back before reporting anything.
        let _ = std::fs::rename(&backup, installed);
        let _ = std::fs::remove_dir_all(&landing);
        return Err(perm(e, parent));
    }
    let _ = std::fs::remove_dir_all(&backup);
    Ok(())
}

/// `ditto` rather than a hand-rolled recursive copy: it is the only tool that
/// preserves resource forks, ACLs and extended attributes, and a bundle that
/// loses its `com.apple.provenance` xattr is a bundle Gatekeeper re-quarantines.
fn copy_tree(from: &Utf8Path, to: &Utf8Path) -> std::io::Result<()> {
    let status = std::process::Command::new("/usr/bin/ditto")
        .arg(from.as_str())
        .arg(to.as_str())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "ditto {from} -> {to} exited with {status}"
        )))
    }
}

fn perm(e: std::io::Error, at: &Utf8Path) -> PlatformError {
    match e.raw_os_error() {
        Some(libc::EACCES | libc::EPERM | libc::EROFS) => {
            PlatformError::PermissionDenied(format!("{at}: {e}"))
        }
        _ => PlatformError::Io(e),
    }
}

/// `open -n` a fresh instance and leave. Returning `Infallible` means the only
/// way out of this function is an error or process death.
fn relaunch(app: &Utf8Path) -> Result<Infallible> {
    std::process::Command::new("/usr/bin/open")
        .arg("-n")
        .arg(app.as_str())
        .spawn()?;
    // Not `abort`: the supervisor's children have to see our pipes close, and
    // a normal exit runs the atexit handlers that flush the log.
    std::process::exit(0)
}
