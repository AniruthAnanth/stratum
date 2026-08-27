//! Windows shell integration — 08 §5.5.
//!
//! Three jobs that share only a registry hive: the `stratum` CLI on the user's
//! `Path`, file associations, and the environment a freshly-launched shell
//! would have.
//!
//! # The CLI "shim" is a `Path` entry, not a file
//!
//! macOS symlinks `Stratum.app/Contents/MacOS/stratum` into `/usr/local/bin`.
//! Windows has no symlink a non-elevated user can rely on, so 08 §5.5
//! prescribes the other mechanism: *"append the install dir to the **user**
//! `Path` via `HKCU\Environment` + `SendMessageTimeout(HWND_BROADCAST,
//! WM_SETTINGCHANGE, …)`"*.
//!
//! **The broadcast is not optional and is this unit's named acceptance.**
//! `HKCU\Environment` is read by the shell at logon; a process that writes it
//! and stops there has made a change that takes effect *after the user next
//! logs out*. Every terminal already open, and every terminal opened later in
//! the same session, still has the old `Path`. The broadcast is what makes
//! Explorer re-read the hive and re-derive the environment it hands to new
//! processes. `SMTO_ABORTIFHUNG` with a short timeout, because
//! `HWND_BROADCAST` reaches every top-level window on the desktop and one hung
//! application must not hang the Settings pane.
//!
//! # `REG_EXPAND_SZ` must survive
//!
//! A user's `Path` legitimately contains `%USERPROFILE%\.cargo\bin`. Reading it
//! expanded and writing it back as `REG_SZ` silently freezes those entries to
//! whatever they resolved to for *us* — which, for a machine with roaming
//! profiles or a different user, is simply wrong. Every read here preserves the
//! raw value and the value *type*, and the edit is a list operation on
//! unexpanded strings.

use std::collections::BTreeMap;

use stratum_platform::{Association, HandlerRole, PlatformError, Result};

/// `HKCU\Environment` — the per-user environment block.
pub const HKCU_ENVIRONMENT: &str = "Environment";
/// The machine-wide environment block under `HKEY_LOCAL_MACHINE`.
pub const HKLM_ENVIRONMENT: &str = r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment";
/// Per-user class registrations. Writing here never needs elevation, which is
/// why [`stratum_platform::InstallScope::User`] is the only scope that works
/// for associations without a UAC prompt.
pub const HKCU_CLASSES: &str = r"Software\Classes";
/// Where Explorer records the user's explicit choice of default handler.
pub const FILE_EXTS: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts";
/// The `Path` value name. Spelled exactly as the registry spells it.
pub const PATH_VALUE: &str = "Path";

/// The ProgID for an extension: `Stratum.do`, `Stratum.dta`.
///
/// A ProgID is a machine-wide identifier in a shared namespace; prefixing every
/// one of ours with the product name is what stops us colliding with Stata's
/// own registrations on a machine that has both — which is most of them.
#[must_use]
pub fn progid(extension: &str) -> String {
    format!(
        "{}.{}",
        stratum_platform::paths::PRODUCT,
        extension.trim_start_matches('.')
    )
}

/// The `shell\open\command` value for a ProgID.
///
/// `"exe" "%1"` with both quoted. An unquoted `%1` is the classic Windows
/// association bug: a file at `C:\My Data\wave 2.do` arrives as two arguments,
/// and the application opens `C:\My`.
#[must_use]
pub fn open_command(exe: &str) -> String {
    format!("\"{exe}\" \"%1\"")
}

/// Split a `Path`-shaped value into entries, dropping the empty segments a
/// hand-edited `Path` collects. Entries are NOT expanded — see the module docs.
#[must_use]
pub fn path_entries(value: &str) -> Vec<&str> {
    value
        .split(';')
        .map(str::trim)
        .filter(|e| !e.is_empty())
        .collect()
}

/// Whether two `Path` entries name the same directory.
///
/// Windows paths are case-insensitive, a trailing `\` is not significant, and
/// an entry containing a space is often quoted in a hand-edited `Path`. Two of
/// those three differences are what make a naive `==` add the same directory to
/// the `Path` on every launch.
#[must_use]
pub fn same_entry(a: &str, b: &str) -> bool {
    fn norm(s: &str) -> String {
        s.trim()
            .trim_matches('"')
            .trim_end_matches('\\')
            .to_ascii_lowercase()
    }
    norm(a) == norm(b)
}

/// The result of a `Path` edit.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PathEdit {
    /// The value to write, or `None` when the `Path` already says what we
    /// wanted. `None` means **do not write and do not broadcast**: rewriting
    /// an unchanged `Path` still costs every process on the desktop a
    /// `WM_SETTINGCHANGE`.
    pub value: Option<String>,
    /// **The counter (ADR-017).** Entries compared. One pass over the list:
    /// this equals the number of entries in the input, never a multiple of it.
    pub entries_scanned: usize,
}

/// Put `dir` at the front of `value`, unless it is already somewhere in it.
///
/// The front, not the back: a user who has an older `stratum` from a package
/// manager should get the one they just installed. Idempotent by construction —
/// see [`same_entry`] for what "already in it" means.
#[must_use]
pub fn path_with_dir(value: &str, dir: &str) -> PathEdit {
    let entries = path_entries(value);
    let scanned = entries.len();
    if entries.iter().any(|e| same_entry(e, dir)) {
        return PathEdit {
            value: None,
            entries_scanned: scanned,
        };
    }
    let mut out = String::with_capacity(value.len() + dir.len() + 1);
    out.push_str(dir);
    for e in entries {
        out.push(';');
        out.push_str(e);
    }
    PathEdit {
        value: Some(out),
        entries_scanned: scanned,
    }
}

/// Remove every entry naming `dir` from `value`.
#[must_use]
pub fn path_without_dir(value: &str, dir: &str) -> PathEdit {
    let entries = path_entries(value);
    let scanned = entries.len();
    let kept: Vec<&str> = entries
        .into_iter()
        .filter(|e| !same_entry(e, dir))
        .collect();
    if kept.len() == scanned {
        return PathEdit {
            value: None,
            entries_scanned: scanned,
        };
    }
    PathEdit {
        value: Some(kept.join(";")),
        entries_scanned: scanned,
    }
}

/// Expand `%NAME%` references the way `ExpandEnvironmentStringsW` does.
///
/// An unresolvable name is left **verbatim, percent signs and all**, which is
/// what Windows does and is the only safe answer: silently dropping
/// `%JAVA_HOME%` from a `Path` would delete a directory the user put there.
#[must_use]
pub fn expand(value: &str, lookup: &BTreeMap<String, String>) -> String {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('%') {
            Some(end) => {
                let name = &after[..end];
                // Case-insensitive, because environment variable names are.
                let hit = lookup
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case(name))
                    .map(|(_, v)| v.clone());
                match hit {
                    Some(v) => out.push_str(&v),
                    None => {
                        out.push('%');
                        out.push_str(name);
                        out.push('%');
                    }
                }
                rest = &after[end + 1..];
            }
            None => {
                // A lone `%` is a literal.
                out.push('%');
                out.push_str(after);
                return out;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Compose the environment a newly-launched process would get, from the two
/// registry blocks.
///
/// The user block wins for every variable **except `Path`, which Windows
/// concatenates** — machine first, then user. Overwriting instead would hand a
/// child an environment with `C:\Windows\system32` missing, and a do-file's
/// `shell` command would fail on every built-in tool. That asymmetry is real
/// Windows behaviour and is the reason this is a named function with a test
/// rather than a `BTreeMap::extend`.
#[must_use]
pub fn merge_environment(
    machine: &BTreeMap<String, String>,
    user: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut out = machine.clone();
    for (k, v) in user {
        if k.eq_ignore_ascii_case(PATH_VALUE) {
            // Find the machine's Path under whatever casing it used.
            let existing = out
                .iter()
                .find(|(mk, _)| mk.eq_ignore_ascii_case(PATH_VALUE))
                .map(|(mk, mv)| (mk.clone(), mv.clone()));
            match existing {
                Some((mk, mv)) if !mv.is_empty() => {
                    out.insert(mk, format!("{};{v}", mv.trim_end_matches(';')));
                }
                Some((mk, _)) => {
                    out.insert(mk, v.clone());
                }
                None => {
                    out.insert(k.clone(), v.clone());
                }
            }
        } else {
            out.insert(k.clone(), v.clone());
        }
    }
    out
}

/// The refusal every `set_default_handler` returns on Windows.
///
/// Windows 8 removed the ability to set a file type's default handler
/// programmatically; the `UserChoice` key is hash-protected and Explorer
/// invalidates a hand-written one. The user must pick us in Settings →
/// Default apps. That is [`PlatformError::PermissionDenied`] exactly as the
/// trait documents — *"where the OS requires the user to confirm in a system
/// dialog"* — and not `Unsupported`, because the capability exists, it simply
/// is not ours to exercise.
///
/// # Errors
/// Always. It exists to name the reason once.
pub fn refuse_default_handler(assoc: &Association) -> PlatformError {
    if assoc.role != HandlerRole::Default {
        return PlatformError::Unsupported(
            "set_default_handler needs HandlerRole::Default; registering an alternate handler \
             is what register_file_associations does",
        );
    }
    PlatformError::PermissionDenied(format!(
        ".{} cannot be claimed programmatically on Windows 8 and later: the UserChoice key is \
         hash-protected. Stratum is registered under Open With; the user chooses the default in \
         Settings > Apps > Default apps.",
        assoc.extension
    ))
}

/// Validate an association before it reaches the registry.
///
/// A `\` or `/` in an extension would let a caller write a key outside
/// `Software\Classes`, and the extension reaches this layer from
/// `Association::alternate` — which is called with data, not with a literal, in
/// the Settings pane.
///
/// # Errors
/// [`PlatformError::Unsupported`] for an empty extension or one containing a
/// path separator, a wildcard or a control character.
pub fn check_extension(extension: &str) -> Result<()> {
    let ext = extension.trim_start_matches('.');
    if ext.is_empty() {
        return Err(PlatformError::Unsupported("an empty file extension"));
    }
    if ext
        .chars()
        .any(|c| c.is_control() || matches!(c, '\\' | '/' | '*' | '?' | ':' | '"' | '.'))
    {
        return Err(PlatformError::Unsupported(
            "a file extension may not contain a separator, a wildcard or a control character",
        ));
    }
    Ok(())
}

#[cfg(target_os = "windows")]
pub use sys::WindowsShell;

#[cfg(target_os = "windows")]
mod sys {
    use std::collections::BTreeMap;

    use camino::Utf8PathBuf;
    use stratum_platform::{
        Association, HandlerInfo, HandlerRole, InstallScope, PlatformError, Result,
        ShellIntegration, ShellKind, ShimStatus,
    };
    use windows::Win32::Foundation::{LPARAM, WPARAM};
    use windows::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, REG_EXPAND_SZ, REG_NONE, REG_SZ,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        SendMessageTimeoutW, HWND_BROADCAST, SMTO_ABORTIFHUNG, WM_SETTINGCHANGE,
    };

    use super::{
        check_extension, expand, merge_environment, open_command, path_with_dir, path_without_dir,
        progid, refuse_default_handler, FILE_EXTS, HKCU_CLASSES, HKCU_ENVIRONMENT,
        HKLM_ENVIRONMENT, PATH_VALUE,
    };
    use crate::process::Probe;
    use crate::registry;
    use crate::win;

    /// How long we will wait for the desktop to acknowledge the environment
    /// broadcast. `HWND_BROADCAST` reaches every top-level window; one that is
    /// not pumping messages must not stall the Settings pane, which is what
    /// `SMTO_ABORTIFHUNG` plus a short timeout buys.
    const BROADCAST_TIMEOUT_MS: u32 = 1000;

    /// [`ShellIntegration`] for Windows.
    #[derive(Debug, Default)]
    pub struct WindowsShell {
        env: Probe<BTreeMap<String, String>>,
    }

    impl WindowsShell {
        /// Construct. Reads no registry until asked.
        #[must_use]
        pub const fn new() -> Self {
            Self { env: Probe::new() }
        }

        /// The directory the CLI lives in, which is what goes on the `Path`.
        fn install_dir() -> Result<Utf8PathBuf> {
            let exe = std::env::current_exe()?;
            let exe = Utf8PathBuf::from_path_buf(exe)
                .map_err(|_| PlatformError::Unsupported("executable path is not valid UTF-8"))?;
            exe.parent()
                .map(camino::Utf8Path::to_path_buf)
                .ok_or(PlatformError::Unsupported(
                    "the executable has no parent directory",
                ))
        }

        /// `<install dir>\stratum.exe`, the CLI beside the GUI.
        fn cli_path() -> Result<Utf8PathBuf> {
            Ok(Self::install_dir()?.join("stratum.exe"))
        }

        fn hive(scope: InstallScope) -> (HKEY, &'static str) {
            match scope {
                InstallScope::User => (HKEY_CURRENT_USER, HKCU_ENVIRONMENT),
                InstallScope::System => (HKEY_LOCAL_MACHINE, HKLM_ENVIRONMENT),
            }
        }
    }

    /// `SendMessageTimeout(HWND_BROADCAST, WM_SETTINGCHANGE, 0, "Environment")`.
    ///
    /// Without this the `Path` edit does not take effect until the user logs
    /// out. See the module docs.
    fn broadcast_environment_change() {
        let topic = win::wide("Environment");
        // SAFETY: `topic` is a NUL-terminated UTF-16 buffer that outlives the
        // call; `lParam` for WM_SETTINGCHANGE is documented as a string naming
        // the section that changed. Failure is not actionable — the value is
        // already written and the change will land at next logon — so the
        // result is deliberately dropped rather than turned into an error the
        // caller cannot do anything about.
        unsafe {
            SendMessageTimeoutW(
                HWND_BROADCAST,
                WM_SETTINGCHANGE,
                WPARAM(0),
                LPARAM(topic.as_ptr() as isize),
                SMTO_ABORTIFHUNG,
                BROADCAST_TIMEOUT_MS,
                None,
            );
        }
    }

    impl ShellIntegration for WindowsShell {
        fn install_cli_shim(&self, scope: InstallScope) -> Result<Utf8PathBuf> {
            let dir = Self::install_dir()?;
            let (hive, subkey) = Self::hive(scope);
            let current = registry::read_value(hive, subkey, PATH_VALUE)?;
            let (raw, ty) = current.unwrap_or_else(|| (String::new(), REG_EXPAND_SZ));
            let edit = path_with_dir(&raw, dir.as_str());
            if let Some(value) = edit.value {
                // The value TYPE is preserved: an existing REG_EXPAND_SZ stays
                // one, so `%USERPROFILE%\...` entries keep working.
                registry::write_string(hive, subkey, PATH_VALUE, &value, ty)?;
                broadcast_environment_change();
            }
            Self::cli_path()
        }

        fn uninstall_cli_shim(&self, scope: InstallScope) -> Result<()> {
            let dir = Self::install_dir()?;
            let (hive, subkey) = Self::hive(scope);
            let Some((raw, ty)) = registry::read_value(hive, subkey, PATH_VALUE)? else {
                // No Path value at all: the caller asked for a state.
                return Ok(());
            };
            let edit = path_without_dir(&raw, dir.as_str());
            if let Some(value) = edit.value {
                registry::write_string(hive, subkey, PATH_VALUE, &value, ty)?;
                broadcast_environment_change();
            }
            Ok(())
        }

        fn shim_status(&self) -> Result<ShimStatus> {
            let dir = Self::install_dir()?;
            let cli = Self::cli_path()?;
            // The process's own PATH, which is what a child would actually
            // search — distinct from the hive, which is what a *new* shell
            // would get. A shim that is in the hive but not yet in this
            // session is installed and not yet usable, and the UI has to be
            // able to say so.
            let live = std::env::var("PATH").unwrap_or_default();
            let on_path = super::path_entries(&live)
                .iter()
                .any(|e| super::same_entry(e, dir.as_str()));

            for scope in [InstallScope::System, InstallScope::User] {
                let (hive, subkey) = Self::hive(scope);
                let Some((raw, _)) = registry::read_value(hive, subkey, PATH_VALUE)? else {
                    continue;
                };
                if super::path_entries(&raw)
                    .iter()
                    .any(|e| super::same_entry(e, dir.as_str()))
                {
                    return Ok(ShimStatus {
                        installed_at: Some(cli.clone()),
                        scope: Some(scope),
                        on_path,
                        points_at_us: cli.is_file(),
                    });
                }
            }
            Ok(ShimStatus {
                installed_at: None,
                scope: None,
                on_path,
                points_at_us: false,
            })
        }

        fn register_file_associations(&self, assoc: &[Association]) -> Result<()> {
            let exe = Self::cli_path()?;
            let gui = Self::install_dir()?.join("Stratum.exe");
            // Open the GUI when it is there, the CLI otherwise: double-clicking
            // a `.do` file should open the IDE, not print to a console that
            // closes.
            let target = if gui.is_file() { gui } else { exe };

            for a in assoc {
                check_extension(&a.extension)?;
                if a.role != HandlerRole::Alternate {
                    // 08 §6.3: we never take over `.do`/`.dta` silently.
                    return Err(PlatformError::Unsupported(
                        "register_file_associations only ever registers an Alternate handler; \
                         becoming the default is set_default_handler's explicit user action",
                    ));
                }
                let prog = progid(&a.extension);
                let ext = format!(".{}", a.extension.trim_start_matches('.'));

                registry::write_string(
                    HKEY_CURRENT_USER,
                    &format!(r"{HKCU_CLASSES}\{prog}"),
                    "",
                    &a.description,
                    REG_SZ,
                )?;
                registry::write_string(
                    HKEY_CURRENT_USER,
                    &format!(r"{HKCU_CLASSES}\{prog}\shell\open\command"),
                    "",
                    &open_command(target.as_str()),
                    REG_SZ,
                )?;
                // The Open With list. An empty REG_NONE value whose *name* is
                // the ProgID is the documented shape; the value carries no data.
                registry::write_empty(
                    HKEY_CURRENT_USER,
                    &format!(r"{HKCU_CLASSES}\{ext}\OpenWithProgids"),
                    &prog,
                    REG_NONE,
                )?;
            }
            Ok(())
        }

        fn set_default_handler(&self, assoc: &Association) -> Result<()> {
            Err(refuse_default_handler(assoc))
        }

        fn default_handler_of(&self, assoc: &Association) -> Result<HandlerInfo> {
            check_extension(&assoc.extension)?;
            let ext = format!(".{}", assoc.extension.trim_start_matches('.'));
            let ours = progid(&assoc.extension);

            // Explorer's recorded user choice wins over the class default,
            // which is the order the shell itself resolves in.
            let choice = registry::read_value(
                HKEY_CURRENT_USER,
                &format!(r"{FILE_EXTS}\{ext}\UserChoice"),
                "ProgId",
            )?
            .map(|(v, _)| v);
            let handler = match choice {
                Some(v) if !v.is_empty() => Some(v),
                _ => {
                    registry::read_value(HKEY_CURRENT_USER, &format!(r"{HKCU_CLASSES}\{ext}"), "")?
                        .map(|(v, _)| v)
                        .filter(|v| !v.is_empty())
                }
            };
            Ok(HandlerInfo {
                is_us: handler
                    .as_deref()
                    .is_some_and(|h| h.eq_ignore_ascii_case(&ours)),
                handler_id: handler,
            })
        }

        fn login_shell_env(&self) -> Result<BTreeMap<String, String>> {
            // The Windows analogue of running the login shell. There is no
            // profile script to source: the environment a new process gets is
            // composed by the shell from these two registry blocks, so reading
            // them is *more* faithful than spawning `cmd` would be, and it
            // cannot hang on a profile that prompts.
            self.env.get(|| {
                let machine =
                    registry::read_all(HKEY_LOCAL_MACHINE, HKLM_ENVIRONMENT).unwrap_or_default();
                let user =
                    registry::read_all(HKEY_CURRENT_USER, HKCU_ENVIRONMENT).unwrap_or_default();
                if machine.is_empty() && user.is_empty() {
                    return Err(PlatformError::BackendUnavailable(
                        "neither environment block in the registry could be read".to_owned(),
                    ));
                }
                let merged = merge_environment(&machine, &user);
                // Expand against the merged block itself, which is what the
                // shell does: `%SystemRoot%` in the user block resolves against
                // the machine block's value.
                Ok(merged
                    .iter()
                    .map(|(k, v)| (k.clone(), expand(v, &merged)))
                    .collect())
            })
        }

        fn shell_kind(&self) -> ShellKind {
            // `ComSpec` is the one the OS itself guarantees. A user who has
            // made PowerShell their shell sets neither, so `cmd` is the honest
            // default rather than a guess at what they prefer.
            match std::env::var("COMSPEC").ok().filter(|s| !s.is_empty()) {
                Some(p) => ShellKind::from_program(&p),
                None => ShellKind::Cmd,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    /// THE COUNTER (ADR-017). One pass over the `Path`, and — the part that
    /// matters — **no write and therefore no `WM_SETTINGCHANGE` broadcast when
    /// nothing changed**. `HWND_BROADCAST` reaches every top-level window on
    /// the desktop; doing it on every launch of the Settings pane is a
    /// user-visible stutter in other people's applications.
    #[test]
    fn installing_twice_scans_once_and_writes_nothing_the_second_time() {
        let before = r"C:\Windows\system32;C:\Windows";
        let first = path_with_dir(before, r"C:\Program Files\Stratum");
        assert_eq!(first.entries_scanned, 2);
        let after = first.value.expect("the directory was not there");
        assert_eq!(
            after,
            r"C:\Program Files\Stratum;C:\Windows\system32;C:\Windows"
        );

        let second = path_with_dir(&after, r"C:\Program Files\Stratum");
        assert_eq!(second.value, None, "a second install must not rewrite Path");
        assert_eq!(second.entries_scanned, 3);
    }

    /// Every one of these is a real spelling found in a real user's `Path`, and
    /// every one of them defeats `entries.contains(&dir)`.
    #[test]
    fn an_entry_is_recognised_across_case_quoting_and_trailing_separators() {
        for existing in [
            r"c:\program files\stratum",
            r"C:\Program Files\Stratum\",
            "\"C:\\Program Files\\Stratum\"",
            r"  C:\Program Files\Stratum  ",
        ] {
            let e = path_with_dir(existing, r"C:\Program Files\Stratum");
            assert_eq!(e.value, None, "{existing}");
        }
    }

    #[test]
    fn uninstall_removes_every_spelling_and_leaves_the_rest_alone() {
        let before = r"C:\Windows;c:\program files\stratum\;C:\Other;C:\Program Files\Stratum";
        let e = path_without_dir(before, r"C:\Program Files\Stratum");
        assert_eq!(e.value.unwrap(), r"C:\Windows;C:\Other");
        assert_eq!(e.entries_scanned, 4);

        // Removing what is not there is not a write.
        assert_eq!(path_without_dir(r"C:\Windows", r"C:\Nope").value, None);
    }

    /// Empty segments are dropped on the way through, because a `Path` that
    /// ends in `;` is extremely common and a re-emitted empty entry means "the
    /// current directory" to some tools.
    #[test]
    fn empty_segments_do_not_survive_an_edit() {
        let e = path_with_dir(r"C:\Windows;;;", r"C:\S");
        assert_eq!(e.value.unwrap(), r"C:\S;C:\Windows");
        assert_eq!(e.entries_scanned, 1);
    }

    /// The whole reason the edit works on unexpanded strings: writing the
    /// expanded form back would freeze another user's home directory into this
    /// user's `Path`.
    #[test]
    fn unexpanded_entries_pass_through_untouched() {
        let before = r"%USERPROFILE%\.cargo\bin;%SystemRoot%\system32";
        let e = path_with_dir(before, r"C:\Program Files\Stratum");
        assert_eq!(
            e.value.unwrap(),
            r"C:\Program Files\Stratum;%USERPROFILE%\.cargo\bin;%SystemRoot%\system32"
        );
    }

    #[test]
    fn expansion_is_case_insensitive_and_leaves_unknown_names_verbatim() {
        let env = map(&[("SystemRoot", r"C:\Windows"), ("USERPROFILE", r"C:\U\ada")]);
        assert_eq!(
            expand(r"%systemroot%\system32", &env),
            r"C:\Windows\system32"
        );
        assert_eq!(expand(r"%USERPROFILE%\bin", &env), r"C:\U\ada\bin");
        // Silently dropping this would delete a directory the user put there.
        assert_eq!(expand(r"%JAVA_HOME%\bin", &env), r"%JAVA_HOME%\bin");
        assert_eq!(expand("100% done", &env), "100% done");
        assert_eq!(expand("no percents", &env), "no percents");
        assert_eq!(expand("", &env), "");
    }

    /// Windows concatenates the two `Path` values and overwrites everything
    /// else. Getting this backwards hands a do-file's `shell` command an
    /// environment with no `system32` in it.
    #[test]
    fn path_is_concatenated_machine_first_while_other_variables_are_overwritten() {
        let machine = map(&[
            ("Path", r"C:\Windows\system32;C:\Windows"),
            ("OS", "Windows_NT"),
            ("TEMP", r"C:\Windows\TEMP"),
        ]);
        let user = map(&[
            ("Path", r"C:\U\ada\bin"),
            ("TEMP", r"C:\U\ada\AppData\Local\Temp"),
        ]);
        let merged = merge_environment(&machine, &user);

        assert_eq!(
            merged["Path"],
            r"C:\Windows\system32;C:\Windows;C:\U\ada\bin"
        );
        assert_eq!(merged["TEMP"], r"C:\U\ada\AppData\Local\Temp");
        assert_eq!(merged["OS"], "Windows_NT");
    }

    /// The registry does not promise a casing, and a `PATH`/`Path` mismatch
    /// that produced two entries would be invisible until a child could not
    /// find `cmd.exe`.
    #[test]
    fn a_differently_cased_path_value_does_not_produce_two_entries() {
        let machine = map(&[("PATH", r"C:\Windows")]);
        let user = map(&[("Path", r"C:\U\bin")]);
        let merged = merge_environment(&machine, &user);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged["PATH"], r"C:\Windows;C:\U\bin");
    }

    #[test]
    fn a_user_path_with_no_machine_path_is_not_prefixed_with_a_separator() {
        let merged = merge_environment(&map(&[]), &map(&[("Path", r"C:\U\bin")]));
        assert_eq!(merged["Path"], r"C:\U\bin");
        let merged = merge_environment(&map(&[("Path", "")]), &map(&[("Path", r"C:\U\bin")]));
        assert_eq!(merged["Path"], r"C:\U\bin");
    }

    /// `.do` files live in directories with spaces in them. An unquoted `%1`
    /// opens `C:\My`.
    #[test]
    fn the_open_command_quotes_both_the_exe_and_the_argument() {
        assert_eq!(
            open_command(r"C:\Program Files\Stratum\Stratum.exe"),
            "\"C:\\Program Files\\Stratum\\Stratum.exe\" \"%1\""
        );
    }

    /// ProgIDs share one machine-wide namespace with Stata's own, and most of
    /// our users have both installed.
    #[test]
    fn progids_are_product_prefixed() {
        assert_eq!(progid("do"), "Stratum.do");
        assert_eq!(progid(".dta"), "Stratum.dta");
    }

    /// The extension reaches this layer from the Settings pane as data. A `\`
    /// in it would write a key outside `Software\Classes`.
    #[test]
    fn an_extension_that_could_escape_the_classes_key_is_refused() {
        for bad in [
            "",
            ".",
            r"do\..\..\Run",
            "do/evil",
            "d*",
            "do:stream",
            "do.bak",
        ] {
            assert!(check_extension(bad).is_err(), "{bad}");
        }
        for good in ["do", ".do", "dta", "smcl"] {
            assert!(check_extension(good).is_ok(), "{good}");
        }
    }

    /// `PermissionDenied`, not `Unsupported`: the capability exists on Windows,
    /// it just belongs to the user rather than to us. The UI shows different
    /// affordances for the two.
    #[test]
    fn claiming_the_default_handler_is_the_users_action_not_ours() {
        let mut a = Association::alternate("do", "Stata do-file");
        assert!(refuse_default_handler(&a).is_unsupported());

        a.role = HandlerRole::Default;
        let err = refuse_default_handler(&a);
        assert!(
            matches!(err, PlatformError::PermissionDenied(ref m) if m.contains("Default apps")),
            "{err}"
        );
    }
}
