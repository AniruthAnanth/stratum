//! How this copy of Stratum was installed — 08 §5.7, §6.2.
//!
//! Everything downstream of this answer is a policy decision the user can see:
//! whether the Install button in "Update available" is live at all
//! ([`stratum_platform::UpdateStrategy`]), whether `install_cli_shim` has any
//! work to do, and whether writing `~/.local/share/applications` is us being
//! helpful or us duplicating what the `.deb` already installed.
//!
//! It is a **pure function of the environment and the executable path** so that
//! all six shapes can be asserted from any host. Getting it wrong is not a
//! crash, it is worse: an AppImage that thinks it is a `.deb` tells the user to
//! run `apt upgrade stratum` for a package that does not exist, and a `.deb`
//! that thinks it is an AppImage tries to rewrite `/usr/bin/stratum` under a
//! package manager's feet.

use camino::{Utf8Path, Utf8PathBuf};
use stratum_platform::{Env, UpdateStrategy};

/// The installation shapes 08 §6.2 ships, plus the two a developer sees.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Packaging {
    /// Running from an AppImage. `$APPIMAGE` is the path of the image itself,
    /// which is what [`crate::updater::LinuxUpdater`] rewrites in place.
    AppImage(Utf8PathBuf),
    /// Installed by `dpkg`/`rpm` into `/usr` or `/opt`.
    SystemPackage,
    /// A Flatpak sandbox. Updates are `flatpak update`'s job, and the
    /// filesystem we can see is not the one the user installed into.
    Flatpak,
    /// A Snap. `snapd` refreshes it; `/snap/...` is read-only anyway.
    Snap,
    /// A Nix store path. Immutable by construction.
    NixStore,
    /// Anything else: `cargo run`, a tarball unpacked into `~/opt`, a CI
    /// runner. There is nothing to self-replace and no package manager to defer
    /// to, so updating is [`UpdateStrategy::Disabled`] rather than a button
    /// that fails.
    Unmanaged,
}

impl Packaging {
    /// Detect from the environment and the running executable's path.
    ///
    /// Order matters and is not arbitrary. The sandbox markers come first
    /// because inside a Flatpak the executable genuinely lives at `/app/bin`
    /// and inside a Snap at `/snap/...`, both of which would otherwise read as
    /// a system package. `$APPIMAGE` comes before the path tests because an
    /// AppImage's runtime mounts itself at `/tmp/.mount_XXXX` and its `argv[0]`
    /// says nothing useful.
    #[must_use]
    pub fn detect(env: &dyn Env, exe: Option<&Utf8Path>) -> Self {
        if env.var("FLATPAK_ID").is_some() || env.var("FLATPAK_SANDBOX_DIR").is_some() {
            return Self::Flatpak;
        }
        if env.var("SNAP").is_some() && env.var("SNAP_NAME").is_some() {
            return Self::Snap;
        }
        if let Some(image) = env.var("APPIMAGE") {
            return Self::AppImage(Utf8PathBuf::from(image));
        }
        let Some(exe) = exe else {
            return Self::Unmanaged;
        };
        let p = exe.as_str();
        if p.starts_with("/nix/store/") {
            return Self::NixStore;
        }
        if p.starts_with("/snap/") {
            return Self::Snap;
        }
        if p.starts_with("/app/") {
            return Self::Flatpak;
        }
        // `/usr/local` is deliberately NOT here. It is the directory a
        // hand-unpacked tarball goes into, no package manager owns it by
        // default, and treating it as package-managed would tell the user to
        // run an `apt upgrade` that cannot do anything.
        if p.starts_with("/usr/bin/")
            || p.starts_with("/usr/lib/")
            || p.starts_with("/usr/libexec/")
            || p.starts_with("/opt/")
        {
            return Self::SystemPackage;
        }
        Self::Unmanaged
    }

    /// The update strategy this shape implies (08 §5.7).
    ///
    /// Only the AppImage can install an update itself. Everything else either
    /// belongs to a package manager — where a silent self-update desynchronises
    /// the package database, which is user-hostile — or has no installed
    /// location to replace.
    #[must_use]
    pub const fn update_strategy(&self) -> UpdateStrategy {
        match self {
            Self::AppImage(_) => UpdateStrategy::AppImageSelfReplace,
            Self::SystemPackage | Self::Flatpak | Self::Snap | Self::NixStore => {
                UpdateStrategy::PackageManaged
            }
            Self::Unmanaged => UpdateStrategy::Disabled,
        }
    }

    /// The command line to show beside "Update available (0.4.2)". `None` for
    /// the shapes where we install the update ourselves or cannot.
    ///
    /// The exact string matters: 08 §5.7 says the UI shows the command and does
    /// nothing else, so a wrong one is the whole feature being wrong.
    #[must_use]
    pub const fn upgrade_hint(&self) -> Option<&'static str> {
        match self {
            Self::SystemPackage => Some("apt upgrade stratum   (or: dnf upgrade stratum)"),
            Self::Flatpak => Some("flatpak update dev.stratum.Stratum"),
            Self::Snap => Some("snap refresh stratum"),
            Self::NixStore => Some("nix profile upgrade stratum"),
            Self::AppImage(_) | Self::Unmanaged => None,
        }
    }

    /// Whether the packaging already put `stratum` on the user's `PATH`.
    ///
    /// `install_cli_shim` returns [`stratum_platform::PlatformError::Unsupported`]
    /// for these, which is the trait's documented answer for "the packaging
    /// already owns `PATH`" — a symlink in `~/.local/bin` that shadows
    /// `/usr/bin/stratum` is how a user ends up running last month's build
    /// without knowing it.
    #[must_use]
    pub const fn owns_path(&self) -> bool {
        matches!(self, Self::SystemPackage | Self::Flatpak | Self::Snap)
    }

    /// Whether the packaging already installed the `.desktop` file and the MIME
    /// package system-wide (08 §6.3). AppImage and unmanaged builds did not,
    /// which is why `register_file_associations` writes into
    /// `~/.local/share` — the explicit, reversible opt-in §6.3 requires.
    #[must_use]
    pub const fn owns_desktop_integration(&self) -> bool {
        matches!(self, Self::SystemPackage | Self::Flatpak | Self::Snap)
    }
}

/// One system package this crate can use at runtime, and how hard the
/// dependency is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RuntimeDependency {
    /// The Debian package name, for `.deb` `depends`/`recommends`.
    pub deb: &'static str,
    /// The RPM package name, for the `.rpm` spec's `Requires`/`Recommends`.
    pub rpm: &'static str,
    /// True when Stratum cannot start without it; false when its absence is a
    /// state this crate reports and continues from.
    pub required: bool,
    /// What breaks without it, in the words the packaging comment should use.
    pub why: &'static str,
}

/// What this crate needs from the system at runtime, split into required and
/// recommended.
///
/// **This constant exists to be transcribed, not consulted at runtime.** W24's
/// acceptance ends with *"the `.deb`/`.rpm` list it as recommended, not
/// required"*, and the file that decides it —
/// `apps/desktop/src-tauri/tauri.linux.conf.json` — is W22's under R0. So the
/// classification lives here, where the code that copes with each absence
/// lives, and the packaging unit transcribes it rather than guessing.
///
/// The reason a keyring must never be `Depends:` is not tidiness. `apt` will
/// pull `gnome-keyring` and its `gcr`/`p11-kit`/GNOME session dependencies onto
/// a KDE box, a headless server and a Docker image, to satisfy a dependency
/// this crate is explicitly designed not to need: with nothing on
/// `org.freedesktop.secrets`, [`crate::credentials::LinuxCredentials`] demotes
/// once to the encrypted file and says so. A hard dependency would make that
/// carefully-built fallback unreachable *and* make the package uninstallable
/// where it matters most.
pub const RUNTIME_DEPENDENCIES: &[RuntimeDependency] = &[
    RuntimeDependency {
        deb: "libwebkit2gtk-4.1-0",
        rpm: "webkit2gtk4.1",
        required: true,
        why: "the webview the entire UI renders in; without it nothing starts",
    },
    RuntimeDependency {
        deb: "libgtk-3-0",
        rpm: "gtk3",
        required: true,
        why: "the toolkit the webview and the window belong to",
    },
    RuntimeDependency {
        deb: "gnome-keyring",
        rpm: "gnome-keyring",
        required: false,
        why: "provides org.freedesktop.secrets. Absent, credentials fall back to \
              the AES-256-GCM file and the Settings pane says so (§22). KDE users \
              get the same interface from kwalletd and must not be made to install \
              this one",
    },
    RuntimeDependency {
        deb: "xdg-desktop-portal-gtk",
        rpm: "xdg-desktop-portal-gtk",
        required: false,
        why: "provides org.freedesktop.portal.FileChooser. Absent, file dialogs \
              fall back to the shell's GTK chooser after PORTAL_DEADLINE; a KDE \
              or wlroots session has its own portal backend instead",
    },
    RuntimeDependency {
        deb: "xdg-utils",
        rpm: "xdg-utils",
        required: false,
        why: "provides xdg-open, used by Reveal and Open-in-browser. Absent, we \
              try `gio open` and then report Unsupported",
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny [`Env`] over a literal table. `stratum-platform`'s own `Fake` is
    /// private to its test module, so this is the same shape at our use site.
    struct Fake(&'static [(&'static str, &'static str)]);

    impl Env for Fake {
        fn var(&self, key: &str) -> Option<String> {
            self.0
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| (*v).to_owned())
        }
        fn home(&self) -> Option<Utf8PathBuf> {
            Some(Utf8PathBuf::from("/home/researcher"))
        }
        fn exe_dir(&self) -> Option<Utf8PathBuf> {
            None
        }
    }

    const EMPTY: Fake = Fake(&[]);

    #[test]
    fn an_appimage_is_detected_from_its_own_variable_not_from_argv() {
        // The AppImage runtime mounts the payload under /tmp/.mount_XXXXXX, so
        // the executable path is useless and $APPIMAGE is the only truth.
        let env = Fake(&[("APPIMAGE", "/home/researcher/Apps/Stratum-0.4.2.AppImage")]);
        let p = Packaging::detect(
            &env,
            Some(Utf8Path::new("/tmp/.mount_Stratabc/usr/bin/stratum")),
        );
        assert_eq!(
            p,
            Packaging::AppImage(Utf8PathBuf::from(
                "/home/researcher/Apps/Stratum-0.4.2.AppImage"
            ))
        );
        assert_eq!(p.update_strategy(), UpdateStrategy::AppImageSelfReplace);
        assert!(p.update_strategy().can_self_install());
        assert_eq!(p.upgrade_hint(), None);
        assert!(!p.owns_path());
    }

    #[test]
    fn a_deb_install_is_package_managed_and_never_self_installs() {
        let p = Packaging::detect(&EMPTY, Some(Utf8Path::new("/usr/bin/stratum")));
        assert_eq!(p, Packaging::SystemPackage);
        assert_eq!(p.update_strategy(), UpdateStrategy::PackageManaged);
        assert!(!p.update_strategy().can_self_install());
        assert!(p.owns_path() && p.owns_desktop_integration());
    }

    /// A sandbox's own paths look exactly like a system install from inside it,
    /// so the marker variables have to win.
    #[test]
    fn a_sandbox_is_recognised_before_its_paths_are_read() {
        let flatpak = Fake(&[("FLATPAK_ID", "dev.stratum.Stratum")]);
        assert_eq!(
            Packaging::detect(&flatpak, Some(Utf8Path::new("/app/bin/stratum"))),
            Packaging::Flatpak
        );
        let snap = Fake(&[("SNAP", "/snap/stratum/42"), ("SNAP_NAME", "stratum")]);
        assert_eq!(
            Packaging::detect(&snap, Some(Utf8Path::new("/snap/stratum/42/bin/stratum"))),
            Packaging::Snap
        );
    }

    /// `/usr/local/bin` is where a hand-unpacked tarball lands and no package
    /// manager owns it. Telling that user to run `apt upgrade stratum` is
    /// advice that cannot work.
    #[test]
    fn usr_local_is_not_package_managed() {
        let p = Packaging::detect(&EMPTY, Some(Utf8Path::new("/usr/local/bin/stratum")));
        assert_eq!(p, Packaging::Unmanaged);
        assert_eq!(p.update_strategy(), UpdateStrategy::Disabled);
        assert_eq!(p.upgrade_hint(), None);
    }

    #[test]
    fn a_cargo_target_directory_is_unmanaged() {
        let p = Packaging::detect(
            &EMPTY,
            Some(Utf8Path::new(
                "/home/researcher/stratum/target/debug/stratum",
            )),
        );
        assert_eq!(p, Packaging::Unmanaged);
        assert!(!p.owns_path() && !p.owns_desktop_integration());
    }

    #[test]
    fn an_unknown_executable_path_is_unmanaged_rather_than_a_guess() {
        assert_eq!(Packaging::detect(&EMPTY, None), Packaging::Unmanaged);
    }

    /// W24's last acceptance clause, as a check rather than a comment: the
    /// keyring is RECOMMENDED. Making it `Depends:` would drag GNOME onto a
    /// KDE box and a headless server to satisfy a dependency this crate is
    /// designed not to need.
    #[test]
    fn no_optional_backend_is_ever_a_hard_package_dependency() {
        let required: Vec<&str> = RUNTIME_DEPENDENCIES
            .iter()
            .filter(|d| d.required)
            .map(|d| d.deb)
            .collect();
        assert_eq!(required, ["libwebkit2gtk-4.1-0", "libgtk-3-0"]);

        for d in RUNTIME_DEPENDENCIES {
            let optional_backend = d.deb.contains("keyring") || d.deb.contains("portal");
            assert!(
                !(optional_backend && d.required),
                "{} must be recommended, not required: {}",
                d.deb,
                d.why
            );
            // Every entry has to name a package on both distro families, or the
            // rpm spec ends up with a Debian name in it.
            assert!(!d.deb.is_empty() && !d.rpm.is_empty() && !d.why.is_empty());
        }
    }

    #[test]
    fn a_nix_store_path_defers_to_nix() {
        let p = Packaging::detect(
            &EMPTY,
            Some(Utf8Path::new("/nix/store/abc123-stratum-0.4.2/bin/stratum")),
        );
        assert_eq!(p, Packaging::NixStore);
        assert_eq!(p.update_strategy(), UpdateStrategy::PackageManaged);
        // Nix has no `apt`, and the whole point of `PackageManaged` is that the
        // string we print is one the user can actually run.
        assert!(p.upgrade_hint().is_some_and(|h| h.starts_with("nix ")));
    }
}
