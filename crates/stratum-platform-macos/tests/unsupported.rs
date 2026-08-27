//! The second W10 acceptance bullet: "`Unsupported` and `Cancelled` are
//! first-class returns everywhere; a test proves no impl `unwrap`s them."
//!
//! This is the behavioural half — every capability that is genuinely absent in
//! an unbundled process is *called* here and must answer with an error rather
//! than abort. `tests/no_unwrap.rs` is the textual half.
//!
//! An unbundled process is not an exotic configuration: it is `cargo run`,
//! `cargo test`, `stratum serve` under a supervisor, and every CI job. If these
//! paths panicked, the failure would appear for the first time in a developer's
//! terminal on day one.
#![cfg(target_os = "macos")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::future::Future;
use std::task::{Context, Poll, Waker};

use stratum_platform::{
    ActionId, Badge, Channel, KeymapPreset, MenuModel, MenuTarget, Notification, NotificationId,
    Platform, PlatformError, PlatformId, StagedUpdate, UpdateInfo, UpdateStrategy,
};
use stratum_platform_macos::{MacosConfig, MacosPlatform};

/// Poll once. Every assertion below fails before its first `await`, so a real
/// executor would only hide a bug where one of them does not.
fn now<F: Future>(f: F) -> F::Output {
    let mut f = Box::pin(f);
    match f.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(v) => v,
        Poll::Pending => panic!("this call was supposed to fail before doing any work"),
    }
}

fn platform() -> MacosPlatform {
    MacosPlatform::new(MacosConfig {
        version: "0.1.0".to_owned(),
        ..MacosConfig::default()
    })
    .unwrap()
}

#[test]
fn notifications_are_unsupported_without_a_bundle_rather_than_fatal() {
    let p = platform();
    let n = p.notifier();
    assert!(!stratum_platform_macos::bundle::is_bundled());

    let err = n.request_permission().unwrap_err();
    assert!(err.is_unsupported(), "{err}");
    let err = n.notify(&Notification::default()).unwrap_err();
    assert!(err.is_unsupported(), "{err}");
    let err = n.withdraw(NotificationId("x".into())).unwrap_err();
    assert!(err.is_unsupported(), "{err}");
    let err = n.set_badge(Badge::Count(3)).unwrap_err();
    assert!(err.is_unsupported(), "{err}");

    // Capabilities are a property of the OS, not of this process, and are
    // answered even when posting is not possible.
    assert!(n.capabilities().badge && n.capabilities().sound);
}

#[test]
fn an_unbundled_build_has_no_update_strategy_and_says_so() {
    let p = platform();
    let u = p.updater();
    assert_eq!(u.strategy(), UpdateStrategy::Disabled);
    assert!(!u.strategy().can_self_install());

    assert!(now(u.check(Channel::Stable)).unwrap_err().is_unsupported());
    assert!(now(u.stage(&info(), Box::new(|_| {})))
        .unwrap_err()
        .is_unsupported());
}

/// §7.4, enforced where it can be acted on: an artifact whose minisign
/// signature was not checked is refused even before the strategy is consulted.
#[test]
fn an_unverified_update_is_refused() {
    let p = platform();
    let err = p
        .updater()
        .apply_and_relaunch(StagedUpdate::unverified("/tmp/nope.tar.gz", info()))
        .unwrap_err();
    assert!(
        matches!(err, PlatformError::PermissionDenied(ref m) if m.contains("minisign")),
        "{err}"
    );
}

#[test]
fn a_menu_host_with_no_sink_reports_it_instead_of_pretending() {
    let p = platform();
    let m = p.menus();

    let err = m
        .install(&MenuModel::default(), MenuTarget::Application)
        .unwrap_err();
    assert!(matches!(err, PlatformError::BackendUnavailable(_)), "{err}");

    // macOS has one menu bar; a per-window install is not a thing the OS can
    // do, and doing it application-wide instead would look like it worked.
    let err = m
        .install(
            &MenuModel::default(),
            MenuTarget::Window(stratum_platform::WindowHandle(1)),
        )
        .unwrap_err();
    assert!(err.is_unsupported(), "{err}");
}

/// macOS declares associations in `Info.plist`; there is no runtime call, and
/// saying so is better than returning `Ok(())` for work that did not happen.
#[test]
fn file_association_registration_is_unsupported_not_silently_ok() {
    let p = platform();
    let err = p.shell().register_file_associations(&[]).unwrap_err();
    assert!(err.is_unsupported(), "{err}");
}

#[test]
fn revealing_a_missing_path_is_an_io_error_not_a_panic() {
    let p = platform();
    let err = p
        .dialogs()
        .reveal(camino::Utf8Path::new("/nonexistent/stratum/test"))
        .unwrap_err();
    assert!(matches!(err, PlatformError::Io(_)), "{err}");
}

#[test]
fn the_aggregate_answers_every_accessor() {
    let p = platform();
    assert_eq!(p.id(), PlatformId::MacOs);
    assert!(p.paths().config_dir().as_str().contains("dev.stratum.app"));
    assert_eq!(
        p.credentials().backend(),
        stratum_platform::CredentialBackend::MacosKeychain
    );
    assert!(p.processes().physical_cores() >= 1);
    assert!(p
        .menus()
        .accelerator(&ActionId::from("run.block"), KeymapPreset::Modern)
        .is_some());
    // The kind follows the machine's `$SHELL` (zsh only as the macOS default
    // when it is unset) — CI runners log in with bash, so asserting a literal
    // `Zsh` here tests the runner, not the accessor.
    let expected = std::env::var("SHELL")
        .map(|s| stratum_platform::ShellKind::from_program(&s))
        .unwrap_or(stratum_platform::ShellKind::Zsh);
    assert_eq!(p.shell().shell_kind(), expected);
}

fn info() -> UpdateInfo {
    UpdateInfo {
        version: "9.9.9".into(),
        notes: String::new(),
        published_ms: 0,
        artifact_url: "https://example.invalid/Stratum.app.tar.gz".into(),
        signature: String::new(),
        size: None,
    }
}

/// An update check reporting version `""` would treat every published release
/// as newer than us, which is an infinite update prompt. `MacosConfig::default()`
/// leaves the version empty, so the substitution has to happen below it.
#[test]
fn the_updater_never_reports_an_empty_current_version() {
    let u = stratum_platform_macos::MacosUpdater::new(String::new(), "/tmp");
    assert_eq!(u.current_version(), env!("CARGO_PKG_VERSION"));
    let u = stratum_platform_macos::MacosUpdater::new("0.4.2", "/tmp");
    assert_eq!(u.current_version(), "0.4.2");
}
