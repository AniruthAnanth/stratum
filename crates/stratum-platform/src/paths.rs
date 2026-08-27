//! Directory resolution — 08 §5.2.
//!
//! A concrete struct, not a trait. Path resolution *varies* by platform but it
//! is pure computation over environment variables, so making it a trait would
//! buy nothing and would put [`crate::PlatformError::Unsupported`] on a path
//! that can never legitimately return it. As a struct it is also exhaustively
//! testable: [`Paths::resolve`] takes the [`PlatformId`] and the [`Env`] as
//! arguments, so the Windows and Linux layouts are asserted from a macOS test
//! run, months before `stratum-platform-windows` exists.
//!
//! # Why not `directories` v5
//!
//! 08 §5.2 says "uses `directories` v5". We do not, and the reason is
//! mechanical rather than aesthetic: `directories` reaches `dirs-sys`, which
//! depends on `windows-sys` when built for a Windows target, and `deny.toml`
//! restricts `windows-sys` to `stratum-platform-windows`. Taking `directories`
//! here would either break `cargo deny` on two of the three release targets or
//! force a widening of the ban list that exists to keep OS crates out of this
//! crate — while the acceptance bullet for this unit is literally "compiles for
//! all three targets with zero OS deps". The table in §5.2 is transcribed below
//! verbatim instead; it is forty lines of `match`.

use camino::{Utf8Path, Utf8PathBuf};

use crate::{PlatformError, PlatformId, Result};

/// The macOS bundle identifier, and the directory name under
/// `~/Library/Application Support` and `~/Library/Caches`.
pub const BUNDLE_ID: &str = "dev.stratum.app";
/// The human-facing product name: the `%APPDATA%\Stratum` folder and
/// `~/Library/Logs/Stratum`.
pub const PRODUCT: &str = "Stratum";
/// The lowercase XDG directory name.
pub const XDG_NAME: &str = "stratum";
/// The per-project volatile cache directory (08 §11.4). Always gitignored.
pub const PROJECT_CACHE_DIR: &str = ".stratum";

/// The environment [`Paths::resolve`] reads. Injected so the resolution is a
/// pure function of its inputs and every platform's table can be asserted from
/// any host.
pub trait Env {
    /// An environment variable, or `None` when unset **or empty**. An empty
    /// `%APPDATA%` is unset for our purposes; treating `""` as a valid base
    /// resolves the config directory to a relative path.
    fn var(&self, key: &str) -> Option<String>;
    /// The user's home directory.
    fn home(&self) -> Option<Utf8PathBuf>;
    /// The directory containing the running executable. `None` when it cannot
    /// be determined, which makes [`Paths::bundled_ado`] fall back to a
    /// relative `ado`.
    fn exe_dir(&self) -> Option<Utf8PathBuf>;
}

/// [`Env`] over the real process environment.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemEnv;

impl Env for SystemEnv {
    fn var(&self, key: &str) -> Option<String> {
        std::env::var(key).ok().filter(|v| !v.is_empty())
    }

    fn home(&self) -> Option<Utf8PathBuf> {
        // `HOME` first on every platform: a user who sets it means it. Windows
        // falls back to the pair NT actually guarantees.
        if let Some(h) = self.var("HOME") {
            return Some(Utf8PathBuf::from(h));
        }
        if let Some(p) = self.var("USERPROFILE") {
            return Some(Utf8PathBuf::from(p));
        }
        match (self.var("HOMEDRIVE"), self.var("HOMEPATH")) {
            (Some(d), Some(p)) => Some(Utf8PathBuf::from(format!("{d}{p}"))),
            _ => None,
        }
    }

    fn exe_dir(&self) -> Option<Utf8PathBuf> {
        let exe = std::env::current_exe().ok()?;
        let dir = exe.parent()?.to_path_buf();
        Utf8PathBuf::from_path_buf(dir).ok()
    }
}

/// Every directory Stratum writes to or reads from, resolved once at startup.
///
/// Handed to `stratum-runtime` as a constructor argument. Nothing below L4 ever
/// computes one of these itself (08 §5.0 rule 3).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Paths {
    id: PlatformId,
    config: Utf8PathBuf,
    data: Utf8PathBuf,
    cache: Utf8PathBuf,
    state: Utf8PathBuf,
    log: Utf8PathBuf,
    bundled_ado: Utf8PathBuf,
    personal_ado: Utf8PathBuf,
}

impl Paths {
    /// Resolve for the host platform from the real environment.
    ///
    /// # Errors
    /// [`PlatformError::BackendUnavailable`] when the home directory cannot be
    /// determined at all, which is the one input with no defensible default.
    pub fn discover() -> Result<Self> {
        Self::resolve(PlatformId::HOST, &SystemEnv)
    }

    /// Resolve for an arbitrary platform and environment. 08 §5.2's table,
    /// transcribed.
    ///
    /// # Errors
    /// [`PlatformError::BackendUnavailable`] when no home directory is
    /// available and the platform's bases are not fully specified by
    /// environment variables.
    pub fn resolve(id: PlatformId, env: &dyn Env) -> Result<Self> {
        let sep = separator(id);
        let home = || -> Result<Utf8PathBuf> {
            env.home().ok_or_else(|| {
                PlatformError::BackendUnavailable(
                    "no home directory: HOME, USERPROFILE and HOMEDRIVE+HOMEPATH are all unset"
                        .to_owned(),
                )
            })
        };

        let (config, data, cache, state, log) = match id {
            PlatformId::MacOs => {
                let h = home()?;
                let app_support = join(&h, &["Library", "Application Support", BUNDLE_ID], sep);
                (
                    app_support.clone(),
                    app_support.clone(),
                    join(&h, &["Library", "Caches", BUNDLE_ID], sep),
                    join(&app_support, &["state"], sep),
                    join(&h, &["Library", "Logs", PRODUCT], sep),
                )
            }
            PlatformId::Windows => {
                // Roaming holds config and data; Local holds anything large,
                // machine-specific or regenerable, so that a roaming profile
                // does not drag a multi-gigabyte result cache across the wire.
                let roaming = match env.var("APPDATA") {
                    Some(v) => Utf8PathBuf::from(v),
                    None => join(&home()?, &["AppData", "Roaming"], sep),
                };
                let local = match env.var("LOCALAPPDATA") {
                    Some(v) => Utf8PathBuf::from(v),
                    None => join(&home()?, &["AppData", "Local"], sep),
                };
                (
                    join(&roaming, &[PRODUCT, "config"], sep),
                    join(&roaming, &[PRODUCT, "data"], sep),
                    join(&local, &[PRODUCT, "cache"], sep),
                    join(&local, &[PRODUCT, "state"], sep),
                    join(&local, &[PRODUCT, "logs"], sep),
                )
            }
            PlatformId::Linux => {
                // The XDG basedir spec requires these to be absolute and says a
                // relative value "must be ignored"; `xdg_base` enforces it.
                let cfg = xdg_base(env, "XDG_CONFIG_HOME", &[".config"], sep, &home)?;
                let dat = xdg_base(env, "XDG_DATA_HOME", &[".local", "share"], sep, &home)?;
                let cch = xdg_base(env, "XDG_CACHE_HOME", &[".cache"], sep, &home)?;
                let st = xdg_base(env, "XDG_STATE_HOME", &[".local", "state"], sep, &home)?;
                let state = join(&st, &[XDG_NAME], sep);
                (
                    join(&cfg, &[XDG_NAME], sep),
                    join(&dat, &[XDG_NAME], sep),
                    join(&cch, &[XDG_NAME], sep),
                    state.clone(),
                    join(&state, &["logs"], sep),
                )
            }
        };

        Ok(Self {
            id,
            bundled_ado: bundled_ado(id, env.exe_dir().as_deref(), sep),
            // Stratum's own PERSONAL, under our data directory rather than at
            // Stata's classic location: two products writing one ado tree is a
            // support burden, and §15's clean state excludes PERSONAL from the
            // ado path anyway. `sysdir` reporting is the runtime's (W06).
            personal_ado: join(&data, &["ado", "personal"], sep),
            config,
            data,
            cache,
            state,
            log,
        })
    }

    /// The platform whose layout these paths follow.
    #[must_use]
    pub const fn id(&self) -> PlatformId {
        self.id
    }

    /// Settings, keymaps, layout overlays.
    #[must_use]
    pub fn config_dir(&self) -> &Utf8Path {
        &self.config
    }

    /// Durable application data: the workspace store, the estimates store.
    #[must_use]
    pub fn data_dir(&self) -> &Utf8Path {
        &self.data
    }

    /// Regenerable blobs. Safe to delete while the app is closed.
    #[must_use]
    pub fn cache_dir(&self) -> &Utf8Path {
        &self.cache
    }

    /// Logs and crash reports.
    #[must_use]
    pub fn state_dir(&self) -> &Utf8Path {
        &self.state
    }

    /// Where the rolling log files go.
    #[must_use]
    pub fn log_dir(&self) -> &Utf8Path {
        &self.log
    }

    /// The read-only ado tree shipped inside the bundle.
    ///
    /// This is a *layout* answer, not an existence check: in a `cargo run` it
    /// points next to `target/debug`, where nothing is installed. Callers that
    /// need "does it exist" must ask the filesystem.
    #[must_use]
    pub fn bundled_ado(&self) -> &Utf8Path {
        &self.bundled_ado
    }

    /// The user's writable PERSONAL sysdir.
    #[must_use]
    pub fn personal_ado(&self) -> &Utf8Path {
        &self.personal_ado
    }

    /// The per-project volatile cache, `<project_root>/.stratum` (08 §11.4).
    ///
    /// Pure: it neither creates the directory nor writes the ignore file. See
    /// [`Paths::ensure_project_cache`].
    #[must_use]
    pub fn project_cache(&self, project_root: &Utf8Path) -> Utf8PathBuf {
        join(project_root, &[PROJECT_CACHE_DIR], separator(self.id))
    }

    /// Create `<project_root>/.stratum` and, on first creation, write
    /// `.stratum/.gitignore` containing `*`.
    ///
    /// The `target/` trick: a directory that ignores itself needs no edit to
    /// the user's own `.gitignore`, so a researcher's analysis repository does
    /// the right thing without anyone remembering to configure it. The ignore
    /// file is written only when absent, so a user who edits it keeps their
    /// edit.
    ///
    /// # Errors
    /// [`PlatformError::Io`] if the directory or the ignore file cannot be
    /// written.
    pub fn ensure_project_cache(&self, project_root: &Utf8Path) -> Result<Utf8PathBuf> {
        let dir = self.project_cache(project_root);
        std::fs::create_dir_all(&dir)?;
        let ignore = dir.join(".gitignore");
        if !ignore.exists() {
            std::fs::write(&ignore, "*\n")?;
        }
        Ok(dir)
    }

    /// Create the four user-writable directories. The bundled ado tree is
    /// read-only and is never created.
    ///
    /// # Errors
    /// [`PlatformError::Io`] on the first directory that cannot be created.
    pub fn ensure_all(&self) -> Result<()> {
        for d in [
            &self.config,
            &self.data,
            &self.cache,
            &self.state,
            &self.log,
        ] {
            std::fs::create_dir_all(d)?;
        }
        Ok(())
    }
}

const fn separator(id: PlatformId) -> char {
    match id {
        PlatformId::Windows => '\\',
        PlatformId::MacOs | PlatformId::Linux => '/',
    }
}

/// Join with an EXPLICIT separator rather than `Utf8Path::join`, which uses the
/// host's. Without this the Windows table renders with `/` when it is resolved
/// from a macOS test, and the test would be asserting the wrong string.
fn join(base: &Utf8Path, parts: &[&str], sep: char) -> Utf8PathBuf {
    let mut s = base.as_str().trim_end_matches(sep).to_owned();
    for p in parts {
        s.push(sep);
        s.push_str(p);
    }
    Utf8PathBuf::from(s)
}

fn xdg_base(
    env: &dyn Env,
    var: &str,
    default_under_home: &[&str],
    sep: char,
    home: &dyn Fn() -> Result<Utf8PathBuf>,
) -> Result<Utf8PathBuf> {
    match env.var(var) {
        // "If an implementation encounters a relative path in any of these
        // variables it should consider the path invalid and ignore it."
        Some(v) if v.starts_with(sep) => Ok(Utf8PathBuf::from(v)),
        _ => Ok(join(&home()?, default_under_home, sep)),
    }
}

fn bundled_ado(id: PlatformId, exe_dir: Option<&Utf8Path>, sep: char) -> Utf8PathBuf {
    let Some(dir) = exe_dir else {
        return Utf8PathBuf::from("ado");
    };
    match id {
        // Stratum.app/Contents/MacOS/stratum -> Stratum.app/Contents/Resources/ado
        PlatformId::MacOs => match dir.parent() {
            Some(contents) => join(contents, &["Resources", "ado"], sep),
            None => join(dir, &["ado"], sep),
        },
        // The installer lays resources down beside the exe.
        PlatformId::Windows => join(dir, &["ado"], sep),
        // /usr/bin/stratum -> /usr/share/stratum/ado; an AppImage mounts the
        // same relative layout under its own root.
        PlatformId::Linux => match dir.parent() {
            Some(prefix) => join(prefix, &["share", XDG_NAME, "ado"], sep),
            None => join(dir, &["ado"], sep),
        },
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    struct Fake {
        vars: BTreeMap<String, String>,
        home: Option<Utf8PathBuf>,
        exe_dir: Option<Utf8PathBuf>,
    }

    impl Fake {
        fn unix() -> Self {
            Self {
                vars: BTreeMap::new(),
                home: Some(Utf8PathBuf::from("/Users/ada")),
                exe_dir: Some(Utf8PathBuf::from(
                    "/Applications/Stratum.app/Contents/MacOS",
                )),
            }
        }

        fn windows() -> Self {
            let mut vars = BTreeMap::new();
            vars.insert("APPDATA".into(), r"C:\Users\ada\AppData\Roaming".into());
            vars.insert("LOCALAPPDATA".into(), r"C:\Users\ada\AppData\Local".into());
            Self {
                vars,
                home: Some(Utf8PathBuf::from(r"C:\Users\ada")),
                exe_dir: Some(Utf8PathBuf::from(r"C:\Program Files\Stratum")),
            }
        }

        fn with(mut self, k: &str, v: &str) -> Self {
            self.vars.insert(k.into(), v.into());
            self
        }
    }

    impl Env for Fake {
        fn var(&self, key: &str) -> Option<String> {
            self.vars.get(key).cloned().filter(|v| !v.is_empty())
        }
        fn home(&self) -> Option<Utf8PathBuf> {
            self.home.clone()
        }
        fn exe_dir(&self) -> Option<Utf8PathBuf> {
            self.exe_dir.clone()
        }
    }

    /// 08 §5.2's table, macOS column.
    #[test]
    fn macos_table() {
        let p = Paths::resolve(PlatformId::MacOs, &Fake::unix()).unwrap();
        assert_eq!(
            p.config_dir(),
            "/Users/ada/Library/Application Support/dev.stratum.app"
        );
        assert_eq!(p.data_dir(), p.config_dir());
        assert_eq!(p.cache_dir(), "/Users/ada/Library/Caches/dev.stratum.app");
        assert_eq!(p.log_dir(), "/Users/ada/Library/Logs/Stratum");
        assert_eq!(
            p.bundled_ado(),
            "/Applications/Stratum.app/Contents/Resources/ado"
        );
    }

    /// The Windows column, asserted from a macOS test run — including the
    /// backslashes, which `Utf8Path::join` would have got wrong.
    #[test]
    fn windows_table() {
        let p = Paths::resolve(PlatformId::Windows, &Fake::windows()).unwrap();
        assert_eq!(
            p.config_dir(),
            r"C:\Users\ada\AppData\Roaming\Stratum\config"
        );
        assert_eq!(p.data_dir(), r"C:\Users\ada\AppData\Roaming\Stratum\data");
        assert_eq!(p.cache_dir(), r"C:\Users\ada\AppData\Local\Stratum\cache");
        assert_eq!(p.log_dir(), r"C:\Users\ada\AppData\Local\Stratum\logs");
        assert_eq!(p.bundled_ado(), r"C:\Program Files\Stratum\ado");
        assert!(!p.config_dir().as_str().contains('/'));
    }

    #[test]
    fn linux_xdg_defaults_and_overrides() {
        let p = Paths::resolve(PlatformId::Linux, &Fake::unix()).unwrap();
        assert_eq!(p.config_dir(), "/Users/ada/.config/stratum");
        assert_eq!(p.data_dir(), "/Users/ada/.local/share/stratum");
        assert_eq!(p.cache_dir(), "/Users/ada/.cache/stratum");
        assert_eq!(p.state_dir(), "/Users/ada/.local/state/stratum");
        assert_eq!(p.log_dir(), "/Users/ada/.local/state/stratum/logs");

        let overridden = Fake::unix().with("XDG_CONFIG_HOME", "/etc/xdg-user");
        let p = Paths::resolve(PlatformId::Linux, &overridden).unwrap();
        assert_eq!(p.config_dir(), "/etc/xdg-user/stratum");
    }

    /// The basedir spec: a relative XDG value is invalid and must be ignored.
    #[test]
    fn linux_relative_xdg_is_ignored() {
        let bad = Fake::unix().with("XDG_CACHE_HOME", "relative/cache");
        let p = Paths::resolve(PlatformId::Linux, &bad).unwrap();
        assert_eq!(p.cache_dir(), "/Users/ada/.cache/stratum");
    }

    #[test]
    fn no_home_is_backend_unavailable_not_a_panic() {
        let mut env = Fake::unix();
        env.home = None;
        let err = Paths::resolve(PlatformId::MacOs, &env).unwrap_err();
        assert!(matches!(err, PlatformError::BackendUnavailable(_)));
    }

    #[test]
    fn project_cache_is_dot_stratum() {
        let p = Paths::resolve(PlatformId::MacOs, &Fake::unix()).unwrap();
        assert_eq!(
            p.project_cache(Utf8Path::new("/w/paper")),
            "/w/paper/.stratum"
        );
    }
}
