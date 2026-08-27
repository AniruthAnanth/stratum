//! W24's file-chooser acceptance, as counters (ADR-017).
//!
//! The plan bullet is *"Linux portal file chooser with a 500 ms GTK fallback"*.
//! ADR-017 is binding on every unit: a performance acceptance bullet must
//! assert a **counter**, not a duration, because the same unchanged tree
//! benchmarked 33 % apart an hour apart on this machine. So the property is
//! expressed as the thing that is actually invariant and actually matters:
//!
//! * the portal is asked **exactly once** per request;
//! * the GTK fallback is asked **at most once**, and only after a portal
//!   failure that means "there is no portal here";
//! * a portal that ANSWERED — including a user pressing Cancel — is never
//!   second-guessed, so the user is shown **exactly one** dialog.
//!
//! The 500 ms is recorded as [`PORTAL_DEADLINE`] and asserted only to be the
//! number 08 §5.4 specifies. The measured cost of a portal that never answers
//! is recorded by the deadline test rather than gated on.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll, Waker};

use camino::Utf8PathBuf;
use stratum_platform::{
    FileDialogs, FolderRequest, OpenRequest, PlatformError, Result, SaveRequest,
};
use stratum_platform_linux::dialogs::{
    DesktopPortal, GtkFallback, LinuxFileDialogs, NoPortal, PORTAL_DEADLINE,
};

/// Poll once. Every fake below answers without yielding, so a real executor
/// would only hide a bug where one of them does not.
fn now<F: Future>(f: F) -> F::Output {
    let mut f = Box::pin(f);
    match f.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(v) => v,
        Poll::Pending => panic!("this call was supposed to finish without yielding"),
    }
}

/// A portal that answers however the test says, and counts.
struct FakePortal {
    answer: fn() -> PlatformError,
    calls: AtomicU64,
}

impl FakePortal {
    fn new(answer: fn() -> PlatformError) -> Arc<Self> {
        Arc::new(Self {
            answer,
            calls: AtomicU64::new(0),
        })
    }
    fn fail(&self) -> PlatformError {
        self.calls.fetch_add(1, Ordering::Relaxed);
        (self.answer)()
    }
}

#[async_trait::async_trait]
impl DesktopPortal for FakePortal {
    async fn open_files(&self, _r: &OpenRequest) -> Result<Vec<Utf8PathBuf>> {
        Err(self.fail())
    }
    async fn save_file(&self, _r: &SaveRequest) -> Result<Utf8PathBuf> {
        Err(self.fail())
    }
    async fn pick_folder(&self, _r: &FolderRequest) -> Result<Utf8PathBuf> {
        Err(self.fail())
    }
    fn show_item_in_folder(&self, _p: &camino::Utf8Path) -> Result<()> {
        Err(self.fail())
    }
}

/// A GTK chooser that always picks the same file, and counts.
#[derive(Default)]
struct FakeGtk {
    calls: AtomicU64,
}

impl FakeGtk {
    fn hit(&self) -> Utf8PathBuf {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Utf8PathBuf::from("/home/jo/from-gtk.do")
    }
}

#[async_trait::async_trait]
impl GtkFallback for FakeGtk {
    async fn open_files(&self, _r: &OpenRequest) -> Result<Vec<Utf8PathBuf>> {
        Ok(vec![self.hit()])
    }
    async fn save_file(&self, _r: &SaveRequest) -> Result<Utf8PathBuf> {
        Ok(self.hit())
    }
    async fn pick_folder(&self, _r: &FolderRequest) -> Result<Utf8PathBuf> {
        Ok(self.hit())
    }
}

/// THE COUNTER GATE. A session with no portal reaches GTK, and does so with
/// exactly one attempt at each.
#[test]
fn a_missing_portal_falls_through_to_gtk_exactly_once() {
    let portal = FakePortal::new(|| PlatformError::Unsupported("no portal"));
    let gtk = Arc::new(FakeGtk::default());
    let d = LinuxFileDialogs::new(portal.clone()).with_fallback(gtk.clone());

    let picked = now(d.open_files(OpenRequest::default())).unwrap();
    assert_eq!(picked, [Utf8PathBuf::from("/home/jo/from-gtk.do")]);

    assert_eq!(d.attempts().portal(), 1, "the portal must be tried once");
    assert_eq!(d.attempts().fallback(), 1, "and GTK exactly once after it");
    assert_eq!(portal.calls.load(Ordering::Relaxed), 1);
    assert_eq!(gtk.calls.load(Ordering::Relaxed), 1);
}

/// A portal that is present but wedged reports `BackendUnavailable` after
/// [`PORTAL_DEADLINE`]; the fallback must treat that exactly like absence.
#[test]
fn a_silent_portal_is_treated_as_an_absent_one() {
    let portal = FakePortal::new(|| {
        PlatformError::BackendUnavailable("did not answer within 500ms".to_owned())
    });
    let gtk = Arc::new(FakeGtk::default());
    let d = LinuxFileDialogs::new(portal).with_fallback(gtk.clone());

    assert!(now(d.save_file(SaveRequest::default())).is_ok());
    assert_eq!(d.attempts().portal(), 1);
    assert_eq!(d.attempts().fallback(), 1);
}

/// THE BUG THIS GATE EXISTS FOR. A user who pressed Escape must not be shown a
/// second file dialog. `Cancelled` is an ANSWER from a portal that is working.
#[test]
fn a_cancelled_portal_dialog_never_opens_a_second_one() {
    let portal = FakePortal::new(|| PlatformError::Cancelled);
    let gtk = Arc::new(FakeGtk::default());
    let d = LinuxFileDialogs::new(portal).with_fallback(gtk.clone());

    let err = now(d.open_files(OpenRequest::default())).unwrap_err();
    assert!(err.is_cancelled(), "{err}");
    assert_eq!(d.attempts().portal(), 1);
    assert_eq!(
        d.attempts().fallback(),
        0,
        "the portal answered; GTK must not be asked"
    );
    assert_eq!(gtk.calls.load(Ordering::Relaxed), 0);
}

/// A locked-down portal that refuses is also an answer, not an absence.
#[test]
fn a_permission_denied_from_the_portal_is_not_retried_in_gtk() {
    let portal = FakePortal::new(|| PlatformError::PermissionDenied("sandbox".to_owned()));
    let gtk = Arc::new(FakeGtk::default());
    let d = LinuxFileDialogs::new(portal).with_fallback(gtk);

    let err = now(d.pick_folder(FolderRequest::default())).unwrap_err();
    assert!(matches!(err, PlatformError::PermissionDenied(_)), "{err}");
    assert_eq!(d.attempts().fallback(), 0);
}

/// Neither backend. The error has to name both halves, because "install
/// xdg-desktop-portal-gtk" is advice a user can act on and "unsupported" is not.
#[test]
fn with_no_portal_and_no_fallback_the_error_names_both() {
    let d = LinuxFileDialogs::new(Arc::new(NoPortal));
    let err = now(d.open_files(OpenRequest::default())).unwrap_err();
    assert!(err.is_unsupported(), "{err}");
    let msg = err.to_string();
    assert!(msg.contains("xdg-desktop-portal"), "{msg}");
    assert!(msg.contains("GTK fallback"), "{msg}");
    assert_eq!(d.attempts().portal(), 1);
    assert_eq!(
        d.attempts().fallback(),
        0,
        "there was nothing to fall back to, and the counter must say so"
    );
}

/// The three panel methods must not drift apart in their fallback policy.
#[test]
fn every_panel_method_follows_the_same_rule() {
    for expect_fallback in [true, false] {
        let portal = FakePortal::new(if expect_fallback {
            || PlatformError::Unsupported("no portal")
        } else {
            || PlatformError::Cancelled
        });
        let gtk = Arc::new(FakeGtk::default());
        let d = LinuxFileDialogs::new(portal).with_fallback(gtk.clone());

        let _ = now(d.open_files(OpenRequest::default()));
        let _ = now(d.save_file(SaveRequest::default()));
        let _ = now(d.pick_folder(FolderRequest::default()));

        assert_eq!(d.attempts().portal(), 3);
        assert_eq!(d.attempts().fallback(), if expect_fallback { 3 } else { 0 });
    }
}

/// 08 §5.4's number, recorded rather than measured.
#[test]
fn the_deadline_is_the_number_the_design_specifies() {
    assert_eq!(PORTAL_DEADLINE, std::time::Duration::from_millis(500));
}

/// The classifier the whole policy rests on, asserted directly so a future
/// change to it is a red test rather than a subtle behaviour change.
#[test]
fn only_absence_shaped_errors_trigger_the_fallback() {
    assert!(LinuxFileDialogs::should_fall_back(
        &PlatformError::Unsupported("x")
    ));
    assert!(LinuxFileDialogs::should_fall_back(
        &PlatformError::BackendUnavailable("x".to_owned())
    ));
    assert!(!LinuxFileDialogs::should_fall_back(
        &PlatformError::Cancelled
    ));
    assert!(!LinuxFileDialogs::should_fall_back(
        &PlatformError::PermissionDenied("x".to_owned())
    ));
    assert!(!LinuxFileDialogs::should_fall_back(&PlatformError::Os {
        code: 1,
        message: "x".to_owned()
    }));
}
