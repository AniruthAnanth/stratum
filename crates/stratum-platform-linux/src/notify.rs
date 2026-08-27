//! `org.freedesktop.Notifications` and the launcher badge — 08 §5.4.
//!
//! Used sparingly and only for the case that needs it: a long `run`, or "Run
//! all stale blocks" (§13), finishing while the window is in the background.
//!
//! # There is no permission model, and saying so is the honest answer
//!
//! macOS has `UNUserNotificationCenter` authorization and Windows has a
//! Settings toggle; freedesktop has neither. A session either has a
//! notification daemon on the bus or it does not.
//! [`Notifier::request_permission`] therefore probes for the service and
//! answers [`PermissionState::Granted`] or
//! [`PlatformError::Unsupported`] — it never prompts, because there is nothing
//! to prompt with, and returning `NotDetermined` forever would make a caller
//! that waits for a decision wait for one that cannot arrive.
//!
//! # The badge is `com.canonical.Unity.LauncherEntry`
//!
//! It is the only badge protocol Linux has. Plasma, Dash-to-Dock, Latte, elementary
//! and Unity itself all consume it; GNOME Shell without an extension does not,
//! and there is no way to detect that from inside the application — the signal
//! is fire-and-forget, with no reply and no subscriber count. So
//! [`NotifierCaps::badge`] reports whether we can *emit* it, which is what we
//! actually know, and the UI must not present the badge as guaranteed.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::OnceLock;

use stratum_platform::{
    Badge, Notification, NotificationId, Notifier, NotifierCaps, PermissionState, PlatformError,
    Result,
};
use zbus::zvariant::Value;

use crate::bus;

const NOTIFY_BUS: &str = "org.freedesktop.Notifications";
const NOTIFY_PATH: &str = "/org/freedesktop/Notifications";
const NOTIFY_IFACE: &str = "org.freedesktop.Notifications";

/// The launcher-entry protocol. The path is ours to choose; the `app_uri` is
/// what identifies us, and it must name the installed `.desktop` file or every
/// consumer ignores the signal.
const LAUNCHER_IFACE: &str = "com.canonical.Unity.LauncherEntry";
const LAUNCHER_PATH: &str = "/dev/stratum/app";

/// What we call ourselves in the notification. Matches
/// [`crate::mime::DESKTOP_FILE`] minus the extension, which is what lets the
/// daemon find our icon.
const APP_NAME: &str = "Stratum";
const APP_ICON: &str = "dev.stratum.Stratum";

/// `expire_timeout = -1`: let the daemon decide. A run that finished is worth a
/// glance, not a notification the user has to dismiss.
const EXPIRE_DEFAULT: i32 = -1;

const NO_DAEMON: PlatformError =
    PlatformError::Unsupported("no org.freedesktop.Notifications daemon in this session");

/// [`Notifier`] for Linux.
#[derive(Debug, Default)]
pub struct LinuxNotifier {
    /// Probed once. See [`LinuxNotifier::capabilities`] for why a negative
    /// result is cached too.
    caps: OnceLock<NotifierCaps>,
    /// The last id we posted, so [`Notification::thread`] can replace rather
    /// than stack — ten finished runs must not be ten rows in the shade.
    last_id: AtomicU32,
}

impl LinuxNotifier {
    /// Construct. Touches no bus until the first call.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            caps: OnceLock::new(),
            last_id: AtomicU32::new(0),
        }
    }

    fn proxy() -> Result<zbus::blocking::Proxy<'static>> {
        let conn = bus::session_blocking()?;
        zbus::blocking::Proxy::new(&conn, NOTIFY_BUS, NOTIFY_PATH, NOTIFY_IFACE).map_err(|e| {
            // A proxy is constructed without a round trip, so this failing is a
            // malformed name rather than an absent service.
            bus::classify(&e)
        })
    }

    /// Ask the daemon what it supports, once.
    ///
    /// The absence is cached along with the presence: a session with no daemon
    /// must not pay a D-Bus round trip every time the UI decides whether to
    /// offer a notification setting. See [`crate::bus`] for the same reasoning
    /// applied to the connection itself.
    fn probe(&self) -> NotifierCaps {
        // The badge does NOT go through the notification daemon. It is a
        // broadcast signal on the session bus, so it survives the daemon being
        // absent entirely — a KDE session with a task-manager badge and no
        // `org.freedesktop.Notifications` is a real configuration, and
        // reporting `badge: false` there would hide a feature that works. Only
        // the bus has to exist; whether anything is listening is not
        // observable, per the module docs.
        let badge = crate::bus::session_blocking().is_ok();
        let without_daemon = NotifierCaps {
            badge,
            ..NotifierCaps::default()
        };

        let Ok(proxy) = Self::proxy() else {
            return without_daemon;
        };
        let Ok(caps) = proxy.call::<_, _, Vec<String>>("GetCapabilities", &()) else {
            return without_daemon;
        };
        let has = |c: &str| caps.iter().any(|x| x == c);
        NotifierCaps {
            actions: has("actions"),
            badge,
            // `x-canonical-private-synchronous` progress bars are a Unity
            // extension nothing implements any more.
            progress: false,
            sound: has("sound"),
        }
    }
}

impl Notifier for LinuxNotifier {
    fn request_permission(&self) -> Result<PermissionState> {
        // Presence IS permission on this platform.
        let proxy = Self::proxy()?;
        proxy
            .call::<_, _, (String, String, String, String)>("GetServerInformation", &())
            .map_err(|e| match bus::classify(&e) {
                // Distinguish "no daemon" from "the daemon errored": the first
                // is Unsupported, which the UI hides an affordance for.
                PlatformError::BackendUnavailable(_) => NO_DAEMON,
                other => other,
            })?;
        Ok(PermissionState::Granted)
    }

    fn notify(&self, n: &Notification) -> Result<NotificationId> {
        let proxy = Self::proxy()?;

        // Actions are `[id, label, id, label, …]`, flat. Sending them to a
        // daemon that does not advertise `actions` makes them silently vanish,
        // so we do not send them at all — the caller asked `capabilities()`.
        let mut actions: Vec<String> = Vec::new();
        if self.capabilities().actions {
            for a in &n.actions {
                actions.push(a.id.clone());
                actions.push(a.label.clone());
            }
        }

        let mut hints: HashMap<&str, Value<'_>> = HashMap::new();
        // The desktop entry the daemon should attribute this to. Without it
        // GNOME shows "Unknown application" and no icon.
        hints.insert("desktop-entry", Value::from(APP_ICON));
        if n.sound {
            // The freedesktop sound naming spec's id for "a task finished",
            // which is what every one of our notifications is.
            hints.insert("sound-name", Value::from("complete"));
        } else {
            hints.insert("suppress-sound", Value::from(true));
        }

        // A thread means "replace the previous one from this thread". The
        // freedesktop protocol has no thread key, only `replaces_id`, so the
        // grouping is expressed as replacement — which is the behaviour §13
        // wants anyway: one row that keeps up to date, not ten rows.
        let replaces = if n.thread.is_some() {
            self.last_id.load(Ordering::Relaxed)
        } else {
            0
        };

        // The subtitle has nowhere to go in this protocol; folding it into the
        // body keeps the information rather than dropping it silently.
        let body = match &n.subtitle {
            Some(sub) if !sub.is_empty() => format!("{sub}\n{}", n.body),
            _ => n.body.clone(),
        };

        let id: u32 = proxy
            .call(
                "Notify",
                &(
                    APP_NAME,
                    replaces,
                    APP_ICON,
                    n.title.as_str(),
                    body.as_str(),
                    actions,
                    hints,
                    EXPIRE_DEFAULT,
                ),
            )
            .map_err(|e| bus::classify(&e))?;
        self.last_id.store(id, Ordering::Relaxed);
        Ok(NotificationId(id.to_string()))
    }

    fn withdraw(&self, id: NotificationId) -> Result<()> {
        let Ok(numeric) = id.0.parse::<u32>() else {
            // An id we did not mint. Nothing to withdraw, and erroring would
            // make a caller that lost track of an id unable to recover.
            return Ok(());
        };
        let proxy = Self::proxy()?;
        // Closing a notification the user already dismissed is a documented
        // no-op.
        proxy
            .call::<_, _, ()>("CloseNotification", &numeric)
            .map_err(|e| bus::classify(&e))
    }

    fn set_badge(&self, badge: Badge) -> Result<()> {
        let conn = bus::session_blocking()?;
        let (count, visible) = match badge {
            Badge::None => (0i64, false),
            Badge::Count(0) => (0, false),
            Badge::Count(n) => (i64::from(n), true),
            // The protocol has no dot. A count of 1 is the conventional
            // stand-in and is what every consumer renders as a single mark.
            Badge::Dot => (1, true),
        };
        let mut props: HashMap<&str, Value<'_>> = HashMap::new();
        props.insert("count", Value::from(count));
        props.insert("count-visible", Value::from(visible));

        // A broadcast signal: no destination, no reply, and no way to know
        // whether anything consumed it. That is the protocol, not a shortcut.
        conn.emit_signal(
            None::<&str>,
            LAUNCHER_PATH,
            LAUNCHER_IFACE,
            "Update",
            &(
                format!("application://{}", crate::mime::DESKTOP_FILE),
                props,
            ),
        )
        .map_err(|e| bus::classify(&e))
    }

    fn capabilities(&self) -> NotifierCaps {
        *self.caps.get_or_init(|| self.probe())
    }
}
