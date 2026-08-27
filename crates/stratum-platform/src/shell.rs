//! Shell integration — 08 §5.5.
//!
//! Three jobs that have nothing in common except that they all talk to the
//! user's *shell environment* rather than to a window: the `stratum` CLI shim,
//! file associations, and the login-shell environment.
//!
//! [`ShellIntegration::login_shell_env`] is the one people forget. A macOS GUI
//! app launched from Finder inherits `launchd`'s minimal `PATH`, so a do-file
//! that shells out to a user-installed `python`, `R` or `git` fails with
//! "command not found" in the app and works in Terminal. The fix is to run the
//! login shell once and cache what it prints.

use std::collections::BTreeMap;

use camino::Utf8PathBuf;

use crate::Result;

/// Where a CLI shim is installed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum InstallScope {
    /// The user's own bin directory: `~/.local/bin` on Unix, the per-user
    /// `Path` on Windows. Never needs elevation.
    User,
    /// Machine-wide: `/usr/local/bin`, the system `Path`. Requires elevation
    /// and may return [`crate::PlatformError::PermissionDenied`].
    System,
}

/// What the CLI shim currently looks like.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ShimStatus {
    /// Where it is, when it is installed.
    pub installed_at: Option<Utf8PathBuf>,
    /// Which scope it was found in.
    pub scope: Option<InstallScope>,
    /// Whether its directory is on the user's `PATH`. A shim in
    /// `~/.local/bin` on a machine that does not add that directory is
    /// installed and useless, and the UI has to be able to say so.
    pub on_path: bool,
    /// Whether it points at *this* build. False after the app moves or an
    /// update lands somewhere new.
    pub points_at_us: bool,
}

/// How aggressively we want to be associated with an extension.
///
/// 08 §6.3: we never take over `.do`/`.dta` silently.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HandlerRole {
    /// Listed under "Open With", default unchanged. The only role
    /// [`ShellIntegration::register_file_associations`] ever applies.
    Alternate,
    /// The default application. Only ever through
    /// [`ShellIntegration::set_default_handler`], only on explicit user action.
    Default,
}

/// One file-type association.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Association {
    /// Extension WITHOUT the dot: `"do"`, `"dta"`, `"smcl"`.
    pub extension: String,
    /// Human description: "Stata do-file".
    pub description: String,
    /// Alternate or default.
    pub role: HandlerRole,
}

impl Association {
    /// Construct an `Alternate` association, which is the only kind we register
    /// without being asked.
    #[must_use]
    pub fn alternate(extension: &str, description: &str) -> Self {
        Self {
            extension: extension.trim_start_matches('.').to_owned(),
            description: description.to_owned(),
            role: HandlerRole::Alternate,
        }
    }
}

/// Who currently opens a file type.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct HandlerInfo {
    /// The OS's identifier for the handler: a bundle id, a ProgID, a `.desktop`
    /// file name. `None` when nothing is registered.
    pub handler_id: Option<String>,
    /// Whether that handler is us.
    pub is_us: bool,
}

/// The user's login shell, for quoting and for the shim's syntax.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ShellKind {
    /// zsh, the macOS default since Catalina.
    Zsh,
    /// bash.
    Bash,
    /// fish, whose `set -x` syntax differs from POSIX.
    Fish,
    /// PowerShell (Windows and cross-platform `pwsh`).
    PowerShell,
    /// `cmd.exe`.
    Cmd,
    /// nushell.
    Nushell,
    /// Anything else, by the basename of `$SHELL`.
    Other(String),
}

impl ShellKind {
    /// Classify from a `$SHELL`-style path or a program name.
    #[must_use]
    pub fn from_program(path: &str) -> Self {
        let name = path
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(path)
            .trim_end_matches(".exe");
        match name {
            "zsh" => Self::Zsh,
            "bash" | "sh" => Self::Bash,
            "fish" => Self::Fish,
            "pwsh" | "powershell" => Self::PowerShell,
            "cmd" => Self::Cmd,
            "nu" => Self::Nushell,
            other => Self::Other(other.to_owned()),
        }
    }
}

/// CLI shim, file associations and the login-shell environment.
pub trait ShellIntegration: Send + Sync {
    /// Put `stratum` on the user's `PATH` and return where it went.
    ///
    /// # Errors
    /// [`crate::PlatformError::PermissionDenied`] for
    /// [`InstallScope::System`] without elevation;
    /// [`crate::PlatformError::Unsupported`] where the packaging already owns
    /// `PATH` (a `.deb` puts `stratum` in `/usr/bin` itself).
    fn install_cli_shim(&self, scope: InstallScope) -> Result<Utf8PathBuf>;

    /// Remove it. Removing an absent shim is `Ok(())`.
    ///
    /// # Errors
    /// As [`ShellIntegration::install_cli_shim`].
    fn uninstall_cli_shim(&self, scope: InstallScope) -> Result<()>;

    /// Where the shim is and whether it works.
    ///
    /// # Errors
    /// [`crate::PlatformError::Io`] if the filesystem could not be read.
    fn shim_status(&self) -> Result<ShimStatus>;

    /// Register as an **alternate** handler for these types.
    ///
    /// # Errors
    /// [`crate::PlatformError::Unsupported`] where associations are declared at
    /// install time rather than at runtime (macOS `Info.plist`).
    fn register_file_associations(&self, assoc: &[Association]) -> Result<()>;

    /// Become the default handler. Only ever from an explicit user action —
    /// this is the opt-in of 08 §6.3.
    ///
    /// # Errors
    /// [`crate::PlatformError::PermissionDenied`] where the OS requires the
    /// user to confirm in a system dialog.
    fn set_default_handler(&self, assoc: &Association) -> Result<()>;

    /// Who currently owns the type.
    ///
    /// # Errors
    /// [`crate::PlatformError::Unsupported`] where the OS will not say.
    fn default_handler_of(&self, assoc: &Association) -> Result<HandlerInfo>;

    /// The environment a login shell would produce, cached after the first
    /// call. See the module docs for why this exists.
    ///
    /// # Errors
    /// [`crate::PlatformError::BackendUnavailable`] when the login shell could
    /// not be run or did not answer in time. Never blocks forever: a shell
    /// profile that prompts is a real thing that happens.
    fn login_shell_env(&self) -> Result<BTreeMap<String, String>>;

    /// The user's login shell.
    fn shell_kind(&self) -> ShellKind;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_kind_from_the_usual_spellings() {
        assert_eq!(ShellKind::from_program("/bin/zsh"), ShellKind::Zsh);
        assert_eq!(ShellKind::from_program("/usr/bin/fish"), ShellKind::Fish);
        assert_eq!(
            ShellKind::from_program(r"C:\Windows\System32\cmd.exe"),
            ShellKind::Cmd
        );
        assert_eq!(
            ShellKind::from_program("/opt/homebrew/bin/elvish"),
            ShellKind::Other("elvish".to_owned())
        );
    }

    /// 08 §6.3 — the default is never taken silently, so the convenience
    /// constructor cannot produce a `Default` role.
    #[test]
    fn associations_default_to_alternate() {
        let a = Association::alternate(".do", "Stata do-file");
        assert_eq!(a.extension, "do");
        assert_eq!(a.role, HandlerRole::Alternate);
    }
}
