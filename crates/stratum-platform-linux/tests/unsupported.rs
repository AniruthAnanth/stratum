//! The second W10 acceptance bullet, on Linux: "`Unsupported` and `Cancelled`
//! are first-class returns everywhere; a test proves no impl `unwrap`s them."
//!
//! This is the behavioural half — every capability that is genuinely absent in
//! a session with no desktop is *called* here and must answer with an error
//! rather than abort. `tests/no_unwrap.rs` is the textual half.
//!
//! A session with no desktop is not an exotic configuration on Linux: it is
//! `cargo test`, `stratum serve` under a supervisor, a container, an `ssh`
//! session and every CI job. If these paths panicked, the failure would appear
//! for the first time in a maintainer's terminal on day one.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::future::Future;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};

use camino::Utf8PathBuf;
use stratum_platform::{
    Channel, ExternalUrl, FileDialogs, OpenRequest, PlatformError, StagedUpdate, UpdateInfo,
    UpdateStrategy, Updater,
};
use stratum_platform_linux::{LinuxFileDialogs, LinuxUpdater, NoPortal, Packaging};

fn now<F: Future>(f: F) -> F::Output {
    let mut f = Box::pin(f);
    match f.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(v) => v,
        Poll::Pending => panic!("this call was supposed to fail before doing any work"),
    }
}

fn info() -> UpdateInfo {
    UpdateInfo {
        version: "9.9.9".into(),
        notes: String::new(),
        published_ms: 0,
        artifact_url: "https://example.invalid/Stratum.AppImage.tar.gz".into(),
        signature: String::new(),
        size: None,
    }
}

/// A `.deb` install must not offer to update itself. Silently self-updating a
/// distro-managed install desynchronises the package database: the next
/// `apt upgrade` reinstalls the archive's version and the user's update is
/// undone with no message anywhere.
#[test]
fn a_package_managed_install_refuses_to_update_and_says_how_to() {
    let u = LinuxUpdater::new("0.4.1", "/tmp/staging", Packaging::SystemPackage);
    assert_eq!(u.strategy(), UpdateStrategy::PackageManaged);
    assert!(!u.strategy().can_self_install());

    let err = now(u.check(Channel::Stable)).unwrap_err();
    assert!(err.is_unsupported(), "{err}");
    let err = now(u.stage(&info(), Box::new(|_| {}))).unwrap_err();
    assert!(err.is_unsupported(), "{err}");

    // The command string is the whole feature for this shape.
    assert_eq!(
        u.upgrade_hint(),
        Some("apt upgrade stratum   (or: dnf upgrade stratum)")
    );
}

#[test]
fn an_unmanaged_build_has_no_update_strategy_at_all() {
    let u = LinuxUpdater::new("0.4.1", "/tmp/staging", Packaging::Unmanaged);
    assert_eq!(u.strategy(), UpdateStrategy::Disabled);
    assert!(now(u.check(Channel::Stable)).unwrap_err().is_unsupported());
    assert_eq!(u.upgrade_hint(), None);
}

/// An AppImage can self-install, but with no feed wired up it reports that
/// rather than pretending there is nothing new.
#[test]
fn an_appimage_can_self_install_but_needs_a_feed() {
    let u = LinuxUpdater::new(
        "0.4.1",
        "/tmp/staging",
        Packaging::AppImage(Utf8PathBuf::from("/home/jo/Stratum.AppImage")),
    );
    assert_eq!(u.strategy(), UpdateStrategy::AppImageSelfReplace);
    assert!(u.strategy().can_self_install());
    let err = now(u.check(Channel::Stable)).unwrap_err();
    assert!(matches!(err, PlatformError::BackendUnavailable(_)), "{err}");
}

/// §7.4, enforced where it can be acted on: an artifact whose minisign
/// signature was not checked is refused before the strategy is even consulted.
#[test]
fn an_unverified_update_is_refused_on_every_packaging() {
    for packaging in [
        Packaging::AppImage(Utf8PathBuf::from("/home/jo/Stratum.AppImage")),
        Packaging::SystemPackage,
        Packaging::Unmanaged,
    ] {
        let u = LinuxUpdater::new("0.4.1", "/tmp/staging", packaging.clone());
        let err = u
            .apply_and_relaunch(StagedUpdate::unverified("/tmp/nope.tar.gz", info()))
            .unwrap_err();
        assert!(
            matches!(err, PlatformError::PermissionDenied(ref m) if m.contains("minisign")),
            "{packaging:?}: {err}"
        );
    }
}

/// An update check reporting version `""` would treat every published release
/// as newer than us, which is an infinite update prompt.
#[test]
fn the_updater_never_reports_an_empty_current_version() {
    let u = LinuxUpdater::new(String::new(), "/tmp", Packaging::Unmanaged);
    assert_eq!(u.current_version(), env!("CARGO_PKG_VERSION"));
    let u = LinuxUpdater::new("0.4.2", "/tmp", Packaging::Unmanaged);
    assert_eq!(u.current_version(), "0.4.2");
}

/// No portal, no GTK, no file manager: three answers, no aborts.
#[test]
fn a_session_with_no_desktop_answers_every_dialog_call() {
    let d = LinuxFileDialogs::new(Arc::new(NoPortal));

    let err = now(d.open_files(OpenRequest::default())).unwrap_err();
    assert!(err.is_unsupported(), "{err}");

    // A path that does not exist is an IO error, not a panic — `reveal`
    // canonicalises first, exactly as the macOS impl does, because it is
    // reachable from a link in a rendered help page.
    let err = d
        .reveal(camino::Utf8Path::new("/nonexistent/stratum/test"))
        .unwrap_err();
    assert!(matches!(err, PlatformError::Io(_)), "{err}");

    // `open_external` is DELIBERATELY not invoked here.
    //
    // It really does spawn the session's URL handler, and this file carries no
    // `cfg(target_os)` guard, so on a developer's macOS machine the fallback
    // chain reached Homebrew's `gio` and opened a real browser tab on every
    // `cargo test --workspace`. A unit test must not have side effects on the
    // machine running it, and "no handler on PATH" is not a property a test can
    // assume when PATH belongs to whoever invoked cargo.
    //
    // The scheme allow-list -- the part that is actually security-relevant and
    // actually ours -- is asserted without spawning anything by
    // `open_external_can_never_be_handed_a_local_path_or_a_custom_scheme`
    // below, which checks rejection before any process is created.
}

/// The scheme allow-list is the security boundary in front of `xdg-open`, and
/// `xdg-open` will happily run a `.desktop` file handed to it as a `file://`
/// URL. Asserted here as well as in the trait crate because this is the impl
/// that actually spawns the handler.
#[test]
fn open_external_can_never_be_handed_a_local_path_or_a_custom_scheme() {
    for u in [
        "file:///usr/share/applications/evil.desktop",
        "javascript:alert(1)",
        "smb://host/share",
        "https://example.com/ a",
    ] {
        assert!(ExternalUrl::parse(u).is_err(), "{u}");
    }
}
