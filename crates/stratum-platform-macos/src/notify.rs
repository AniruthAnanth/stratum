//! User notifications and the dock badge — 08 §5.4.
//!
//! `UNUserNotificationCenter` is only available to a process with a bundle
//! identifier, and asking for it without one raises an Objective-C exception
//! that aborts rather than returning an error. Every method therefore begins
//! with the [`crate::bundle`] check and returns
//! [`PlatformError::Unsupported`] when it fails — which is the honest answer
//! for `cargo run`, `cargo test` and a headless CI box, and is why
//! `Unsupported` is a first-class return in this layer rather than a panic
//! waiting to happen.

use std::sync::atomic::{AtomicU64, Ordering};

use block2::RcBlock;
use objc2_foundation::{NSArray, NSString};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNAuthorizationStatus, UNMutableNotificationContent,
    UNNotificationRequest, UNNotificationSettings, UNNotificationSound, UNUserNotificationCenter,
};
use stratum_platform::{
    Badge, Notification, NotificationId, Notifier, NotifierCaps, PermissionState, PlatformError,
    Result,
};

use crate::bundle;

const NOT_BUNDLED: PlatformError =
    PlatformError::Unsupported("notifications need an app bundle; run the packaged Stratum.app");

/// [`Notifier`] for macOS.
#[derive(Debug, Default)]
pub struct MacosNotifier {
    /// Monotonic suffix, so two notifications posted in the same millisecond
    /// still get distinct identifiers and `withdraw` stays precise.
    seq: AtomicU64,
}

impl MacosNotifier {
    /// Construct.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            seq: AtomicU64::new(0),
        }
    }

    fn center() -> Result<objc2::rc::Retained<UNUserNotificationCenter>> {
        if !bundle::is_bundled() {
            return Err(NOT_BUNDLED);
        }
        // SAFETY: guarded by the bundle check above, which is exactly the
        // precondition `currentNotificationCenter` documents.
        Ok(UNUserNotificationCenter::currentNotificationCenter())
    }
}

impl Notifier for MacosNotifier {
    fn request_permission(&self) -> Result<PermissionState> {
        let center = Self::center()?;
        let (tx, rx) = std::sync::mpsc::channel::<bool>();

        let handler = RcBlock::new(
            move |granted: objc2::runtime::Bool, _err: *mut objc2_foundation::NSError| {
                let _ = tx.send(granted.as_bool());
            },
        );
        center.requestAuthorizationWithOptions_completionHandler(
            UNAuthorizationOptions::Alert
                | UNAuthorizationOptions::Sound
                | UNAuthorizationOptions::Badge,
            &handler,
        );

        // The completion handler runs on a framework queue, not on ours, so a
        // bounded wait here cannot deadlock the caller. A user staring at the
        // system prompt is the normal reason this takes seconds.
        match rx.recv_timeout(std::time::Duration::from_secs(60)) {
            Ok(true) => Ok(PermissionState::Granted),
            Ok(false) => Ok(PermissionState::Denied),
            Err(_) => Ok(PermissionState::NotDetermined),
        }
    }

    fn notify(&self, n: &Notification) -> Result<NotificationId> {
        let center = Self::center()?;

        let content = UNMutableNotificationContent::new();
        content.setTitle(&NSString::from_str(&n.title));
        content.setBody(&NSString::from_str(&n.body));
        if let Some(sub) = &n.subtitle {
            content.setSubtitle(&NSString::from_str(sub));
        }
        if n.sound {
            content.setSound(Some(&UNNotificationSound::defaultSound()));
        }
        if let Some(thread) = &n.thread {
            content.setThreadIdentifier(&NSString::from_str(thread));
        }

        let id = format!(
            "dev.stratum.app.{}",
            self.seq.fetch_add(1, Ordering::Relaxed)
        );
        let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
            &NSString::from_str(&id),
            &content,
            // No trigger: deliver now.
            None,
        );
        // A nil completion handler is documented as valid.
        center.addNotificationRequest_withCompletionHandler(&request, None);
        Ok(NotificationId(id))
    }

    fn withdraw(&self, id: NotificationId) -> Result<()> {
        let center = Self::center()?;
        let ids = NSArray::from_retained_slice(&[NSString::from_str(&id.0)]);
        // Withdrawing an identifier the user already dismissed is a documented
        // no-op on both selectors.
        center.removeDeliveredNotificationsWithIdentifiers(&ids);
        center.removePendingNotificationRequestsWithIdentifiers(&ids);
        Ok(())
    }

    fn set_badge(&self, badge: Badge) -> Result<()> {
        if !bundle::is_bundled() {
            return Err(NOT_BUNDLED);
        }
        crate::dock::set_badge_label(match badge {
            Badge::None => None,
            Badge::Count(0) => None,
            Badge::Count(n) => Some(n.to_string()),
            // The dock tile has no dot; a bullet is the conventional stand-in.
            Badge::Dot => Some("\u{2022}".to_owned()),
        })
    }

    fn capabilities(&self) -> NotifierCaps {
        NotifierCaps {
            // Actions exist on macOS but need a UNNotificationCategory
            // registered at launch, which is the shell's job and not wired yet.
            actions: false,
            badge: true,
            // No determinate progress in a UNNotification.
            progress: false,
            sound: true,
        }
    }
}

/// Read the current authorization state without prompting. Not part of the
/// trait; the Settings pane uses it to render "Notifications: denied — enable
/// in System Settings" without triggering a prompt as a side effect.
///
/// # Errors
/// [`PlatformError::Unsupported`] when unbundled.
pub fn authorization_status() -> Result<PermissionState> {
    let center = MacosNotifier::center()?;
    let (tx, rx) = std::sync::mpsc::channel::<isize>();
    let handler = RcBlock::new(
        move |settings: core::ptr::NonNull<UNNotificationSettings>| {
            // SAFETY: the framework hands us a live, non-null settings object.
            let status = unsafe { settings.as_ref().authorizationStatus() };
            let _ = tx.send(status.0);
        },
    );
    center.getNotificationSettingsWithCompletionHandler(&handler);

    match rx.recv_timeout(std::time::Duration::from_secs(5)) {
        Ok(s) if s == UNAuthorizationStatus::Denied.0 => Ok(PermissionState::Denied),
        Ok(s) if s == UNAuthorizationStatus::NotDetermined.0 => Ok(PermissionState::NotDetermined),
        Ok(_) => Ok(PermissionState::Granted),
        Err(_) => Err(PlatformError::BackendUnavailable(
            "the notification service did not answer".to_owned(),
        )),
    }
}
