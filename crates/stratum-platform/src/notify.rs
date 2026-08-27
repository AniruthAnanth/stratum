//! User notifications and the dock/taskbar badge — 08 §5.4.
//!
//! Used sparingly and only for the case that actually needs it: a long `run`,
//! or a "Run all stale blocks" (§13), finishing while the app is in the
//! background. A statistics IDE that toasts on every command is a statistics
//! IDE people turn off notifications for.
//!
//! Every method may return [`crate::PlatformError::Unsupported`]: a headless CI
//! box has no notification daemon, an unbundled macOS build has no
//! `UNUserNotificationCenter`, and a Linux session may have no
//! `org.freedesktop.Notifications` on the bus. That is a state to render, not a
//! failure to report.

use crate::Result;

/// Whether we may post notifications.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PermissionState {
    /// We may.
    Granted,
    /// The user said no. Do not ask again; point at System Settings.
    Denied,
    /// Never asked. Ask at the moment the first long run finishes, not at
    /// launch.
    NotDetermined,
}

/// A button on a notification.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct NotificationAction {
    /// Routed back through the same command dispatcher as the keymap.
    pub id: String,
    /// The button label.
    pub label: String,
}

/// What to post.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Notification {
    /// Bold first line.
    pub title: String,
    /// A second line, on the platforms that render one.
    pub subtitle: Option<String>,
    /// The body.
    pub body: String,
    /// Play the default sound.
    pub sound: bool,
    /// Buttons. Empty on platforms whose [`NotifierCaps::actions`] is false.
    pub actions: Vec<NotificationAction>,
    /// Grouping key — one thread per session, so ten finished runs collapse.
    pub thread: Option<String>,
}

/// A posted notification, so it can be withdrawn when the user comes back to
/// the window. A string because macOS `UNNotificationRequest` identifiers and
/// Linux's D-Bus ids have nothing in common but being opaque.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct NotificationId(pub String);

/// The dock / taskbar badge.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Badge {
    /// Clear it.
    None,
    /// A number: blocks still queued.
    Count(u32),
    /// A dot, where the OS supports one and a number would be noise.
    Dot,
}

/// What this platform's notification stack can actually do. Asked before
/// building a [`Notification`], so we do not post buttons that silently vanish.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct NotifierCaps {
    /// Buttons are rendered.
    pub actions: bool,
    /// A dock/taskbar badge exists.
    pub badge: bool,
    /// A determinate progress bar in the notification itself.
    pub progress: bool,
    /// A sound can be played.
    pub sound: bool,
}

/// Post, withdraw, badge.
pub trait Notifier: Send + Sync {
    /// Ask the OS for permission, prompting the user if it has not been asked.
    ///
    /// # Errors
    /// [`crate::PlatformError::Unsupported`] where there is no permission model
    /// or no notification service at all.
    fn request_permission(&self) -> Result<PermissionState>;

    /// Post one.
    ///
    /// # Errors
    /// [`crate::PlatformError::PermissionDenied`] when the user has said no;
    /// [`crate::PlatformError::Unsupported`] with no service.
    fn notify(&self, n: &Notification) -> Result<NotificationId>;

    /// Withdraw a posted notification. Withdrawing one the user already
    /// dismissed is `Ok(())`.
    ///
    /// # Errors
    /// [`crate::PlatformError::Unsupported`] where withdrawal is impossible.
    fn withdraw(&self, id: NotificationId) -> Result<()>;

    /// Set the dock/taskbar badge.
    ///
    /// # Errors
    /// [`crate::PlatformError::Unsupported`] where there is no badge.
    fn set_badge(&self, badge: Badge) -> Result<()>;

    /// What this platform supports.
    fn capabilities(&self) -> NotifierCaps;
}
