//! The AppUserModelID, and why a toast without one is worse than no toast.
//!
//! Windows routes a desktop toast by AUMID. If the calling process has not set
//! one, or the one it set does not match a Start-menu shortcut carrying
//! `System.AppUserModel.ID`, `ToastNotifier::Show` **succeeds and nothing
//! appears** — no error, no log line, no window. That silent success is the
//! specific failure W24's acceptance names, and the whole point of this module
//! is to convert it into a [`stratum_platform::PlatformError::Unsupported`]
//! that a caller can render.
//!
//! Two halves, and only one of them is a syscall:
//!
//! * The *identity* — what our AUMID is and whether a candidate string is even
//!   legal — is pure, and is checked here on every host.
//! * The *registration* — `SetCurrentProcessExplicitAppUserModelID`, and
//!   looking for the shortcut the installer laid down — is `sys` below.
//!
//! # Why a shortcut on disk is the test, and not the shortcut's property store
//!
//! The exact condition Windows checks is `System.AppUserModel.ID` inside the
//! `.lnk`'s property store, which means `IShellLink` + `IPropertyStore` + a COM
//! apartment. We check for the shortcut *file* instead. The installer (W22) is
//! the only thing that creates it and the only thing that can set the property,
//! so on any machine where the file exists but the property is missing, the
//! installer is broken — and a runtime check cannot repair that, it can only
//! misreport whose bug it is. The file's presence is the honest signal this
//! layer can obtain, and it converts the silent-drop case (`cargo run`, a
//! portable unzip, CI) into `Unsupported`, which is the case that actually
//! bites.

use camino::Utf8PathBuf;
use stratum_platform::{paths::PRODUCT, Env, PlatformError, Result};

/// Our AppUserModelID.
///
/// Deliberately identical to [`stratum_platform::paths::BUNDLE_ID`]: one
/// application identity across the three platforms means the Start-menu
/// shortcut, the macOS bundle id and the Linux `.desktop` file cannot drift
/// apart, and it is what the installer writes into
/// `System.AppUserModel.ID`.
pub const AUMID: &str = stratum_platform::paths::BUNDLE_ID;

/// The Start-menu shortcut the installer creates: `Stratum.lnk`.
pub const SHORTCUT_FILE: &str = "Stratum.lnk";

/// Microsoft's documented ceiling for an AppUserModelID.
pub const MAX_LEN: usize = 128;

/// Reject a string that Windows will not accept as an AUMID.
///
/// The rules are Microsoft's, not ours: at most [`MAX_LEN`] characters, no
/// whitespace, and no `\` (the separator the shell uses when it composes an
/// AUMID from a relaunch command). A violation is
/// [`PlatformError::Unsupported`] rather than `Os`, because nothing went wrong
/// — the identity is simply not one the platform can carry.
///
/// # Errors
/// [`PlatformError::Unsupported`] for an empty, over-long, whitespace-bearing
/// or backslash-bearing id.
pub fn validate(aumid: &str) -> Result<()> {
    if aumid.is_empty() {
        return Err(PlatformError::Unsupported("the AppUserModelID is empty"));
    }
    if aumid.chars().count() > MAX_LEN {
        return Err(PlatformError::Unsupported(
            "an AppUserModelID may not exceed 128 characters",
        ));
    }
    if aumid.chars().any(char::is_whitespace) {
        return Err(PlatformError::Unsupported(
            "an AppUserModelID may not contain whitespace",
        ));
    }
    if aumid.contains('\\') {
        return Err(PlatformError::Unsupported(
            "an AppUserModelID may not contain a backslash",
        ));
    }
    Ok(())
}

/// Where the installer's Start-menu shortcut can be, per-user first.
///
/// Pure over the environment so the layout is asserted from any host — the
/// same reason [`stratum_platform::Paths::resolve`] takes an [`Env`].
/// `%APPDATA%` is the per-user Start menu and `%PROGRAMDATA%` the all-users
/// one; an install can have written either, and a per-user install is the one
/// that needs no elevation, so it is checked first.
#[must_use]
pub fn shortcut_candidates(env: &dyn Env) -> Vec<Utf8PathBuf> {
    let mut out = Vec::with_capacity(2);
    for base in [env.var("APPDATA"), env.var("PROGRAMDATA")] {
        let Some(base) = base else { continue };
        let mut p = base.trim_end_matches('\\').to_owned();
        p.push_str("\\Microsoft\\Windows\\Start Menu\\Programs\\");
        p.push_str(PRODUCT);
        p.push('\\');
        p.push_str(SHORTCUT_FILE);
        out.push(Utf8PathBuf::from(p));
        // Tauri's NSIS bundler writes the shortcut flat under `Programs\`
        // when no Start-menu folder is configured, which is our default.
        let flat = format!(
            "{}\\Microsoft\\Windows\\Start Menu\\Programs\\{SHORTCUT_FILE}",
            base.trim_end_matches('\\')
        );
        out.push(Utf8PathBuf::from(flat));
    }
    out
}

/// The error every notification path returns when the AUMID is not registered.
///
/// A single `const` rather than a formatted string: the reason is a property of
/// the *build and installation*, never of the notification being posted, and
/// naming it once keeps the four call sites saying the same thing.
pub const UNREGISTERED: PlatformError = PlatformError::Unsupported(
    "no Start-menu shortcut carries Stratum's AppUserModelID, so Windows would accept this \
     toast and show nothing; run the installed Stratum",
);

#[cfg(target_os = "windows")]
pub use sys::{is_registered, register_for_process};

#[cfg(target_os = "windows")]
mod sys {
    use std::sync::OnceLock;

    use stratum_platform::{Result, SystemEnv};
    use windows::core::PCWSTR;
    use windows::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;

    use crate::win;

    /// `SetCurrentProcessExplicitAppUserModelID`, once per process.
    ///
    /// Must run before the first `ToastNotificationManager` call and before any
    /// window is shown, or the shell has already decided which taskbar group
    /// this process belongs to.
    ///
    /// # Errors
    /// [`stratum_platform::PlatformError::Unsupported`] if [`super::AUMID`] is
    /// not a legal id, and [`stratum_platform::PlatformError::Os`] if the shell
    /// refuses it.
    pub fn register_for_process() -> Result<()> {
        static DONE: OnceLock<std::result::Result<(), (i32, String)>> = OnceLock::new();
        DONE.get_or_init(|| {
            if let Err(e) = super::validate(super::AUMID) {
                // `validate` only ever produces `Unsupported(&'static str)`,
                // and this cache stores a code/message pair; -1 is the
                // "we refused, the OS was never asked" sentinel.
                return Err((-1, e.to_string()));
            }
            let w = win::wide(super::AUMID);
            // SAFETY: `w` is a NUL-terminated UTF-16 buffer that outlives the
            // call, which is the entire contract of `PCWSTR`.
            unsafe { SetCurrentProcessExplicitAppUserModelID(PCWSTR(w.as_ptr())) }
                .map_err(|e| (e.code().0, e.message()))
        })
        .clone()
        .map_err(|(code, message)| {
            if code == -1 {
                super::UNREGISTERED
            } else {
                win::classify(code, message)
            }
        })
    }

    /// Whether a Start-menu shortcut for us exists. See the module docs for
    /// why this is the signal rather than the shortcut's property store.
    #[must_use]
    pub fn is_registered() -> bool {
        static FOUND: OnceLock<bool> = OnceLock::new();
        *FOUND.get_or_init(|| {
            super::shortcut_candidates(&SystemEnv)
                .iter()
                .any(|p| p.is_file())
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use std::collections::BTreeMap;

    use super::*;

    struct Fake(BTreeMap<String, String>);

    impl Env for Fake {
        fn var(&self, key: &str) -> Option<String> {
            self.0.get(key).cloned().filter(|v| !v.is_empty())
        }
        fn home(&self) -> Option<Utf8PathBuf> {
            None
        }
        fn exe_dir(&self) -> Option<Utf8PathBuf> {
            None
        }
    }

    fn env() -> Fake {
        Fake(BTreeMap::from([
            (
                "APPDATA".to_owned(),
                r"C:\Users\ada\AppData\Roaming".to_owned(),
            ),
            ("PROGRAMDATA".to_owned(), r"C:\ProgramData".to_owned()),
        ]))
    }

    /// The AUMID is the application identity, shared with the macOS bundle id
    /// and the Linux desktop file. If this ever diverges, the installer writes
    /// one string into the shortcut and the process announces another, and
    /// every toast is silently dropped.
    #[test]
    fn the_aumid_is_the_one_application_identity_and_is_legal() {
        assert_eq!(AUMID, "dev.stratum.app");
        assert!(validate(AUMID).is_ok());
    }

    #[test]
    fn microsofts_rules_are_enforced_before_the_shell_is_asked() {
        for bad in ["", "dev.stratum app", "dev\\stratum", &"x".repeat(129)] {
            let err = validate(bad).unwrap_err();
            assert!(err.is_unsupported(), "{bad}");
        }
        assert!(validate(&"x".repeat(128)).is_ok());
    }

    /// Backslashes, `Programs\`, and the per-user location first — asserted
    /// from a macOS test run, exactly as `Paths::resolve` asserts the Windows
    /// column of 08 §5.2.
    #[test]
    fn shortcut_candidates_are_the_two_start_menus_user_first() {
        let c = shortcut_candidates(&env());
        assert_eq!(
            c[0].as_str(),
            r"C:\Users\ada\AppData\Roaming\Microsoft\Windows\Start Menu\Programs\Stratum\Stratum.lnk"
        );
        assert_eq!(
            c[1].as_str(),
            r"C:\Users\ada\AppData\Roaming\Microsoft\Windows\Start Menu\Programs\Stratum.lnk"
        );
        assert!(c[2].as_str().starts_with(r"C:\ProgramData\"));
        assert_eq!(c.len(), 4);
        assert!(c.iter().all(|p| !p.as_str().contains('/')));
    }

    #[test]
    fn a_trailing_separator_in_appdata_does_not_double_up() {
        let e = Fake(BTreeMap::from([(
            "APPDATA".to_owned(),
            r"C:\Users\ada\AppData\Roaming\".to_owned(),
        )]));
        assert!(!shortcut_candidates(&e)[0].as_str().contains(r"\\"));
    }

    #[test]
    fn an_unset_environment_yields_no_candidates_rather_than_a_relative_path() {
        assert!(shortcut_candidates(&Fake(BTreeMap::new())).is_empty());
    }
}
