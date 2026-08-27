//! Shell integration against the real filesystem and the real LaunchServices.
//!
//! Every shim assertion lives in ONE `#[test]` because it overrides `HOME`, and
//! `HOME` is process-global while cargo's test harness is threaded. Splitting
//! them would be a race that passes locally and fails on a busier machine.
#![cfg(target_os = "macos")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use camino::Utf8PathBuf;
use stratum_platform::{Association, HandlerRole, InstallScope, ShellIntegration, ShellKind};
use stratum_platform_macos::MacosShell;

#[test]
fn cli_shim_install_status_uninstall() {
    let home = tempfile::tempdir().unwrap();
    let home = Utf8PathBuf::from_path_buf(home.path().to_path_buf()).unwrap();
    std::env::set_var("HOME", home.as_str());

    let shell = MacosShell::new();

    let status = shell.shim_status().unwrap();
    assert!(status.installed_at.is_none() && !status.points_at_us);

    let link = shell.install_cli_shim(InstallScope::User).unwrap();
    assert_eq!(link, home.join(".local/bin/stratum"));
    assert!(link.symlink_metadata().unwrap().file_type().is_symlink());

    let status = shell.shim_status().unwrap();
    assert_eq!(status.installed_at.as_ref(), Some(&link));
    assert_eq!(status.scope, Some(InstallScope::User));
    assert!(status.points_at_us, "the shim should point at this binary");
    assert!(
        !status.on_path,
        "a fresh temp HOME is not on PATH, and the UI has to be able to say so"
    );

    // Re-installing over our own stale link is the common case after an update.
    let again = shell.install_cli_shim(InstallScope::User).unwrap();
    assert_eq!(again, link);

    shell.uninstall_cli_shim(InstallScope::User).unwrap();
    assert!(link.symlink_metadata().is_err());
    // Removing an absent shim is a state, not an event.
    shell.uninstall_cli_shim(InstallScope::User).unwrap();

    // A real file at the shim path was put there by somebody else and is not
    // ours to delete.
    std::fs::write(&link, b"#!/bin/sh\n").unwrap();
    let err = shell.uninstall_cli_shim(InstallScope::User).unwrap_err();
    assert!(
        matches!(err, stratum_platform::PlatformError::PermissionDenied(_)),
        "{err}"
    );
}

/// Read-only: this must never call `set_default_handler`, which would change
/// the developer's own file associations.
#[test]
fn default_handler_lookup_is_read_only_and_answers() {
    let shell = MacosShell::new();
    let txt = Association::alternate("txt", "Plain text");
    let info = shell.default_handler_of(&txt).unwrap();
    assert!(
        info.handler_id.is_some(),
        "macOS always has a handler for public.plain-text"
    );
    assert!(!info.is_us, "the test binary is not a document handler");

    // `.do` may have no handler at all on a machine without Stata; the point is
    // that the lookup answers rather than failing.
    let _ = shell.default_handler_of(&Association::alternate("do", "Stata do-file"));

    // The opt-in of 08 §6.3: an Alternate association cannot be promoted by
    // accident.
    let err = shell.set_default_handler(&txt).unwrap_err();
    assert!(err.is_unsupported(), "{err}");
    assert_eq!(txt.role, HandlerRole::Alternate);
}

#[test]
fn login_shell_env_answers_or_says_why() {
    let shell = MacosShell::new();
    match shell.login_shell_env() {
        Ok(env) => {
            assert!(
                env.contains_key("PATH"),
                "a login shell that prints no PATH is not a login shell"
            );
            // Cached: the second call must not run the shell again.
            assert_eq!(shell.login_shell_env().unwrap(), env);
        }
        // A profile that blocks is a real thing; the contract is that we say so
        // within the timeout instead of hanging the app's startup.
        Err(e) => assert!(
            matches!(e, stratum_platform::PlatformError::BackendUnavailable(_)),
            "{e}"
        ),
    }
    assert!(matches!(
        shell.shell_kind(),
        ShellKind::Zsh | ShellKind::Bash | ShellKind::Fish | ShellKind::Other(_)
    ));
}
