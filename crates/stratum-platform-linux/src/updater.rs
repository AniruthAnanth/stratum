//! Updates on Linux — 08 §5.7, §7.4.
//!
//! Exactly one Linux shape can install an update itself: the AppImage, by
//! rewriting its own file. Everything the distributions ship —`.deb`, `.rpm`,
//! Flatpak, Snap, Nix — is [`UpdateStrategy::PackageManaged`], where the UI
//! shows "Update available (0.4.2) — install with `apt upgrade stratum`" and
//! **does nothing else**. Silently self-updating a distro-managed install
//! desynchronises the package database: the next `apt upgrade` reinstalls the
//! version the archive has, the user's update is silently undone, and `dpkg
//! --verify` reports a tampered package. See [`crate::packaging`] for how the
//! shape is decided and for the per-manager command string.
//!
//! The download lives behind [`UpdateFeed`], injected by the desktop: this
//! crate may not link a network crate (`deny.toml` restricts `reqwest` to
//! `stratum-ai`, `stratum-cli` and `stratum-desktop`).
//!
//! # Replacing a running executable
//!
//! `open(O_WRONLY)` on a running binary is `ETXTBSY`, so an AppImage cannot
//! overwrite itself in place — but `rename(2)` over it is fine: the old inode
//! stays alive for the running process while the directory entry moves. So the
//! sequence is write-beside, `fsync`, `chmod`, `rename`, relaunch. The `fsync`
//! is not ceremony: without it a power loss between the rename and the
//! writeback leaves the user with a zero-length file where their application
//! was, and an AppImage has no package manager to repair it.

use std::convert::Infallible;
use std::sync::Arc;

use camino::{Utf8Path, Utf8PathBuf};
use stratum_platform::{
    Channel, PlatformError, ProgressFn, Result, StagedUpdate, UpdateFeed, UpdateInfo,
    UpdateStrategy, Updater,
};

use crate::packaging::Packaging;

/// [`Updater`] for Linux.
pub struct LinuxUpdater {
    feed: Option<Arc<dyn UpdateFeed>>,
    current_version: String,
    staging_dir: Utf8PathBuf,
    packaging: Packaging,
}

impl std::fmt::Debug for LinuxUpdater {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LinuxUpdater")
            .field("has_feed", &self.feed.is_some())
            .field("current_version", &self.current_version)
            .field("packaging", &self.packaging)
            .finish()
    }
}

impl LinuxUpdater {
    /// Construct. `staging_dir` should be under
    /// [`stratum_platform::Paths::cache_dir`]: a half-downloaded update is
    /// regenerable by definition.
    ///
    /// An empty `current_version` becomes this build's own version. A feed
    /// asked "what is newer than ``?" answers "everything", and the resulting
    /// update loop is invisible until a user is stuck in it.
    #[must_use]
    pub fn new(
        current_version: impl Into<String>,
        staging_dir: impl Into<Utf8PathBuf>,
        packaging: Packaging,
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
            packaging,
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

    /// What the UI shows beside "Update available" for a package-managed
    /// install. `None` when we install it ourselves or cannot.
    #[must_use]
    pub const fn upgrade_hint(&self) -> Option<&'static str> {
        self.packaging.upgrade_hint()
    }

    fn feed(&self) -> Result<&dyn UpdateFeed> {
        self.feed.as_deref().ok_or_else(|| {
            PlatformError::BackendUnavailable("no update feed is configured".to_owned())
        })
    }

    /// Package-managed and disabled builds do not check either: an update the
    /// user cannot install is a notification with no action behind it.
    fn refuse_unless_self_installing(&self) -> Result<()> {
        match self.strategy() {
            UpdateStrategy::AppImageSelfReplace => Ok(()),
            UpdateStrategy::PackageManaged => Err(PlatformError::Unsupported(
                "this install is managed by a package manager; updates are its job",
            )),
            _ => Err(PlatformError::Unsupported(
                "this build has nothing to update: it was not installed from a package \
                 or an AppImage",
            )),
        }
    }
}

#[async_trait::async_trait]
impl Updater for LinuxUpdater {
    fn strategy(&self) -> UpdateStrategy {
        self.packaging.update_strategy()
    }

    async fn check(&self, channel: Channel) -> Result<Option<UpdateInfo>> {
        self.refuse_unless_self_installing()?;
        self.feed()?.latest(channel, &self.current_version).await
    }

    async fn stage(&self, info: &UpdateInfo, on_progress: ProgressFn) -> Result<StagedUpdate> {
        self.refuse_unless_self_installing()?;
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
        let Packaging::AppImage(installed) = &self.packaging else {
            self.refuse_unless_self_installing()?;
            return Err(PlatformError::Unsupported(
                "only an AppImage install can replace itself",
            ));
        };

        let new_image = unpack_appimage(staged.path(), &self.staging_dir)?;
        replace_in_place(&new_image, installed)?;
        relaunch(installed)
    }
}

/// Find the `.AppImage` inside a staged artifact.
///
/// Tauri's updater publishes `*.AppImage.tar.gz` for this target, but a feed
/// may equally hand us the bare image. Both are accepted; anything else is
/// refused by name rather than executed hopefully.
fn unpack_appimage(archive: &Utf8Path, into: &Utf8Path) -> Result<Utf8PathBuf> {
    if archive.extension() == Some("AppImage") {
        return Ok(archive.to_owned());
    }
    if !archive.as_str().ends_with(".tar.gz") {
        return Err(PlatformError::Unsupported(
            "the staged artifact is neither an .AppImage nor a .tar.gz",
        ));
    }

    let dest = into.join("unpacked");
    if dest.exists() {
        std::fs::remove_dir_all(&dest)?;
    }
    std::fs::create_dir_all(&dest)?;
    // `tar` rather than a Rust tar crate: it is present on every Linux in
    // 08 §6.2's tested set, and it preserves the executable bit, which is the
    // one attribute an AppImage cannot lose.
    let status = std::process::Command::new("tar")
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
    std::fs::read_dir(&dest)?
        .filter_map(std::result::Result::ok)
        .map(|e| e.path())
        .filter_map(|p| Utf8PathBuf::from_path_buf(p).ok())
        .find(|p| p.extension() == Some("AppImage"))
        .ok_or(PlatformError::Unsupported(
            "the staged archive contains no .AppImage",
        ))
}

/// Write `new_image` over `installed`. See the module docs for why this is
/// write-beside-then-rename rather than an overwrite.
fn replace_in_place(new_image: &Utf8Path, installed: &Utf8Path) -> Result<()> {
    use std::io::Write;

    let parent = installed.parent().ok_or(PlatformError::Unsupported(
        "the installed AppImage has no parent directory",
    ))?;
    let name = installed.file_name().unwrap_or("Stratum.AppImage");
    // In the SAME directory, or the rename below is a cross-device link error
    // — and the staging directory is under the cache dir, which is routinely
    // on a different filesystem from `~/Applications`.
    let landing = parent.join(format!(".{name}.incoming"));

    let bytes = std::fs::read(new_image).map_err(|e| perm(e, new_image))?;
    let mut f = std::fs::File::create(&landing).map_err(|e| perm(e, parent))?;
    f.write_all(&bytes).map_err(|e| perm(e, &landing))?;
    // Not ceremony. Without it, a power loss between the rename and the
    // writeback leaves a zero-length file where the user's application was,
    // and an AppImage has no package manager to repair it.
    f.sync_all().map_err(|e| perm(e, &landing))?;
    drop(f);
    set_executable(&landing)?;

    if let Err(e) = std::fs::rename(&landing, installed) {
        let _ = std::fs::remove_file(&landing);
        return Err(perm(e, parent));
    }
    Ok(())
}

#[cfg(unix)]
fn set_executable(path: &Utf8Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .map_err(|e| perm(e, path))
}

#[cfg(not(unix))]
fn set_executable(_path: &Utf8Path) -> Result<()> {
    Err(PlatformError::Unsupported(
        "file modes are a Unix concept; this build cannot install an AppImage",
    ))
}

fn perm(e: std::io::Error, at: &Utf8Path) -> PlatformError {
    match e.kind() {
        std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::ReadOnlyFilesystem => {
            PlatformError::PermissionDenied(format!("{at}: {e}"))
        }
        _ => PlatformError::Io(e),
    }
}

/// Start the new image and leave. Returning [`Infallible`] means the only way
/// out of this function is an error or process death.
fn relaunch(image: &Utf8Path) -> Result<Infallible> {
    std::process::Command::new(image.as_str()).spawn()?;
    // Not `abort`: the supervisor's children have to see our pipes close, and a
    // normal exit runs the atexit handlers that flush the log.
    std::process::exit(0)
}
