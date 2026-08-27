//! macOS shell integration — 08 §5.5.
//!
//! The CLI shim is a symlink to the `stratum` binary inside the app bundle:
//! `/usr/local/bin/stratum` for [`InstallScope::System`], `~/.local/bin/stratum`
//! for [`InstallScope::User`]. A symlink rather than a copy so that an update
//! does not leave a stale binary behind, and so that `uninstall` can refuse to
//! delete anything that is not a symlink — removing a real file at
//! `/usr/local/bin/stratum` that some other installer put there is not ours to
//! do.
//!
//! [`MacosShell::login_shell_env`] is the reason this trait exists at all: a
//! GUI app launched from Finder inherits `launchd`'s minimal `PATH`, so a
//! do-file that shells out to a Homebrew `python` fails in the app and works in
//! Terminal.

use std::collections::BTreeMap;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use camino::{Utf8Path, Utf8PathBuf};
use stratum_platform::{
    Association, HandlerInfo, HandlerRole, InstallScope, PlatformError, Result, ShellIntegration,
    ShellKind, ShimStatus,
};

/// How long we will wait for a login shell to print its environment. A profile
/// that takes longer than this is a profile that is waiting for something, and
/// blocking app startup on it is worse than falling back.
const LOGIN_SHELL_TIMEOUT: Duration = Duration::from_secs(3);

/// [`ShellIntegration`] for macOS.
#[derive(Debug, Default)]
pub struct MacosShell {
    login_env: OnceLock<BTreeMap<String, String>>,
}

impl MacosShell {
    /// Construct.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            login_env: OnceLock::new(),
        }
    }

    /// The binary a shim should point at: the `stratum` CLI beside the app's
    /// main executable when bundled, otherwise this executable.
    fn shim_target() -> Result<Utf8PathBuf> {
        let exe = std::env::current_exe()?;
        let exe = Utf8PathBuf::from_path_buf(exe)
            .map_err(|_| PlatformError::Unsupported("executable path is not valid UTF-8"))?;
        if let Some(dir) = exe.parent() {
            let cli = dir.join("stratum");
            if cli != exe && cli.is_file() {
                return Ok(cli);
            }
        }
        Ok(exe)
    }

    fn shim_path(scope: InstallScope) -> Result<Utf8PathBuf> {
        match scope {
            InstallScope::System => Ok(Utf8PathBuf::from("/usr/local/bin/stratum")),
            InstallScope::User => Ok(home()?.join(".local/bin/stratum")),
        }
    }
}

impl ShellIntegration for MacosShell {
    fn install_cli_shim(&self, scope: InstallScope) -> Result<Utf8PathBuf> {
        let target = Self::shim_target()?;
        let link = Self::shim_path(scope)?;
        if let Some(dir) = link.parent() {
            std::fs::create_dir_all(dir).map_err(|e| elevate(e, dir))?;
        }
        // `symlink` fails on an existing path, and replacing our own stale link
        // is the common case (the app moved, or an update landed).
        if link.symlink_metadata().is_ok() {
            std::fs::remove_file(&link).map_err(|e| elevate(e, &link))?;
        }
        std::os::unix::fs::symlink(&target, &link).map_err(|e| elevate(e, &link))?;
        Ok(link)
    }

    fn uninstall_cli_shim(&self, scope: InstallScope) -> Result<()> {
        let link = Self::shim_path(scope)?;
        let Ok(meta) = link.symlink_metadata() else {
            // Already absent: the caller asked for a state.
            return Ok(());
        };
        if !meta.file_type().is_symlink() {
            return Err(PlatformError::PermissionDenied(format!(
                "{link} is not a symlink; Stratum did not create it and will not remove it"
            )));
        }
        std::fs::remove_file(&link).map_err(|e| elevate(e, &link))
    }

    fn shim_status(&self) -> Result<ShimStatus> {
        let target = Self::shim_target().ok();
        let path_dirs: Vec<String> = std::env::var("PATH")
            .unwrap_or_default()
            .split(':')
            .map(str::to_owned)
            .collect();

        for scope in [InstallScope::System, InstallScope::User] {
            let link = Self::shim_path(scope)?;
            if link.symlink_metadata().is_err() {
                continue;
            }
            let points_at_us = std::fs::read_link(&link)
                .ok()
                .and_then(|p| Utf8PathBuf::from_path_buf(p).ok())
                .zip(target.as_ref())
                .is_some_and(|(actual, want)| &actual == want);
            let on_path = link
                .parent()
                .is_some_and(|d| path_dirs.iter().any(|p| p == d.as_str()));
            return Ok(ShimStatus {
                installed_at: Some(link),
                scope: Some(scope),
                on_path,
                points_at_us,
            });
        }
        Ok(ShimStatus {
            installed_at: None,
            scope: None,
            on_path: false,
            points_at_us: false,
        })
    }

    fn register_file_associations(&self, _assoc: &[Association]) -> Result<()> {
        // Not a gap: on macOS the association IS the bundle's `CFBundleDocumentTypes`,
        // declared at build time (W22) and picked up by LaunchServices when the
        // app is first launched. There is no runtime registration to perform,
        // and pretending to do one would be the lie.
        Err(PlatformError::Unsupported(
            "macOS declares file associations in the bundle's Info.plist; nothing to do at runtime",
        ))
    }

    fn set_default_handler(&self, assoc: &Association) -> Result<()> {
        if assoc.role != HandlerRole::Default {
            return Err(PlatformError::Unsupported(
                "set_default_handler needs HandlerRole::Default; registering an alternate \
                 handler is the bundle's job",
            ));
        }
        let uti = uti_for_extension(&assoc.extension)?;
        let bundle_id = crate::bundle::identifier().ok_or(PlatformError::Unsupported(
            "only a bundled app can be a document handler",
        ))?;
        launch_services::set_default_handler(&uti, &bundle_id)
    }

    fn default_handler_of(&self, assoc: &Association) -> Result<HandlerInfo> {
        let uti = uti_for_extension(&assoc.extension)?;
        let handler = launch_services::default_handler(&uti);
        let us = crate::bundle::identifier();
        Ok(HandlerInfo {
            is_us: match (&handler, &us) {
                (Some(h), Some(u)) => h.eq_ignore_ascii_case(u),
                _ => false,
            },
            handler_id: handler,
        })
    }

    fn login_shell_env(&self) -> Result<BTreeMap<String, String>> {
        if let Some(cached) = self.login_env.get() {
            return Ok(cached.clone());
        }
        let env = read_login_shell_env(&shell_program())?;
        Ok(self.login_env.get_or_init(|| env).clone())
    }

    fn shell_kind(&self) -> ShellKind {
        ShellKind::from_program(&shell_program())
    }
}

fn home() -> Result<Utf8PathBuf> {
    let h = std::env::var("HOME").map_err(|_| {
        PlatformError::BackendUnavailable("HOME is not set in this session".to_owned())
    })?;
    Ok(Utf8PathBuf::from(h))
}

/// `$SHELL`, or the macOS default. Never an empty string.
fn shell_program() -> String {
    std::env::var("SHELL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/bin/zsh".to_owned())
}

/// `EACCES`/`EPERM` become [`PlatformError::PermissionDenied`] rather than a
/// raw IO error: "install the shim system-wide" failing for lack of admin
/// rights is an outcome with a UI affordance, not a crash.
fn elevate(e: std::io::Error, path: &Utf8Path) -> PlatformError {
    match e.raw_os_error() {
        Some(libc::EACCES | libc::EPERM | libc::EROFS) => {
            PlatformError::PermissionDenied(format!("{path}: {e}"))
        }
        _ => PlatformError::Io(e),
    }
}

/// Run the login shell once and parse what it exports.
///
/// `-l -c` and NUL-separated output: `-i` would source the interactive rc file,
/// which on a real machine prints banners, runs `nvm`, and occasionally waits
/// for input. NUL separation is required because `PATH`-adjacent variables
/// legitimately contain newlines.
fn read_login_shell_env(shell: &str) -> Result<BTreeMap<String, String>> {
    use std::process::{Command, Stdio};

    let mut child = Command::new(shell)
        .args(["-l", "-c", "/usr/bin/env -0"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| {
            PlatformError::BackendUnavailable(format!("could not run the login shell {shell}: {e}"))
        })?;

    let deadline = Instant::now() + LOGIN_SHELL_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(PlatformError::BackendUnavailable(format!(
                    "{shell} did not print its environment within {LOGIN_SHELL_TIMEOUT:?}; \
                     a login profile is waiting for something"
                )));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(e) => return Err(PlatformError::Io(e)),
        }
    }

    let mut out = Vec::new();
    if let Some(mut stdout) = child.stdout.take() {
        use std::io::Read;
        stdout.read_to_end(&mut out)?;
    }
    Ok(parse_env0(&out))
}

/// Parse `env -0` output. Public to the crate so the test can feed it a fixture
/// rather than depending on the developer's own shell profile.
pub(crate) fn parse_env0(bytes: &[u8]) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for entry in bytes.split(|b| *b == 0) {
        if entry.is_empty() {
            continue;
        }
        let Ok(s) = std::str::from_utf8(entry) else {
            continue;
        };
        if let Some((k, v)) = s.split_once('=') {
            map.insert(k.to_owned(), v.to_owned());
        }
    }
    map
}

/// The Uniform Type Identifier for a filename extension, e.g. `do` →
/// `com.stata.do` or a dynamic `dyn.…` UTI when nothing claims it.
fn uti_for_extension(ext: &str) -> Result<String> {
    launch_services::uti_for_extension(ext.trim_start_matches('.')).ok_or(
        PlatformError::Unsupported("no Uniform Type Identifier for this extension"),
    )
}

/// The three LaunchServices calls, wrapped.
///
/// These are the deprecated-but-live C entry points; the modern replacement
/// (`NSWorkspace.setDefaultApplicationAtURL:toOpenContentType:`) is async,
/// arrived in macOS 12, and would raise our floor for no behavioural gain.
mod launch_services {
    use core_foundation::base::TCFType;
    use core_foundation::string::{CFString, CFStringRef};
    use core_foundation_sys::base::OSStatus;
    use stratum_platform::{PlatformError, Result};

    // `kLSRolesAll`.
    const K_LS_ROLES_ALL: u32 = 0xFFFF_FFFF;

    #[link(name = "CoreServices", kind = "framework")]
    extern "C" {
        static kUTTagClassFilenameExtension: CFStringRef;
        fn UTTypeCreatePreferredIdentifierForTag(
            tag_class: CFStringRef,
            tag: CFStringRef,
            conforming_to: CFStringRef,
        ) -> CFStringRef;
        fn LSCopyDefaultRoleHandlerForContentType(
            content_type: CFStringRef,
            role: u32,
        ) -> CFStringRef;
        fn LSSetDefaultRoleHandlerForContentType(
            content_type: CFStringRef,
            role: u32,
            handler: CFStringRef,
        ) -> OSStatus;
    }

    pub(super) fn uti_for_extension(ext: &str) -> Option<String> {
        let tag = CFString::new(ext);
        // SAFETY: both arguments are live CFStrings; the result is +1 or NULL.
        let raw = unsafe {
            UTTypeCreatePreferredIdentifierForTag(
                kUTTagClassFilenameExtension,
                tag.as_concrete_TypeRef(),
                std::ptr::null(),
            )
        };
        if raw.is_null() {
            return None;
        }
        Some(unsafe { CFString::wrap_under_create_rule(raw) }.to_string())
    }

    pub(super) fn default_handler(uti: &str) -> Option<String> {
        let uti = CFString::new(uti);
        // SAFETY: as above; NULL means nothing is registered.
        let raw = unsafe {
            LSCopyDefaultRoleHandlerForContentType(uti.as_concrete_TypeRef(), K_LS_ROLES_ALL)
        };
        if raw.is_null() {
            return None;
        }
        Some(unsafe { CFString::wrap_under_create_rule(raw) }.to_string())
    }

    pub(super) fn set_default_handler(uti: &str, bundle_id: &str) -> Result<()> {
        let uti = CFString::new(uti);
        let handler = CFString::new(bundle_id);
        // SAFETY: two live CFStrings and a documented role mask.
        let status = unsafe {
            LSSetDefaultRoleHandlerForContentType(
                uti.as_concrete_TypeRef(),
                K_LS_ROLES_ALL,
                handler.as_concrete_TypeRef(),
            )
        };
        if status == 0 {
            Ok(())
        } else {
            Err(PlatformError::Os {
                code: i64::from(status),
                message: "LSSetDefaultRoleHandlerForContentType failed".to_owned(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// NUL separation, not newline: a `PATH`-adjacent variable legitimately
    /// contains a newline, and a line-based parser silently truncates it.
    #[test]
    fn env0_parses_values_containing_newlines_and_equals_signs() {
        let raw = b"PATH=/usr/bin:/bin\0GREETING=hi\nthere\0Q=a=b\0EMPTY=\0\0";
        let env = parse_env0(raw);
        assert_eq!(env["PATH"], "/usr/bin:/bin");
        assert_eq!(env["GREETING"], "hi\nthere");
        assert_eq!(env["Q"], "a=b");
        assert_eq!(env["EMPTY"], "");
        assert_eq!(env.len(), 4);
    }

    #[test]
    fn a_line_without_an_equals_sign_is_skipped_not_guessed_at() {
        assert!(parse_env0(b"NOTANASSIGNMENT\0").is_empty());
    }
}
