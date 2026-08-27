//! Shell integration on Linux — 08 §5.5, §6.3.
//!
//! Three jobs with nothing in common except that they talk to the user's
//! *environment* rather than to a window: the `stratum` CLI shim, file
//! associations, and the login-shell environment.
//!
//! # The shim is packaging-dependent, and refusing is the right answer twice
//!
//! A `.deb`, an `.rpm`, a Flatpak and a Snap all put `stratum` on `PATH`
//! themselves. Writing `~/.local/bin/stratum` on top of one of those creates a
//! symlink that **shadows** the packaged binary — `~/.local/bin` precedes
//! `/usr/bin` on every distro's default `PATH` — so the user keeps running the
//! build that was installed the day they clicked the button, through every
//! `apt upgrade` afterwards. [`ShellIntegration::install_cli_shim`] returns
//! [`PlatformError::Unsupported`] there, which is the trait's own documented
//! answer for "the packaging already owns `PATH`".
//!
//! An AppImage gets a **script**, not a symlink: launching the AppImage
//! directly starts the GUI, and a `stratum` on `PATH` that opens a window is
//! not a CLI.
//!
//! # `login_shell_env` on Linux, where people assume it is a macOS problem
//!
//! It is not. A GUI session started by GDM or SDDM runs the application from a
//! systemd user unit whose environment comes from `~/.config/environment.d` and
//! `systemd --user`, *not* from `~/.profile`, `~/.bash_profile` or
//! `~/.zprofile`. A researcher whose `PATH` gains `~/.local/bin`, a conda
//! prefix or a `pyenv` shim in `~/.profile` has those in a terminal and not in
//! the launcher, so a do-file that shells out to `python` fails only inside the
//! IDE. One login shell, once, cached.

use std::collections::BTreeMap;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use camino::{Utf8Path, Utf8PathBuf};
use stratum_platform::{
    Association, Env, HandlerInfo, HandlerRole, InstallScope, PlatformError, Result,
    ShellIntegration, ShellKind, ShimStatus,
};

use crate::mime::{self, MimeApps};
use crate::packaging::Packaging;

/// How long we will wait for a login shell to print its environment. A profile
/// that takes longer than this is a profile that is waiting for something, and
/// blocking app startup on it is worse than falling back.
const LOGIN_SHELL_TIMEOUT: Duration = Duration::from_secs(3);

/// The XDG base directories this module writes into.
///
/// Resolved once, from the environment, as plain data — so every path below is
/// assertable against a temporary directory rather than against the developer's
/// own `~`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct XdgDirs {
    /// `$XDG_CONFIG_HOME`, default `~/.config`. Holds `mimeapps.list`.
    pub config_home: Utf8PathBuf,
    /// `$XDG_DATA_HOME`, default `~/.local/share`. Holds `applications/` and
    /// `mime/packages/`.
    pub data_home: Utf8PathBuf,
    /// `$XDG_DATA_DIRS`, default `/usr/local/share:/usr/share`. Read-only, and
    /// searched after `data_home` when resolving who handles a type.
    pub data_dirs: Vec<Utf8PathBuf>,
    /// `~/.local/bin`, where a user-scope shim goes.
    pub user_bin: Utf8PathBuf,
    /// `/usr/local/bin`, where a system-scope shim goes. A field rather than a
    /// constant so `tests/shell.rs` can point it somewhere writable — a test
    /// that reads the real `/usr/local/bin` passes or fails depending on what
    /// else the developer has installed.
    pub system_bin: Utf8PathBuf,
}

impl XdgDirs {
    /// Resolve from an environment.
    ///
    /// # Errors
    /// [`PlatformError::BackendUnavailable`] when neither the XDG variables nor
    /// `HOME` say where the user's directories are, which is the one input with
    /// no defensible default.
    pub fn resolve(env: &dyn Env) -> Result<Self> {
        let home = || -> Result<Utf8PathBuf> {
            env.home().ok_or_else(|| {
                PlatformError::BackendUnavailable(
                    "neither HOME nor the XDG base directory variables are set".to_owned(),
                )
            })
        };
        // The spec is explicit that a relative value is invalid and must be
        // ignored, which is not pedantry: `XDG_DATA_HOME=.local/share` would
        // otherwise put the user's desktop entry wherever we happened to be
        // launched from. "Absolute" is the Base Directory spec's notion — a
        // leading `/` — not the build host's: this module is Linux semantics
        // wherever it compiles, and `Utf8Path::is_absolute("/x")` is false
        // when these paths are unit-tested on a Windows host.
        let absolute = |v: Option<String>| -> Option<Utf8PathBuf> {
            v.map(Utf8PathBuf::from)
                .filter(|p| p.as_str().starts_with('/'))
        };
        let config_home = match absolute(env.var("XDG_CONFIG_HOME")) {
            Some(p) => p,
            None => home()?.join(".config"),
        };
        let data_home = match absolute(env.var("XDG_DATA_HOME")) {
            Some(p) => p,
            None => home()?.join(".local/share"),
        };
        let data_dirs = env
            .var("XDG_DATA_DIRS")
            .map(|v| {
                v.split(':')
                    .filter(|s| !s.is_empty())
                    .map(Utf8PathBuf::from)
                    // Same POSIX notion of absolute as `absolute` above.
                    .filter(|p| p.as_str().starts_with('/'))
                    .collect::<Vec<_>>()
            })
            .filter(|v: &Vec<Utf8PathBuf>| !v.is_empty())
            .unwrap_or_else(|| {
                vec![
                    Utf8PathBuf::from("/usr/local/share"),
                    Utf8PathBuf::from("/usr/share"),
                ]
            });
        let user_bin = home()?.join(".local/bin");
        Ok(Self {
            config_home,
            data_home,
            data_dirs,
            user_bin,
            system_bin: Utf8PathBuf::from("/usr/local/bin"),
        })
    }

    /// `$XDG_CONFIG_HOME/mimeapps.list`, the file that decides defaults.
    #[must_use]
    pub fn mimeapps(&self) -> Utf8PathBuf {
        self.config_home.join("mimeapps.list")
    }

    /// `$XDG_DATA_HOME/applications/dev.stratum.Stratum.desktop`.
    #[must_use]
    pub fn desktop_entry(&self) -> Utf8PathBuf {
        self.data_home.join("applications").join(mime::DESKTOP_FILE)
    }

    /// `$XDG_DATA_HOME/mime/packages/dev.stratum.stratum.xml`.
    #[must_use]
    pub fn mime_package(&self) -> Utf8PathBuf {
        self.data_home
            .join("mime/packages")
            .join(mime::MIME_PACKAGE_FILE)
    }
}

/// [`ShellIntegration`] for Linux.
#[derive(Debug)]
pub struct LinuxShell {
    dirs: XdgDirs,
    packaging: Packaging,
    /// This build's executable, or the AppImage that contains it.
    exe: Option<Utf8PathBuf>,
    shell: String,
    login_env: OnceLock<BTreeMap<String, String>>,
}

impl LinuxShell {
    /// Construct from resolved inputs.
    #[must_use]
    pub fn new(dirs: XdgDirs, packaging: Packaging, exe: Option<Utf8PathBuf>) -> Self {
        Self {
            dirs,
            packaging,
            exe,
            shell: String::new(),
            login_env: OnceLock::new(),
        }
    }

    /// Construct from the real environment.
    ///
    /// # Errors
    /// As [`XdgDirs::resolve`].
    pub fn discover(env: &dyn Env) -> Result<Self> {
        let exe = std::env::current_exe()
            .ok()
            .and_then(|p| Utf8PathBuf::from_path_buf(p).ok());
        let packaging = Packaging::detect(env, exe.as_deref());
        let mut me = Self::new(XdgDirs::resolve(env)?, packaging, exe);
        me.shell = env.var("SHELL").unwrap_or_default();
        Ok(me)
    }

    /// The `stratum` a shim should reach.
    ///
    /// For an AppImage that is the image itself, invoked through the shim
    /// script; for anything else it is the CLI binary beside the desktop
    /// executable, which is where Tauri's `externalBin` puts it (08 §6.1).
    fn shim_target(&self) -> Result<Utf8PathBuf> {
        if let Packaging::AppImage(image) = &self.packaging {
            return Ok(image.clone());
        }
        let exe = self
            .exe
            .clone()
            .ok_or(PlatformError::Unsupported("this build has no known path"))?;
        if let Some(dir) = exe.parent() {
            let cli = dir.join("stratum");
            if cli != exe && cli.is_file() {
                return Ok(cli);
            }
        }
        Ok(exe)
    }

    fn shim_path(&self, scope: InstallScope) -> Utf8PathBuf {
        match scope {
            InstallScope::System => self.dirs.system_bin.join("stratum"),
            InstallScope::User => self.dirs.user_bin.join("stratum"),
        }
    }

    /// Read `mimeapps.list`, or an empty one.
    fn mimeapps(&self) -> Result<MimeApps> {
        match std::fs::read_to_string(self.dirs.mimeapps()) {
            Ok(text) => Ok(MimeApps::parse(&text)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(MimeApps::default()),
            Err(e) => Err(PlatformError::Io(e)),
        }
    }

    fn write_mimeapps(&self, apps: &MimeApps) -> Result<()> {
        write_file(&self.dirs.mimeapps(), apps.to_text().as_bytes())
    }
}

impl ShellIntegration for LinuxShell {
    fn install_cli_shim(&self, scope: InstallScope) -> Result<Utf8PathBuf> {
        if self.packaging.owns_path() {
            // See the module docs: a shim here shadows the packaged binary and
            // freezes the user on today's build forever.
            return Err(PlatformError::Unsupported(
                "this packaging already puts `stratum` on PATH; a shim would shadow it",
            ));
        }
        let target = self.shim_target()?;
        let link = self.shim_path(scope);
        if let Some(dir) = link.parent() {
            std::fs::create_dir_all(dir).map_err(|e| elevate(&e, dir))?;
        }
        if link.symlink_metadata().is_ok() {
            std::fs::remove_file(&link).map_err(|e| elevate(&e, &link))?;
        }

        if matches!(self.packaging, Packaging::AppImage(_)) {
            // A script, not a symlink: running the AppImage opens the GUI, and
            // `--cli` is how its entry point selects the headless binary.
            let script = format!(
                "#!/bin/sh\n\
                 # Written by Stratum. Safe to delete.\n\
                 exec {} --cli \"$@\"\n",
                shell_quote(target.as_str())
            );
            write_file(&link, script.as_bytes())?;
            set_executable(&link)?;
            return Ok(link);
        }
        symlink(&target, &link)?;
        Ok(link)
    }

    fn uninstall_cli_shim(&self, scope: InstallScope) -> Result<()> {
        let link = self.shim_path(scope);
        let Ok(meta) = link.symlink_metadata() else {
            // Already absent: the caller asked for a state.
            return Ok(());
        };
        // A symlink is ours; so is a small script whose first lines say so.
        // Anything else at that path belongs to another installer and removing
        // it is not ours to do.
        let ours = meta.file_type().is_symlink()
            || std::fs::read_to_string(&link).is_ok_and(|s| s.contains("# Written by Stratum."));
        if !ours {
            return Err(PlatformError::PermissionDenied(format!(
                "{link} was not created by Stratum and will not be removed"
            )));
        }
        std::fs::remove_file(&link).map_err(|e| elevate(&e, &link))
    }

    fn shim_status(&self) -> Result<ShimStatus> {
        let target = self.shim_target().ok();
        let path_dirs: Vec<String> = std::env::var("PATH")
            .unwrap_or_default()
            .split(':')
            .map(str::to_owned)
            .collect();

        for scope in [InstallScope::System, InstallScope::User] {
            let link = self.shim_path(scope);
            if link.symlink_metadata().is_err() {
                continue;
            }
            let points_at_us = match std::fs::read_link(&link) {
                Ok(p) => Utf8PathBuf::from_path_buf(p)
                    .ok()
                    .zip(target.as_ref())
                    .is_some_and(|(actual, want)| &actual == want),
                // Not a symlink: the AppImage script. It points at us if it
                // names the image we are running from.
                Err(_) => std::fs::read_to_string(&link)
                    .is_ok_and(|s| target.as_ref().is_some_and(|t| s.contains(t.as_str()))),
            };
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

    fn register_file_associations(&self, assoc: &[Association]) -> Result<()> {
        if self.packaging.owns_desktop_integration() {
            // The `.deb`/`.rpm`/Flatpak already installed the desktop entry and
            // the MIME package system-wide (08 §6.1). A second copy in
            // `~/.local/share` gives the user two launcher icons.
            return Err(PlatformError::Unsupported(
                "this packaging installs the desktop entry and MIME types itself",
            ));
        }
        // Validated BEFORE anything is written. Rejecting halfway through
        // leaves a desktop entry and a MIME package on disk for a call that
        // reported failure, and the user then has a launcher icon they did not
        // agree to.
        if assoc.iter().any(|a| a.role != HandlerRole::Alternate) {
            // §6.3: registering never takes the default. That is
            // `set_default_handler`, and only from an explicit user action.
            return Err(PlatformError::Unsupported(
                "register_file_associations only ever registers HandlerRole::Alternate",
            ));
        }

        let exec = self.shim_target()?;
        write_file(
            &self.dirs.desktop_entry(),
            mime::desktop_entry(&exec).as_bytes(),
        )?;
        write_file(
            &self.dirs.mime_package(),
            mime::mime_package_xml().as_bytes(),
        )?;

        let mut apps = self.mimeapps()?;
        for a in assoc {
            let Some(m) = mime::mime_for_extension(&a.extension) else {
                continue;
            };
            apps.add_association(m, mime::DESKTOP_FILE);
        }
        // The scheme handler is not an extension and has no `Association`, but
        // it is the thing that makes `stratum://` OAuth callbacks work at all.
        apps.add_association(mime::MIME_SCHEME, mime::DESKTOP_FILE);
        self.write_mimeapps(&apps)?;

        refresh_caches(&self.dirs);
        Ok(())
    }

    fn set_default_handler(&self, assoc: &Association) -> Result<()> {
        if assoc.role != HandlerRole::Default {
            return Err(PlatformError::Unsupported(
                "set_default_handler needs HandlerRole::Default",
            ));
        }
        let m = mime::mime_for_extension(&assoc.extension).ok_or(PlatformError::Unsupported(
            "Stratum does not claim this file extension",
        ))?;
        let mut apps = self.mimeapps()?;
        apps.set_default(m, mime::DESKTOP_FILE);
        self.write_mimeapps(&apps)?;
        refresh_caches(&self.dirs);
        Ok(())
    }

    fn default_handler_of(&self, assoc: &Association) -> Result<HandlerInfo> {
        let m = mime::mime_for_extension(&assoc.extension).ok_or(PlatformError::Unsupported(
            "Stratum does not claim this file extension",
        ))?;
        // Search order is the freedesktop one: the user's file wins, then the
        // system files in `XDG_DATA_DIRS` order. Reading only the user's file
        // would report "nothing handles .do" on a machine where Stata's
        // packaged entry does.
        let mut candidates = vec![self.dirs.mimeapps()];
        candidates.push(self.dirs.data_home.join("applications/mimeapps.list"));
        candidates.extend(
            self.dirs
                .data_dirs
                .iter()
                .map(|d| d.join("applications/mimeapps.list")),
        );

        for path in candidates {
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            if let Some(handler) = MimeApps::parse(&text).default_for(m) {
                return Ok(HandlerInfo {
                    is_us: handler == mime::DESKTOP_FILE,
                    handler_id: Some(handler.to_owned()),
                });
            }
        }
        Ok(HandlerInfo {
            handler_id: None,
            is_us: false,
        })
    }

    fn login_shell_env(&self) -> Result<BTreeMap<String, String>> {
        if let Some(cached) = self.login_env.get() {
            return Ok(cached.clone());
        }
        let env = read_login_shell_env(&self.shell_program())?;
        Ok(self.login_env.get_or_init(|| env).clone())
    }

    fn shell_kind(&self) -> ShellKind {
        ShellKind::from_program(&self.shell_program())
    }
}

impl LinuxShell {
    /// `$SHELL`, or the near-universal Linux default. Never an empty string.
    fn shell_program(&self) -> String {
        if !self.shell.is_empty() {
            return self.shell.clone();
        }
        std::env::var("SHELL")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "/bin/bash".to_owned())
    }
}

/// `EACCES`/`EPERM`/`EROFS` become [`PlatformError::PermissionDenied`] rather
/// than a raw IO error: "install the shim system-wide" failing for lack of
/// admin rights is an outcome with a UI affordance, not a crash.
fn elevate(e: &std::io::Error, path: &Utf8Path) -> PlatformError {
    match e.kind() {
        std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::ReadOnlyFilesystem => {
            PlatformError::PermissionDenied(format!("{path}: {e}"))
        }
        _ => PlatformError::Os {
            code: e.raw_os_error().unwrap_or(-1).into(),
            message: format!("{path}: {e}"),
        },
    }
}

/// Create parents and write.
fn write_file(path: &Utf8Path, bytes: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| elevate(&e, dir))?;
    }
    std::fs::write(path, bytes).map_err(|e| elevate(&e, path))
}

#[cfg(unix)]
fn set_executable(path: &Utf8Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .map_err(|e| elevate(&e, path))
}

#[cfg(not(unix))]
fn set_executable(_path: &Utf8Path) -> Result<()> {
    Err(PlatformError::Unsupported(
        "file modes are a Unix concept; this build cannot install a shim",
    ))
}

#[cfg(unix)]
fn symlink(target: &Utf8Path, link: &Utf8Path) -> Result<()> {
    std::os::unix::fs::symlink(target, link).map_err(|e| elevate(&e, link))
}

#[cfg(not(unix))]
fn symlink(_target: &Utf8Path, _link: &Utf8Path) -> Result<()> {
    Err(PlatformError::Unsupported(
        "this build cannot create a Unix symlink",
    ))
}

/// Single-quote for a POSIX shell. Only used for the AppImage shim's `exec`
/// line, and only because an AppImage genuinely can live in
/// `~/My Apps/Stratum.AppImage`.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Rebuild the desktop and MIME caches. Best effort by design: `xdg-utils` and
/// `shared-mime-info` are not installed everywhere, the files we wrote are
/// already correct, and the desktop picks them up on its next scan regardless.
/// Failing the whole registration because a cache tool is missing would be the
/// wrong shape.
fn refresh_caches(dirs: &XdgDirs) {
    let jobs: [(&str, &str); 2] = [
        ("update-desktop-database", "applications"),
        ("update-mime-database", "mime"),
    ];
    for (program, subdir) in jobs {
        let _ = std::process::Command::new(program)
            .arg(dirs.data_home.join(subdir).as_str())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
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
        // `env -0` without an absolute path: on Linux it is `/usr/bin/env` on a
        // merged-/usr distro and `/bin/env` elsewhere, and the login shell has
        // just resolved `PATH` for us.
        .args(["-l", "-c", "env -0"])
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

/// Parse `env -0` output. Crate-visible so the test can feed it a fixture
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

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake(&'static [(&'static str, &'static str)]);

    impl Env for Fake {
        fn var(&self, key: &str) -> Option<String> {
            self.0
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| (*v).to_owned())
        }
        fn home(&self) -> Option<Utf8PathBuf> {
            Some(Utf8PathBuf::from("/home/jo"))
        }
        fn exe_dir(&self) -> Option<Utf8PathBuf> {
            None
        }
    }

    #[test]
    fn xdg_defaults_match_the_base_directory_specification() {
        let d = XdgDirs::resolve(&Fake(&[]));
        assert!(d.is_ok());
        if let Ok(d) = d {
            assert_eq!(d.config_home, "/home/jo/.config");
            assert_eq!(d.data_home, "/home/jo/.local/share");
            assert_eq!(d.data_dirs, ["/usr/local/share", "/usr/share"]);
            assert_eq!(d.mimeapps(), "/home/jo/.config/mimeapps.list");
            assert_eq!(
                d.desktop_entry(),
                "/home/jo/.local/share/applications/dev.stratum.Stratum.desktop"
            );
        }
    }

    /// The spec says a relative value is invalid and must be ignored. It is not
    /// pedantry: honouring it would write the user's desktop entry relative to
    /// whatever directory the launcher happened to start us in.
    #[test]
    fn a_relative_xdg_variable_is_ignored_rather_than_joined() {
        let d = XdgDirs::resolve(&Fake(&[
            ("XDG_DATA_HOME", ".local/share"),
            ("XDG_DATA_DIRS", "share:/usr/share"),
        ]));
        assert!(d.is_ok());
        if let Ok(d) = d {
            assert_eq!(d.data_home, "/home/jo/.local/share");
            assert_eq!(d.data_dirs, ["/usr/share"]);
        }
    }

    #[test]
    fn xdg_variables_are_honoured_when_absolute() {
        let d = XdgDirs::resolve(&Fake(&[
            ("XDG_CONFIG_HOME", "/run/user/1000/config"),
            ("XDG_DATA_HOME", "/run/user/1000/data"),
        ]));
        assert!(d.is_ok());
        if let Ok(d) = d {
            assert_eq!(d.mimeapps(), "/run/user/1000/config/mimeapps.list");
            assert_eq!(
                d.mime_package(),
                "/run/user/1000/data/mime/packages/dev.stratum.stratum.xml"
            );
        }
    }

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

    #[test]
    fn an_appimage_path_with_a_space_survives_the_shim_script() {
        assert_eq!(
            shell_quote("/home/jo/My Apps/S.AppImage"),
            "'/home/jo/My Apps/S.AppImage'"
        );
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
    }
}
