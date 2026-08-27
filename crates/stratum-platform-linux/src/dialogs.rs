//! File panels, Reveal, and Open-in-browser on Linux — 08 §5.4.
//!
//! W24's acceptance: *"Linux portal file chooser with a 500 ms GTK fallback."*
//! This module is the decision; [`crate::portal`] is the portal half and the
//! shell supplies the GTK half through [`GtkFallback`].
//!
//! # What the 500 ms is, and what it is not
//!
//! It is a deadline on `org.freedesktop.portal.Desktop` **answering the method
//! call at all** — i.e. on the portal existing, being activatable, and being
//! alive. It is emphatically *not* a deadline on the user choosing a file:
//! people take minutes in a file dialog, and a chooser that vanished after half
//! a second would be a defect, not a fallback. Once the portal has returned its
//! request handle we wait for the user indefinitely.
//!
//! # Why the GTK half is injected instead of linked
//!
//! The same reason [`stratum_platform::MenuSink`] exists: the application shell
//! owns the widget toolkit. `stratum-desktop` runs on Tauri, which on Linux has
//! already initialised GTK 3 and owns the main loop, and a second GTK binding
//! in this crate would be a second authority over the same `GMainContext` —
//! `gtk_init` from a worker thread while Tauri's loop is running is undefined,
//! not merely rude. It also keeps this crate free of `gtk-sys`' `pkg-config`
//! build script, which is what makes `cargo check --target
//! x86_64-unknown-linux-gnu` possible from a machine with no GTK on it.
//!
//! With no fallback installed and no portal answering, the honest result is
//! [`PlatformError::Unsupported`] naming both halves. That is a state the UI
//! renders ("no file chooser is available in this session"), not a crash.
//!
//! # The gate is a counter, not a duration (ADR-017)
//!
//! [`DialogAttempts`] records how many times each backend was asked. The
//! property that matters — **the user is shown at most one dialog per request,
//! and a portal that answered is never second-guessed** — is a count, is
//! machine-independent, and is what `tests/dialogs.rs` asserts. The 500 ms
//! itself is a recorded constant.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use camino::{Utf8Path, Utf8PathBuf};
use stratum_platform::{
    ExternalUrl, FileDialogs, FolderRequest, OpenRequest, PlatformError, Result, SaveRequest,
};

/// How long the portal has to answer before the GTK fallback is used.
///
/// 500 ms is 08 §5.4's number. It is long enough that an `xdg-desktop-portal`
/// being D-Bus-activated from cold — a fork, an exec and a bus registration —
/// comfortably makes it, and short enough that a session with no portal at all
/// does not feel like the application hung.
pub const PORTAL_DEADLINE: Duration = Duration::from_millis(500);

/// How many times each backend was asked. See the module docs: this is the
/// gate, and the duration is merely recorded.
#[derive(Debug, Default)]
pub struct DialogAttempts {
    portal: AtomicU64,
    fallback: AtomicU64,
}

impl DialogAttempts {
    /// Times the portal was asked.
    #[must_use]
    pub fn portal(&self) -> u64 {
        self.portal.load(Ordering::Relaxed)
    }

    /// Times the GTK fallback was asked. Must never exceed [`Self::portal`]:
    /// the fallback is only ever reached through a portal attempt that failed.
    #[must_use]
    pub fn fallback(&self) -> u64 {
        self.fallback.load(Ordering::Relaxed)
    }
}

/// The `org.freedesktop.portal.Desktop` half.
///
/// A trait rather than a concrete type so the policy above can be exercised
/// against a portal that is absent, silent, or answers with a cancel, on a host
/// with no D-Bus at all.
#[async_trait::async_trait]
pub trait DesktopPortal: Send + Sync {
    /// `org.freedesktop.portal.FileChooser.OpenFile`.
    ///
    /// # Errors
    /// [`PlatformError::BackendUnavailable`] when the portal did not answer
    /// within [`PORTAL_DEADLINE`] or is not on the bus — that, and
    /// [`PlatformError::Unsupported`], are the two the fallback reacts to.
    /// [`PlatformError::Cancelled`] when the user dismissed the panel, which is
    /// an ANSWER and must not trigger the fallback.
    async fn open_files(&self, req: &OpenRequest) -> Result<Vec<Utf8PathBuf>>;

    /// `org.freedesktop.portal.FileChooser.SaveFile`.
    ///
    /// # Errors
    /// As [`DesktopPortal::open_files`].
    async fn save_file(&self, req: &SaveRequest) -> Result<Utf8PathBuf>;

    /// `OpenFile` with the `directory` option.
    ///
    /// # Errors
    /// As [`DesktopPortal::open_files`].
    async fn pick_folder(&self, req: &FolderRequest) -> Result<Utf8PathBuf>;

    /// `org.freedesktop.FileManager1.ShowItems` — the one call that *selects*
    /// the file rather than merely opening its directory.
    ///
    /// # Errors
    /// As [`DesktopPortal::open_files`].
    fn show_item_in_folder(&self, path: &Utf8Path) -> Result<()>;
}

/// The GTK `FileChooserNative` half, supplied by the application shell.
///
/// `stratum-desktop` implements this over the GTK its Tauri window already
/// owns. See the module docs for why it is not linked here.
#[async_trait::async_trait]
pub trait GtkFallback: Send + Sync {
    /// A GTK Open panel.
    ///
    /// # Errors
    /// [`PlatformError::Cancelled`] when dismissed; whatever GTK reports
    /// otherwise.
    async fn open_files(&self, req: &OpenRequest) -> Result<Vec<Utf8PathBuf>>;

    /// A GTK Save panel.
    ///
    /// # Errors
    /// As [`GtkFallback::open_files`].
    async fn save_file(&self, req: &SaveRequest) -> Result<Utf8PathBuf>;

    /// A GTK folder panel.
    ///
    /// # Errors
    /// As [`GtkFallback::open_files`].
    async fn pick_folder(&self, req: &FolderRequest) -> Result<Utf8PathBuf>;
}

/// A [`DesktopPortal`] that is not there. The default on a host with no portal
/// implementation compiled in, and the reason [`LinuxFileDialogs`] has
/// something to fall back *from* even then.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoPortal;

const NO_PORTAL: PlatformError =
    PlatformError::Unsupported("no xdg-desktop-portal is available in this session");

#[async_trait::async_trait]
impl DesktopPortal for NoPortal {
    async fn open_files(&self, _req: &OpenRequest) -> Result<Vec<Utf8PathBuf>> {
        Err(NO_PORTAL)
    }
    async fn save_file(&self, _req: &SaveRequest) -> Result<Utf8PathBuf> {
        Err(NO_PORTAL)
    }
    async fn pick_folder(&self, _req: &FolderRequest) -> Result<Utf8PathBuf> {
        Err(NO_PORTAL)
    }
    fn show_item_in_folder(&self, _path: &Utf8Path) -> Result<()> {
        Err(NO_PORTAL)
    }
}

/// [`FileDialogs`] for Linux: portal first, injected GTK second.
pub struct LinuxFileDialogs {
    portal: Arc<dyn DesktopPortal>,
    fallback: Option<Arc<dyn GtkFallback>>,
    attempts: DialogAttempts,
}

impl std::fmt::Debug for LinuxFileDialogs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LinuxFileDialogs")
            .field("has_gtk_fallback", &self.fallback.is_some())
            .field("portal_attempts", &self.attempts.portal())
            .field("fallback_attempts", &self.attempts.fallback())
            .finish()
    }
}

impl LinuxFileDialogs {
    /// With a portal and no fallback.
    #[must_use]
    pub fn new(portal: Arc<dyn DesktopPortal>) -> Self {
        Self {
            portal,
            fallback: None,
            attempts: DialogAttempts::default(),
        }
    }

    /// Attach the shell's GTK chooser.
    #[must_use]
    pub fn with_fallback(mut self, fallback: Arc<dyn GtkFallback>) -> Self {
        self.fallback = Some(fallback);
        self
    }

    /// The counters. See the module docs — this is the acceptance gate.
    #[must_use]
    pub fn attempts(&self) -> &DialogAttempts {
        &self.attempts
    }

    /// Whether a portal failure means "there is no portal here" (fall back) or
    /// "the portal answered, and this is the answer" (do not).
    ///
    /// Getting this wrong in the permissive direction is the bug that matters:
    /// treating [`PlatformError::Cancelled`] as a fallback trigger shows the
    /// user a second file dialog immediately after they pressed Escape on the
    /// first.
    #[must_use]
    pub fn should_fall_back(e: &PlatformError) -> bool {
        matches!(
            e,
            PlatformError::BackendUnavailable(_) | PlatformError::Unsupported(_)
        )
    }

    /// The error when neither backend exists. Names both halves, because
    /// "install xdg-desktop-portal-gtk" is advice the user can act on and
    /// "unsupported" is not.
    const fn nothing_available() -> PlatformError {
        PlatformError::Unsupported(
            "no file chooser is available: no xdg-desktop-portal answered and this build \
             has no GTK fallback installed",
        )
    }

    /// One `portal → maybe fallback` attempt, with the counters kept in step.
    ///
    /// Written once as a macro-free helper over two futures so that the three
    /// panel methods cannot drift apart — an `open_files` that falls back on
    /// `Cancelled` while `save_file` does not is precisely the kind of
    /// divergence that survives review.
    async fn choose<T, PF, GF>(&self, portal: PF, gtk: GF) -> Result<T>
    where
        PF: std::future::Future<Output = Result<T>>,
        GF: std::future::Future<Output = Result<T>>,
    {
        self.attempts.portal.fetch_add(1, Ordering::Relaxed);
        let err = match portal.await {
            Ok(v) => return Ok(v),
            Err(e) => e,
        };
        if !Self::should_fall_back(&err) {
            return Err(err);
        }
        if self.fallback.is_none() {
            return Err(Self::nothing_available());
        }
        self.attempts.fallback.fetch_add(1, Ordering::Relaxed);
        gtk.await
    }
}

#[async_trait::async_trait]
impl FileDialogs for LinuxFileDialogs {
    async fn open_files(&self, req: OpenRequest) -> Result<Vec<Utf8PathBuf>> {
        // Both futures are built up front and only one is polled: `choose`
        // awaits the second only after the first has failed in a way that
        // permits it.
        let portal = self.portal.clone();
        let fallback = self.fallback.clone();
        let req = &req;
        self.choose(async move { portal.open_files(req).await }, async move {
            match fallback {
                Some(f) => f.open_files(req).await,
                None => Err(NO_PORTAL),
            }
        })
        .await
    }

    async fn save_file(&self, req: SaveRequest) -> Result<Utf8PathBuf> {
        let portal = self.portal.clone();
        let fallback = self.fallback.clone();
        let req = &req;
        self.choose(async move { portal.save_file(req).await }, async move {
            match fallback {
                Some(f) => f.save_file(req).await,
                None => Err(NO_PORTAL),
            }
        })
        .await
    }

    async fn pick_folder(&self, req: FolderRequest) -> Result<Utf8PathBuf> {
        let portal = self.portal.clone();
        let fallback = self.fallback.clone();
        let req = &req;
        self.choose(async move { portal.pick_folder(req).await }, async move {
            match fallback {
                Some(f) => f.pick_folder(req).await,
                None => Err(NO_PORTAL),
            }
        })
        .await
    }

    fn reveal(&self, path: &Utf8Path) -> Result<()> {
        // Canonicalise first, for the same reason macOS does: `reveal` is
        // reachable from a link in a rendered help page, and a leading `-`
        // would otherwise be read as a flag by whatever we hand it to.
        let abs = std::fs::canonicalize(path)?;
        let abs = Utf8PathBuf::from_path_buf(abs)
            .map_err(|_| PlatformError::Unsupported("path is not valid UTF-8"))?;

        // `ShowItems` SELECTS the file, which is what Reveal means. Only if
        // that is unavailable do we degrade to opening the containing folder —
        // which is a different, weaker thing, and worth having in that order.
        match self.portal.show_item_in_folder(&abs) {
            Ok(()) => return Ok(()),
            Err(e) if !Self::should_fall_back(&e) => return Err(e),
            Err(_) => {}
        }
        let dir = abs.parent().unwrap_or(&abs);
        open_with_handler(dir.as_str())
    }

    fn open_external(&self, url: &ExternalUrl) -> Result<()> {
        // `ExternalUrl` has already rejected every scheme but https/http/mailto
        // and every string containing whitespace, quotes or control characters,
        // and `Command` does not go through a shell, so there is nothing left
        // to escape.
        open_with_handler(url.as_str())
    }
}

/// Hand `target` to the session's URL handler.
///
/// `xdg-open` first because it is the freedesktop-specified entry point and
/// respects `mimeapps.list`; `gio open` second because a GNOME-flavoured
/// container often has GIO and not `xdg-utils`. Anything else is
/// [`PlatformError::Unsupported`] rather than a silent success — a "Open in
/// browser" that does nothing and reports nothing is the worst of the three
/// possible outcomes.
fn open_with_handler(target: &str) -> Result<()> {
    let mut last: Option<PlatformError> = None;
    for argv in [["xdg-open", target], ["gio", "open"]] {
        let mut cmd = std::process::Command::new(argv[0]);
        if argv[0] == "gio" {
            cmd.arg("open").arg(target);
        } else {
            cmd.arg(target);
        }
        // Stdio is inherited by default and `xdg-open` is chatty on stderr when
        // it falls through its own handler list; that noise ends up in the
        // supervisor's log for no purpose.
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        match cmd.status() {
            Ok(s) if s.success() => return Ok(()),
            Ok(s) => {
                last = Some(PlatformError::Os {
                    code: i64::from(s.code().unwrap_or(-1)),
                    message: format!("{} {target} failed", argv[0]),
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => last = Some(PlatformError::Io(e)),
        }
    }
    Err(last.unwrap_or(PlatformError::Unsupported(
        "neither xdg-open nor gio is on PATH; no URL handler in this session",
    )))
}
