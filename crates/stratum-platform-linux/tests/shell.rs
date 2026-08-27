//! Shell integration: the CLI shim and §6.3's file associations.
//!
//! Every path below is inside a temporary directory. A test that writes to the
//! developer's real `~/.local/share` and then asserts on `/usr/local/bin`
//! passes or fails depending on what else they have installed, which is not a
//! test.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use camino::Utf8PathBuf;
use stratum_platform::{Association, HandlerRole, InstallScope, PlatformError, ShellIntegration};
use stratum_platform_linux::{mime, LinuxShell, Packaging, XdgDirs};

struct Sandbox {
    _guard: tempfile::TempDir,
    root: Utf8PathBuf,
    dirs: XdgDirs,
}

fn sandbox() -> Sandbox {
    let guard = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(guard.path().to_path_buf()).unwrap();
    let dirs = XdgDirs {
        config_home: root.join("config"),
        data_home: root.join("data"),
        data_dirs: vec![root.join("system-share")],
        user_bin: root.join("bin"),
        system_bin: root.join("sbin"),
    };
    Sandbox {
        _guard: guard,
        root,
        dirs,
    }
}

/// A stand-in for the installed executable, so `shim_target` has something real
/// to point at.
fn fake_exe(root: &Utf8PathBuf) -> Utf8PathBuf {
    let dir = root.join("opt/stratum");
    std::fs::create_dir_all(&dir).unwrap();
    let exe = dir.join("stratum-desktop");
    std::fs::write(&exe, b"#!/bin/true\n").unwrap();
    std::fs::write(dir.join("stratum"), b"#!/bin/true\n").unwrap();
    exe
}

// cfg(unix): the shim IS a symlink, and the non-unix build of `symlink`
// answers Unsupported by design — there is no Windows behaviour to test.
#[cfg(unix)]
#[test]
fn a_user_shim_is_a_symlink_to_the_cli_beside_the_app() {
    let s = sandbox();
    let exe = fake_exe(&s.root);
    let shell = LinuxShell::new(s.dirs.clone(), Packaging::Unmanaged, Some(exe.clone()));

    let link = shell.install_cli_shim(InstallScope::User).unwrap();
    assert_eq!(link, s.dirs.user_bin.join("stratum"));
    // The CLI binary Tauri's `externalBin` puts beside the app, not the
    // desktop executable itself.
    assert_eq!(
        Utf8PathBuf::from_path_buf(std::fs::read_link(&link).unwrap()).unwrap(),
        exe.parent().unwrap().join("stratum")
    );

    let status = shell.shim_status().unwrap();
    assert_eq!(status.installed_at.as_deref(), Some(link.as_path()));
    assert_eq!(status.scope, Some(InstallScope::User));
    assert!(status.points_at_us);
    // The directory is not on this test process's PATH, and the UI has to be
    // able to say "installed and useless".
    assert!(!status.on_path);

    shell.uninstall_cli_shim(InstallScope::User).unwrap();
    assert!(shell.shim_status().unwrap().installed_at.is_none());
    // Removing an absent shim is a state, not an event.
    assert!(shell.uninstall_cli_shim(InstallScope::User).is_ok());
}

/// Installing over our own stale link is the common case: the app moved, or an
/// update landed somewhere new.
// cfg(unix): installs a symlink, as above.
#[cfg(unix)]
#[test]
fn installing_twice_replaces_our_own_link() {
    let s = sandbox();
    let exe = fake_exe(&s.root);
    let shell = LinuxShell::new(s.dirs.clone(), Packaging::Unmanaged, Some(exe));
    let first = shell.install_cli_shim(InstallScope::User).unwrap();
    let second = shell.install_cli_shim(InstallScope::User).unwrap();
    assert_eq!(first, second);
}

/// Someone else's `stratum` at that path is not ours to delete.
#[test]
fn a_shim_we_did_not_create_is_not_removed() {
    let s = sandbox();
    let exe = fake_exe(&s.root);
    let shell = LinuxShell::new(s.dirs.clone(), Packaging::Unmanaged, Some(exe));

    std::fs::create_dir_all(&s.dirs.user_bin).unwrap();
    std::fs::write(s.dirs.user_bin.join("stratum"), b"#!/bin/sh\necho hi\n").unwrap();

    let err = shell.uninstall_cli_shim(InstallScope::User).unwrap_err();
    assert!(matches!(err, PlatformError::PermissionDenied(_)), "{err}");
}

/// An AppImage gets an exec script: launching the image directly opens the GUI,
/// and a `stratum` on `PATH` that opens a window is not a CLI.
// cfg(unix): the script is chmod 0755, and the non-unix `set_executable`
// answers Unsupported by design.
#[cfg(unix)]
#[test]
fn an_appimage_shim_is_a_script_that_quotes_its_path() {
    let s = sandbox();
    let image = s.root.join("My Apps/Stratum-0.4.2.AppImage");
    std::fs::create_dir_all(image.parent().unwrap()).unwrap();
    std::fs::write(&image, b"AI\x02").unwrap();

    let shell = LinuxShell::new(
        s.dirs.clone(),
        Packaging::AppImage(image.clone()),
        Some(s.root.join("tmp/.mount_x/usr/bin/stratum")),
    );
    let link = shell.install_cli_shim(InstallScope::User).unwrap();
    let script = std::fs::read_to_string(&link).unwrap();

    assert!(script.starts_with("#!/bin/sh\n"), "{script}");
    assert!(script.contains("# Written by Stratum."), "{script}");
    // The space in the directory name must survive.
    assert!(
        script.contains(&format!("exec '{image}' --cli \"$@\"")),
        "{script}"
    );
    assert!(shell.shim_status().unwrap().points_at_us);
    // Our own script IS removable — the marker comment is what identifies it.
    assert!(shell.uninstall_cli_shim(InstallScope::User).is_ok());
}

/// A `.deb`, `.rpm`, Flatpak or Snap already owns `PATH`; a shim there shadows
/// the packaged binary and freezes the user on today's build forever.
#[test]
fn a_packaged_install_refuses_the_shim_rather_than_shadowing_itself() {
    let s = sandbox();
    for packaging in [
        Packaging::SystemPackage,
        Packaging::Flatpak,
        Packaging::Snap,
    ] {
        let shell = LinuxShell::new(
            s.dirs.clone(),
            packaging.clone(),
            Some(Utf8PathBuf::from("/usr/bin/stratum")),
        );
        let err = shell.install_cli_shim(InstallScope::User).unwrap_err();
        assert!(err.is_unsupported(), "{packaging:?}: {err}");
    }
}

/// A `Default`-role association is refused BEFORE anything is written, so a
/// rejected call leaves no launcher icon behind.
#[test]
fn a_default_role_association_is_refused_before_any_file_is_written() {
    let s = sandbox();
    let exe = fake_exe(&s.root);
    let shell = LinuxShell::new(s.dirs.clone(), Packaging::Unmanaged, Some(exe));

    let mut assoc = Association::alternate("do", "Stata do-file");
    assoc.role = HandlerRole::Default;
    let err = shell.register_file_associations(&[assoc]).unwrap_err();
    assert!(err.is_unsupported(), "{err}");
    assert!(!s.dirs.desktop_entry().exists());
    assert!(!s.dirs.mime_package().exists());
}

/// §6.3, end to end: registering writes the entry, the MIME package and the
/// `[Added Associations]` group, and leaves the DEFAULT alone.
#[test]
fn registering_associations_offers_us_without_taking_the_default() {
    let s = sandbox();
    let exe = fake_exe(&s.root);
    let shell = LinuxShell::new(s.dirs.clone(), Packaging::Unmanaged, Some(exe));

    // A machine where Stata already owns .do — the situation §6.3 is about.
    std::fs::create_dir_all(&s.dirs.config_home).unwrap();
    std::fs::write(
        s.dirs.mimeapps(),
        format!("[Default Applications]\n{}=stata.desktop\n", mime::MIME_DO),
    )
    .unwrap();

    shell
        .register_file_associations(&[
            Association::alternate("do", "Stata do-file"),
            Association::alternate("dta", "Stata dataset"),
        ])
        .unwrap();

    let entry = std::fs::read_to_string(s.dirs.desktop_entry()).unwrap();
    assert!(entry.contains("StartupWMClass=Stratum"));
    let pkg = std::fs::read_to_string(s.dirs.mime_package()).unwrap();
    assert!(pkg.contains("&lt;stata_dta&gt;"));

    let list = std::fs::read_to_string(s.dirs.mimeapps()).unwrap();
    assert!(list.contains(&format!("{}={};", mime::MIME_DO, mime::DESKTOP_FILE)));
    // The scheme handler comes along, or `stratum://` OAuth callbacks (§21/§22)
    // never reach us.
    assert!(list.contains(mime::MIME_SCHEME));

    // Stata is still the default, which is the entire point.
    let who = shell
        .default_handler_of(&Association::alternate("do", "Stata do-file"))
        .unwrap();
    assert_eq!(who.handler_id.as_deref(), Some("stata.desktop"));
    assert!(!who.is_us);
}

/// Taking the default is a separate, explicit call.
#[test]
fn becoming_the_default_is_a_second_deliberate_step() {
    let s = sandbox();
    let exe = fake_exe(&s.root);
    let shell = LinuxShell::new(s.dirs.clone(), Packaging::Unmanaged, Some(exe));

    let mut assoc = Association::alternate("do", "Stata do-file");
    // The convenience constructor cannot produce `Default`, and the setter
    // refuses anything else.
    let err = shell.set_default_handler(&assoc).unwrap_err();
    assert!(err.is_unsupported(), "{err}");

    assoc.role = HandlerRole::Default;
    shell.set_default_handler(&assoc).unwrap();
    let who = shell.default_handler_of(&assoc).unwrap();
    assert_eq!(who.handler_id.as_deref(), Some(mime::DESKTOP_FILE));
    assert!(who.is_us);
}

/// The freedesktop search order: the user's file first, then the system ones.
/// Reading only the user's file reports "nothing handles .do" on a machine
/// where a packaged entry does.
#[test]
fn the_system_mimeapps_files_are_consulted_after_the_users() {
    let s = sandbox();
    let exe = fake_exe(&s.root);
    let shell = LinuxShell::new(s.dirs.clone(), Packaging::Unmanaged, Some(exe));

    let system = s.dirs.data_dirs[0].join("applications/mimeapps.list");
    std::fs::create_dir_all(system.parent().unwrap()).unwrap();
    std::fs::write(
        &system,
        format!("[Default Applications]\n{}=stata.desktop\n", mime::MIME_DTA),
    )
    .unwrap();

    let who = shell
        .default_handler_of(&Association::alternate("dta", "Stata dataset"))
        .unwrap();
    assert_eq!(who.handler_id.as_deref(), Some("stata.desktop"));

    // An extension we do not claim is `Unsupported`, not "nobody handles it".
    let err = shell
        .default_handler_of(&Association::alternate("smcl", "SMCL log"))
        .unwrap_err();
    assert!(err.is_unsupported(), "{err}");
}

/// A packaged install already ships the desktop entry and MIME package
/// system-wide (08 §6.1); a second copy in `~/.local/share` gives the user two
/// launcher icons.
#[test]
fn a_packaged_install_does_not_write_a_second_desktop_entry() {
    let s = sandbox();
    let shell = LinuxShell::new(
        s.dirs.clone(),
        Packaging::SystemPackage,
        Some(Utf8PathBuf::from("/usr/bin/stratum")),
    );
    let err = shell.register_file_associations(&[]).unwrap_err();
    assert!(err.is_unsupported(), "{err}");
    assert!(!s.dirs.desktop_entry().exists());
}
