//! Updates — 08 §5.7 and §7.4.
//!
//! # `PackageManaged` is a real answer
//!
//! Silently self-updating a distro-managed install desynchronises the package
//! database and is user-hostile. On a `.deb`/`.rpm` the UI shows "Update
//! available (0.4.2) — install with `apt upgrade stratum`" and does nothing
//! else. [`UpdateStrategy::Disabled`] is the same for an enterprise policy or a
//! `--no-update` build.
//!
//! # Why the network is not in this layer
//!
//! Every strategy verifies a **minisign signature** over the downloaded
//! artifact before staging it, independently of OS code signing (§7.4). But
//! `deny.toml` restricts `reqwest` to `stratum-ai`, `stratum-cli` and
//! `stratum-desktop` — the platform impl crates may not open a socket, and that
//! ban is deliberate rather than incidental. So the download-and-verify half is
//! [`UpdateFeed`], injected by whoever *is* allowed to fetch, and the platform
//! impl owns only the part that is genuinely OS-specific: swapping a `.app`,
//! running an installer, rewriting an AppImage in place. A [`StagedUpdate`]
//! whose signature did not verify is refused by
//! [`Updater::apply_and_relaunch`], which is where that invariant is enforced
//! rather than merely documented.

use std::convert::Infallible;

use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};

use crate::Result;

/// Release channel.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    /// The default.
    #[default]
    Stable,
    /// Pre-release.
    Beta,
    /// Every green build on `main`.
    Nightly,
}

/// How this packaging updates itself.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum UpdateStrategy {
    /// macOS: replace `Stratum.app` and relaunch.
    AppBundleSwap,
    /// Windows: run the NSIS/MSI installer, which may need elevation.
    WindowsInstaller,
    /// AppImage: rewrite our own file in place.
    AppImageSelfReplace,
    /// `.deb`/`.rpm`: **do nothing**. Tell the user to use their package
    /// manager.
    PackageManaged,
    /// Enterprise policy, or a build compiled without an updater.
    Disabled,
}

impl UpdateStrategy {
    /// Whether this build is able to install an update itself. False for
    /// [`UpdateStrategy::PackageManaged`] and [`UpdateStrategy::Disabled`],
    /// which is what the "Install" button's disabled state is derived from.
    #[must_use]
    pub const fn can_self_install(self) -> bool {
        matches!(
            self,
            Self::AppBundleSwap | Self::WindowsInstaller | Self::AppImageSelfReplace
        )
    }
}

/// A release the feed offered us.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct UpdateInfo {
    /// Semver of the offered release.
    pub version: String,
    /// Release notes, Markdown.
    pub notes: String,
    /// Publication time. Milliseconds since the Unix epoch, like every other
    /// timestamp in this codebase (A2) — never a formatted string.
    pub published_ms: u64,
    /// Where the artifact is. Not opened by this crate.
    pub artifact_url: String,
    /// The detached minisign signature, base64, as Tauri's updater emits it.
    pub signature: String,
    /// Artifact size, when the feed states one.
    pub size: Option<u64>,
}

/// Download progress.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Progress {
    /// Bytes so far.
    pub downloaded: u64,
    /// Total, when the server sent a length.
    pub total: Option<u64>,
}

/// Progress callback. Boxed rather than generic because [`Updater`] is a `dyn`
/// trait.
pub type ProgressFn = Box<dyn Fn(Progress) + Send + Sync>;

/// A downloaded, signature-verified artifact on disk.
///
/// The only constructors are [`StagedUpdate::verified`] and
/// [`StagedUpdate::unverified`], so "did the minisign check pass?" is a
/// decision someone had to make explicitly rather than a field that defaults to
/// `true`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct StagedUpdate {
    path: Utf8PathBuf,
    info: UpdateInfo,
    signature_verified: bool,
}

impl StagedUpdate {
    /// The artifact's minisign signature was checked against the release public
    /// key and matched.
    #[must_use]
    pub fn verified(path: impl Into<Utf8PathBuf>, info: UpdateInfo) -> Self {
        Self {
            path: path.into(),
            info,
            signature_verified: true,
        }
    }

    /// The artifact is on disk but its signature has NOT been checked.
    /// [`Updater::apply_and_relaunch`] refuses it.
    #[must_use]
    pub fn unverified(path: impl Into<Utf8PathBuf>, info: UpdateInfo) -> Self {
        Self {
            path: path.into(),
            info,
            signature_verified: false,
        }
    }

    /// Where the artifact is.
    #[must_use]
    pub fn path(&self) -> &Utf8Path {
        &self.path
    }

    /// Which release it is.
    #[must_use]
    pub const fn info(&self) -> &UpdateInfo {
        &self.info
    }

    /// Whether §7.4's minisign check passed.
    #[must_use]
    pub const fn is_verified(&self) -> bool {
        self.signature_verified
    }
}

/// The network half, injected from a layer that is allowed to open a socket.
///
/// Implemented by `stratum-desktop`; see the module docs.
#[async_trait::async_trait]
pub trait UpdateFeed: Send + Sync {
    /// Ask the feed what is newer than `current_version` on `channel`.
    ///
    /// # Errors
    /// [`crate::PlatformError::BackendUnavailable`] when the feed is
    /// unreachable — offline is not an error the user should see a dialog for.
    async fn latest(&self, channel: Channel, current_version: &str) -> Result<Option<UpdateInfo>>;

    /// Download into `dir` and verify the minisign signature. MUST return
    /// [`StagedUpdate::unverified`] rather than an error if it chooses not to
    /// verify, so the refusal happens at the single point that enforces it.
    ///
    /// # Errors
    /// [`crate::PlatformError::BackendUnavailable`] on a transport failure,
    /// [`crate::PlatformError::Io`] on a write failure.
    async fn download(
        &self,
        info: &UpdateInfo,
        dir: &Utf8Path,
        on_progress: ProgressFn,
    ) -> Result<StagedUpdate>;
}

/// Check, stage, apply.
#[async_trait::async_trait]
pub trait Updater: Send + Sync {
    /// How this packaging updates. Never fails: it is a property of the build.
    fn strategy(&self) -> UpdateStrategy;

    /// Is there something newer?
    ///
    /// # Errors
    /// [`crate::PlatformError::Unsupported`] for [`UpdateStrategy::Disabled`],
    /// [`crate::PlatformError::BackendUnavailable`] with no feed wired up or an
    /// unreachable one.
    async fn check(&self, channel: Channel) -> Result<Option<UpdateInfo>>;

    /// Download and verify.
    ///
    /// # Errors
    /// As [`Updater::check`], plus [`crate::PlatformError::Io`].
    async fn stage(&self, info: &UpdateInfo, on_progress: ProgressFn) -> Result<StagedUpdate>;

    /// Install and restart. On success the process does not return — hence
    /// [`Infallible`].
    ///
    /// # Errors
    /// [`crate::PlatformError::PermissionDenied`] for an unverified
    /// [`StagedUpdate`] or an install location we cannot write;
    /// [`crate::PlatformError::Unsupported`] for a strategy that cannot
    /// self-install.
    fn apply_and_relaunch(&self, staged: StagedUpdate) -> Result<Infallible>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info() -> UpdateInfo {
        UpdateInfo {
            version: "0.4.2".into(),
            notes: String::new(),
            published_ms: 0,
            artifact_url: "https://example.invalid/Stratum.app.tar.gz".into(),
            signature: String::new(),
            size: None,
        }
    }

    #[test]
    fn package_managed_and_disabled_cannot_self_install() {
        assert!(UpdateStrategy::AppBundleSwap.can_self_install());
        assert!(!UpdateStrategy::PackageManaged.can_self_install());
        assert!(!UpdateStrategy::Disabled.can_self_install());
    }

    #[test]
    fn verification_is_an_explicit_decision() {
        assert!(StagedUpdate::verified("/tmp/a", info()).is_verified());
        assert!(!StagedUpdate::unverified("/tmp/a", info()).is_verified());
    }
}
