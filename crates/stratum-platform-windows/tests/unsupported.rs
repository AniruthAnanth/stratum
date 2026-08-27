//! The behavioural half of "`Unsupported` and `Cancelled` are first-class
//! returns everywhere" — the capabilities that are genuinely absent in a build
//! nobody installed are *called* here and must answer with an error rather than
//! abort.
//!
//! An uninstalled process is not an exotic configuration: it is `cargo run`,
//! `cargo test`, `stratum serve` under a supervisor, a portable unzip, and
//! every CI job. If these paths panicked, the failure would appear for the
//! first time in a developer's terminal on day one.
//!
//! **This file runs on Windows only, and that is a stated limitation, not a
//! hidden one.** The unit that wrote it has no Windows machine; it is verified
//! here with `cargo check --target x86_64-pc-windows-msvc --tests` and it runs
//! for real the first time CI's `windows-latest` job executes
//! `cargo nextest run -p stratum-platform-windows`. Everything that could be
//! made host-portable was — see `tests/accelerators.rs`, `tests/no_unwrap.rs`
//! and the `#[cfg(test)]` modules beside each pure function, which together
//! carry this crate's real bug surface.
#![cfg(target_os = "windows")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::future::Future;
use std::task::{Context, Poll, Waker};

use stratum_platform::{
    ActionId, Badge, Channel, CredentialBackend, KeymapPreset, MenuModel, MenuTarget, Notification,
    NotificationId, Platform, PlatformError, PlatformId, ShellKind, StagedUpdate, UpdateInfo,
    UpdateStrategy, WindowHandle,
};
use stratum_platform_windows::{WindowsConfig, WindowsPlatform};

/// Poll once. Every assertion below fails before its first `await`, so a real
/// executor would only hide a bug where one of them does not.
fn now<F: Future>(f: F) -> F::Output {
    let mut f = Box::pin(f);
    match f.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(v) => v,
        Poll::Pending => panic!("this call was supposed to fail before doing any work"),
    }
}

fn platform() -> WindowsPlatform {
    WindowsPlatform::new(WindowsConfig {
        version: "0.1.0".to_owned(),
        ..WindowsConfig::default()
    })
    .unwrap()
}

fn info() -> UpdateInfo {
    UpdateInfo {
        version: "9.9.9".into(),
        notes: String::new(),
        published_ms: 0,
        artifact_url: "https://example.invalid/Stratum-setup.exe".into(),
        signature: String::new(),
        size: None,
    }
}

/// A toast whose AUMID matches no Start-menu shortcut is accepted by Windows
/// and never shown. That silent success is the exact failure W24's acceptance
/// names, and converting it into an error a caller can render is the whole
/// point of `stratum_platform_windows::aumid`.
#[test]
fn a_toast_without_a_registered_aumid_is_unsupported_rather_than_silently_dropped() {
    let p = platform();
    let n = p.notifier();
    assert!(
        !stratum_platform_windows::aumid::is_registered(),
        "a cargo test run must not find an installed Start-menu shortcut"
    );

    assert!(n.request_permission().unwrap_err().is_unsupported());
    assert!(n
        .notify(&Notification::default())
        .unwrap_err()
        .is_unsupported());
    assert!(n
        .withdraw(NotificationId("stratum\tn0".into()))
        .unwrap_err()
        .is_unsupported());

    // Capabilities are a property of the OS, not of this process, and are
    // answered even when posting is not possible.
    assert!(n.capabilities().sound);
    assert!(!n.capabilities().actions);
}

/// Windows 11 removed live tiles and the taskbar overlay icon belongs to a
/// window this call is given none of. `Ok(())` for work that did not happen
/// would leave the queue count silently wrong forever.
#[test]
fn the_absent_badge_is_reported_not_faked() {
    let p = platform();
    assert!(!p.notifier().capabilities().badge);
    assert!(p
        .notifier()
        .set_badge(Badge::Count(3))
        .unwrap_err()
        .is_unsupported());
    assert!(p
        .notifier()
        .set_badge(Badge::None)
        .unwrap_err()
        .is_unsupported());
}

#[test]
fn an_uninstalled_build_has_no_update_strategy_and_says_so() {
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
/// signature was not checked is refused before anything is spawned.
#[test]
fn an_unverified_update_is_refused() {
    let p = platform();
    let err = p
        .updater()
        .apply_and_relaunch(StagedUpdate::unverified(
            r"C:\nope\Stratum-setup.exe",
            info(),
        ))
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
        .install(&MenuModel::default(), MenuTarget::Window(WindowHandle(1)))
        .unwrap_err();
    assert!(matches!(err, PlatformError::BackendUnavailable(_)), "{err}");

    // Windows has one menu bar per window; there is no application-wide one.
    let err = m
        .install(&MenuModel::default(), MenuTarget::Application)
        .unwrap_err();
    assert!(err.is_unsupported(), "{err}");
}

/// Windows 8 removed programmatic default-handler assignment. `PermissionDenied`
/// and not `Unsupported`: the capability exists, it belongs to the user, and
/// the UI shows a different affordance for the two.
#[test]
fn claiming_the_default_handler_is_the_users_action() {
    let p = platform();
    let mut a = stratum_platform::Association::alternate("do", "Stata do-file");
    a.role = stratum_platform::HandlerRole::Default;
    let err = p.shell().set_default_handler(&a).unwrap_err();
    assert!(matches!(err, PlatformError::PermissionDenied(_)), "{err}");
}

#[test]
fn revealing_a_missing_path_is_an_io_error_not_a_panic() {
    let p = platform();
    let err = p
        .dialogs()
        .reveal(camino::Utf8Path::new(r"C:\nonexistent\stratum\test"))
        .unwrap_err();
    assert!(matches!(err, PlatformError::Io(_)), "{err}");
}

/// The Credential Manager is present on every Windows since 2000, so unlike the
/// three above this one is expected to *work* in CI — reading a key that is not
/// there is `Ok(None)`, and listing a service with no items is an empty vector.
/// Both are states, not errors.
#[test]
fn an_absent_credential_is_a_state_not_an_error() {
    let p = platform();
    let c = p.credentials();
    let service = stratum_platform::credentials::service("w24-selftest");

    assert_eq!(c.backend(), CredentialBackend::WindowsCredentialManager);
    assert!(c.backend().is_os_store());
    assert!(c.get(&service, "absent").unwrap().is_none());
    assert_eq!(c.list_accounts(&service).unwrap(), Vec::<String>::new());
    // Deleting what is not there is `Ok`: the caller asked for a state.
    c.delete(&service, "absent").unwrap();
}

/// A full round trip through the real store, in a service namespace nothing
/// else uses, cleaned up either way. `CRED_PERSIST_LOCAL_MACHINE` is not
/// observable from `CredReadW`'s result, so what this proves is the encoding
/// and the enumeration filter, not the persistence class.
#[test]
fn a_secret_round_trips_through_the_credential_manager() {
    let p = platform();
    let c = p.credentials();
    let service = stratum_platform::credentials::service("w24-roundtrip");
    let secret = stratum_platform::SecretString::from("sk-\u{00e9}\u{1F511}-test".to_owned());

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        c.set(&service, "openai", &secret).unwrap();
        let got = c.get(&service, "openai").unwrap().unwrap();
        use stratum_platform::ExposeSecret as _;
        assert_eq!(got.expose_secret(), "sk-\u{00e9}\u{1F511}-test");

        // Replacing rather than duplicating.
        c.set(&service, "openai", &secret).unwrap();
        c.set(&service, "anthropic", &secret).unwrap();
        assert_eq!(c.list_accounts(&service).unwrap(), ["anthropic", "openai"]);
    }));
    let _ = c.delete(&service, "openai");
    let _ = c.delete(&service, "anthropic");
    if let Err(e) = result {
        std::panic::resume_unwind(e);
    }
    assert_eq!(c.list_accounts(&service).unwrap(), Vec::<String>::new());
}

#[test]
fn the_aggregate_answers_every_accessor() {
    let p = platform();
    assert_eq!(p.id(), PlatformId::Windows);
    assert!(p.paths().config_dir().as_str().contains("Stratum"));
    assert!(!p.paths().config_dir().as_str().contains('/'));
    assert_eq!(
        p.credentials().backend(),
        CredentialBackend::WindowsCredentialManager
    );
    assert!(p.processes().physical_cores() >= 1);
    assert!(p.processes().available_memory().is_some_and(|m| m > 0));
    assert!(p
        .menus()
        .accelerator(&ActionId::from("run.block"), KeymapPreset::Modern)
        .is_some());
    assert!(matches!(
        p.shell().shell_kind(),
        ShellKind::Cmd | ShellKind::PowerShell
    ));
}

/// `GetLogicalProcessorInformationEx`, not `GetSystemInfo`. A CI runner is a
/// hyperthreaded VM, so the physical count is normally strictly below the
/// logical one — but a single-vCPU runner makes them equal, so the assertion is
/// the direction, not the inequality.
#[test]
fn physical_cores_never_exceeds_the_logical_count() {
    let p = platform();
    let logical = std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get);
    let physical = p.processes().physical_cores();
    assert!(physical >= 1);
    assert!(
        physical <= logical,
        "physical {physical} > logical {logical}: this is reading the wrong number, which is \
         the BLAS oversubscription bug"
    );
    if let Some(perf) = p.processes().performance_cores() {
        assert!(perf >= 1 && perf <= physical);
    }
}

/// An update that never reports version `""`: a feed asked "what is newer than
/// ``?" answers "everything", and the resulting prompt loop is invisible until
/// a user is stuck in it. `WindowsConfig::default()` leaves it empty, so the
/// substitution has to happen below it.
#[test]
fn the_updater_never_reports_an_empty_current_version() {
    let u = stratum_platform_windows::WindowsUpdater::new(String::new(), r"C:\tmp");
    assert_eq!(u.current_version(), env!("CARGO_PKG_VERSION"));
    let u = stratum_platform_windows::WindowsUpdater::new("0.4.2", r"C:\tmp");
    assert_eq!(u.current_version(), "0.4.2");
}
