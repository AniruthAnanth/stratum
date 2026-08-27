//! Windows toasts and the taskbar badge — 08 §5.4.
//!
//! Used sparingly and only for the case that needs it: a long `run`, or a "Run
//! all stale blocks" (§13), finishing while the app is in the background.
//!
//! # Everything here is downstream of the AUMID
//!
//! `ToastNotificationManager::CreateToastNotifierWithId` takes an
//! AppUserModelID and **succeeds for an id nothing has registered**; `Show`
//! then returns `Ok` and no toast appears. See [`crate::aumid`] for the whole
//! argument. Every method below therefore begins by asking that module whether
//! we are registered, and returns
//! [`stratum_platform::PlatformError::Unsupported`] when we are not — which is
//! the honest answer for `cargo run`, `cargo test`, a portable unzip and every
//! CI box, and is why `Unsupported` is a first-class return in this layer
//! rather than a panic waiting to happen.
//!
//! # The payload is built as text, and that is the point
//!
//! A toast is an XML document. Every field in a [`Notification`] is
//! user-controlled: the title carries a do-file's name, the body carries a
//! command and a diagnostic. `Stata's "count"` in a filename, or a `<` in an
//! error message, is not exotic — it is Tuesday. [`toast_xml`] escapes, and it
//! is a pure function so the escaping is asserted on every host rather than
//! discovered when `XmlDocument::LoadXml` rejects a document.

use stratum_platform::Notification;

/// Windows' ceiling for a notification tag or group.
pub const MAX_KEY_LEN: usize = 64;

/// The group every Stratum toast belongs to when a caller names no thread.
pub const DEFAULT_GROUP: &str = "stratum";

/// XML text-node and attribute escaping.
///
/// All five predefined entities, not the three a text node strictly needs: the
/// same function is used for both positions, and a version that is only correct
/// in one of them is a version that will be used in the other.
#[must_use]
pub fn escape_xml(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            // A control character is not representable in XML 1.0 at all, and
            // `LoadXml` rejects the whole document over one of them. Tab,
            // newline and carriage return are the three that are legal.
            c if (c as u32) < 0x20 && !matches!(c, '\t' | '\n' | '\r') => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

/// Clamp a tag or group to what Windows accepts.
///
/// Truncated at a **character** boundary, not a byte one, and only ASCII
/// alphanumerics plus `.`/`-`/`_` survive: the documented rule is 64 characters
/// with no further constraint, but a tag is also a dictionary key in the Action
/// Center and a stray control character there is not worth the risk.
#[must_use]
pub fn clamp_key(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .take(MAX_KEY_LEN)
        .collect()
}

/// The `(group, tag)` pair for a notification.
///
/// The group is the caller's thread — one thread per session, so ten finished
/// runs collapse into one Action Center entry — and the tag is a monotonic
/// sequence number, so two toasts posted in the same millisecond stay
/// individually withdrawable.
#[must_use]
pub fn notification_key(thread: Option<&str>, seq: u64) -> (String, String) {
    let group = thread.map_or_else(
        || DEFAULT_GROUP.to_owned(),
        |t| {
            let c = clamp_key(t);
            if c.is_empty() {
                DEFAULT_GROUP.to_owned()
            } else {
                c
            }
        },
    );
    (group, format!("n{seq}"))
}

/// Pack a `(group, tag)` into the opaque [`stratum_platform::NotificationId`].
///
/// A tab, because [`clamp_key`] has already guaranteed neither half can contain
/// one. The trait's id is a single opaque string on purpose — macOS
/// `UNNotificationRequest` identifiers and Linux D-Bus ids have nothing in
/// common — so Windows' two-part key has to travel inside it.
#[must_use]
pub fn pack_id(group: &str, tag: &str) -> String {
    format!("{group}\t{tag}")
}

/// The inverse of [`pack_id`].
///
/// `None` for an id this platform did not mint, which is what a caller
/// replaying a persisted id from a macOS session would hand us.
#[must_use]
pub fn unpack_id(id: &str) -> Option<(&str, &str)> {
    let (group, tag) = id.split_once('\t')?;
    (!group.is_empty() && !tag.is_empty()).then_some((group, tag))
}

/// Build the toast payload.
///
/// `ToastGeneric` rather than one of the legacy `ToastText0N` templates: the
/// legacy templates are fixed-shape and silently drop a third line.
///
/// **`n.actions` is deliberately not rendered.** Buttons on an unpackaged
/// Win32 toast route their activation through an `INotificationActivationCallback`
/// COM server that the installer must register under `HKCU\Software\Classes\CLSID`;
/// without it the button appears and does nothing when clicked. A control that
/// looks live and is not is worse than its absence, so
/// [`stratum_platform::NotifierCaps::actions`] is `false` here and the payload
/// matches what the capability promises. Wiring the activator is an installer
/// change (W22) plus a callback in the shell, not something this layer can do
/// alone.
#[must_use]
pub fn toast_xml(n: &Notification) -> String {
    let mut x = String::with_capacity(256);
    x.push_str("<toast><visual><binding template=\"ToastGeneric\">");
    // The first `<text>` is the title, and its position is structural: emitting
    // it conditionally would promote the body to bold on a titleless toast.
    x.push_str("<text>");
    x.push_str(&escape_xml(&n.title));
    x.push_str("</text>");
    if let Some(sub) = &n.subtitle {
        x.push_str("<text>");
        x.push_str(&escape_xml(sub));
        x.push_str("</text>");
    }
    if !n.body.is_empty() {
        x.push_str("<text>");
        x.push_str(&escape_xml(&n.body));
        x.push_str("</text>");
    }
    x.push_str("</binding></visual>");
    if !n.sound {
        x.push_str("<audio silent=\"true\"/>");
    }
    x.push_str("</toast>");
    x
}

/// What this platform's notification stack can do.
///
/// A `const fn` so the four answers are one reviewable place rather than a
/// struct literal buried in a trait impl, and so the tests can state why each
/// one is what it is.
#[must_use]
pub const fn capabilities() -> stratum_platform::NotifierCaps {
    stratum_platform::NotifierCaps {
        // See `toast_xml`: needs a COM activation server the installer registers.
        actions: false,
        // Windows 11 removed live tiles, and a Win32 taskbar overlay icon needs
        // an HWND that `Notifier::set_badge` is not given. See `set_badge`.
        badge: false,
        // `<progress>` exists in the adaptive schema but driving it needs
        // NotificationData + `ToastNotifier::Update`, which is a stateful
        // surface this trait does not have.
        progress: false,
        sound: true,
    }
}

#[cfg(target_os = "windows")]
pub use sys::WindowsNotifier;

#[cfg(target_os = "windows")]
mod sys {
    use std::sync::atomic::{AtomicU64, Ordering};

    use stratum_platform::{
        Badge, Notification, NotificationId, Notifier, NotifierCaps, PermissionState,
        PlatformError, Result,
    };
    use windows::core::HSTRING;
    use windows::Data::Xml::Dom::XmlDocument;
    use windows::UI::Notifications::{
        NotificationSetting, ToastNotification, ToastNotificationManager, ToastNotifier,
    };

    use super::{capabilities, notification_key, pack_id, toast_xml, unpack_id};
    use crate::aumid;
    use crate::win;

    /// [`Notifier`] for Windows.
    #[derive(Debug, Default)]
    pub struct WindowsNotifier {
        /// Monotonic suffix, so two notifications posted in the same
        /// millisecond still get distinct tags and `withdraw` stays precise.
        seq: AtomicU64,
    }

    impl WindowsNotifier {
        /// Construct. Touches nothing until the first call, so building a
        /// [`crate::WindowsPlatform`] can never talk to the shell.
        #[must_use]
        pub const fn new() -> Self {
            Self {
                seq: AtomicU64::new(0),
            }
        }

        /// A notifier bound to our AUMID, or the reason there is none.
        fn notifier() -> Result<ToastNotifier> {
            if !aumid::is_registered() {
                return Err(aumid::UNREGISTERED);
            }
            aumid::register_for_process()?;
            ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(aumid::AUMID))
                .map_err(|e| win::classify(e.code().0, e.message()))
        }
    }

    impl Notifier for WindowsNotifier {
        fn request_permission(&self) -> Result<PermissionState> {
            // Windows has no permission *prompt* for a desktop application —
            // the user turns notifications off per-app in Settings, and asking
            // is not a thing an app can do. Reading the setting is therefore
            // the whole of this call, and it never surprises the user with a
            // dialog.
            let setting = Self::notifier()?
                .Setting()
                .map_err(|e| win::classify(e.code().0, e.message()))?;
            Ok(match setting {
                NotificationSetting::Enabled => PermissionState::Granted,
                // Disabled for the app, for the user, or by group policy. All
                // three mean "do not post and point at Settings"; none of them
                // means "ask again", which is what NotDetermined would invite.
                _ => PermissionState::Denied,
            })
        }

        fn notify(&self, n: &Notification) -> Result<NotificationId> {
            let notifier = Self::notifier()?;

            let doc = XmlDocument::new().map_err(|e| win::classify(e.code().0, e.message()))?;
            doc.LoadXml(&HSTRING::from(toast_xml(n)))
                .map_err(|e| win::classify(e.code().0, e.message()))?;
            let toast = ToastNotification::CreateToastNotification(&doc)
                .map_err(|e| win::classify(e.code().0, e.message()))?;

            let (group, tag) = notification_key(
                n.thread.as_deref(),
                self.seq.fetch_add(1, Ordering::Relaxed),
            );
            toast
                .SetGroup(&HSTRING::from(&group))
                .map_err(|e| win::classify(e.code().0, e.message()))?;
            toast
                .SetTag(&HSTRING::from(&tag))
                .map_err(|e| win::classify(e.code().0, e.message()))?;

            notifier
                .Show(&toast)
                .map_err(|e| win::classify(e.code().0, e.message()))?;
            Ok(NotificationId(pack_id(&group, &tag)))
        }

        fn withdraw(&self, id: NotificationId) -> Result<()> {
            if !aumid::is_registered() {
                return Err(aumid::UNREGISTERED);
            }
            let Some((group, tag)) = unpack_id(&id.0) else {
                // An id we did not mint. Nothing to withdraw and nothing broke.
                return Ok(());
            };
            let history = ToastNotificationManager::History()
                .map_err(|e| win::classify(e.code().0, e.message()))?;
            // Withdrawing one the user already dismissed is a documented no-op.
            history
                .RemoveGroupedTagWithId(
                    &HSTRING::from(tag),
                    &HSTRING::from(group),
                    &HSTRING::from(aumid::AUMID),
                )
                .map_err(|e| win::classify(e.code().0, e.message()))
        }

        fn set_badge(&self, _badge: Badge) -> Result<()> {
            // Not a gap we are hiding. `BadgeUpdateManager` writes to a tile,
            // and Windows 11 removed tiles; the taskbar's own overlay icon is
            // `ITaskbarList3::SetOverlayIcon`, which needs the `HWND` of a
            // specific window — and `Notifier::set_badge` is given no window.
            // Returning `Ok(())` for work that did not happen would leave the
            // queue count silently wrong forever.
            Err(PlatformError::Unsupported(
                "Windows has no application badge: the taskbar overlay icon belongs to a window \
                 and this call is given none",
            ))
        }

        fn capabilities(&self) -> NotifierCaps {
            capabilities()
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use stratum_platform::{Notification, NotificationAction};

    use super::*;

    fn n() -> Notification {
        Notification {
            title: "Run finished".into(),
            subtitle: None,
            body: "42 blocks".into(),
            sound: true,
            actions: Vec::new(),
            thread: None,
        }
    }

    #[test]
    fn the_payload_is_toastgeneric_with_the_title_first() {
        let x = toast_xml(&n());
        assert_eq!(
            x,
            "<toast><visual><binding template=\"ToastGeneric\">\
             <text>Run finished</text><text>42 blocks</text>\
             </binding></visual></toast>"
        );
    }

    #[test]
    fn a_subtitle_sits_between_the_title_and_the_body() {
        let mut n = n();
        n.subtitle = Some("wave2.do".into());
        let x = toast_xml(&n);
        let title = x.find("Run finished").unwrap();
        let sub = x.find("wave2.do").unwrap();
        let body = x.find("42 blocks").unwrap();
        assert!(title < sub && sub < body, "{x}");
    }

    /// The reason `toast_xml` is a pure function. A do-file called
    /// `Q&A <draft>.do`, or a Stata error message containing `<`, would make
    /// `XmlDocument::LoadXml` reject the whole document — and the user would
    /// get no notification with no explanation.
    #[test]
    fn every_field_is_escaped_because_every_field_is_a_users_filename() {
        let mut n = n();
        n.title = "Q&A <draft>.do".into();
        n.body = "r(198) \"quoted\" & 'apostrophe'".into();
        let x = toast_xml(&n);
        assert!(x.contains("Q&amp;A &lt;draft&gt;.do"), "{x}");
        assert!(x.contains("&quot;quoted&quot;"), "{x}");
        assert!(x.contains("&apos;apostrophe&apos;"), "{x}");
        // Nothing unescaped survived into the markup.
        assert_eq!(x.matches("<text>").count(), 2);
        assert!(!x.contains("<draft>"));
    }

    /// XML 1.0 cannot represent a control character at all, and `LoadXml`
    /// rejects the document rather than the character. A `\u{0}` in a body is
    /// what a truncated engine log line looks like.
    #[test]
    fn control_characters_are_replaced_not_passed_through() {
        let mut n = n();
        n.body = "a\u{0}b\u{7}c\td\ne".into();
        let x = toast_xml(&n);
        assert!(x.contains("a b c\td\ne"), "{x}");
        assert!(!x.contains('\u{0}'));
    }

    #[test]
    fn silence_is_explicit_and_sound_is_the_default() {
        let mut n = n();
        assert!(!toast_xml(&n).contains("<audio"));
        n.sound = false;
        assert!(toast_xml(&n).contains("<audio silent=\"true\"/>"));
    }

    /// The capability says `actions: false`, so the payload must not carry
    /// buttons — a button whose activation cannot be routed looks live and is
    /// not. See `toast_xml`'s docs for what would have to land first.
    #[test]
    fn buttons_are_not_rendered_because_the_capability_says_they_are_not_supported() {
        let mut n = n();
        n.actions = vec![NotificationAction {
            id: "run.retry".into(),
            label: "Retry".into(),
        }];
        assert!(!capabilities().actions);
        assert!(!toast_xml(&n).contains("<actions"), "{}", toast_xml(&n));
    }

    #[test]
    fn windows_has_no_application_badge_and_the_capability_says_so() {
        assert!(!capabilities().badge);
        assert!(capabilities().sound);
    }

    /// A thread groups a session's toasts so ten finished runs collapse into
    /// one Action Center entry; the tag keeps each individually withdrawable.
    #[test]
    fn the_group_is_the_thread_and_the_tag_is_unique() {
        let (g1, t1) = notification_key(Some("session-7"), 0);
        let (g2, t2) = notification_key(Some("session-7"), 1);
        assert_eq!(g1, "session-7");
        assert_eq!(g1, g2);
        assert_ne!(t1, t2);

        assert_eq!(notification_key(None, 0).0, DEFAULT_GROUP);
        assert_eq!(notification_key(Some(""), 0).0, DEFAULT_GROUP);
    }

    /// 64 characters is Windows' documented ceiling, and a thread key is a
    /// project path in practice.
    #[test]
    fn a_long_or_exotic_thread_is_clamped_rather_than_rejected() {
        let (g, _) = notification_key(Some(&"a/b c\u{1F4C8}".repeat(50)), 0);
        assert_eq!(g.chars().count(), MAX_KEY_LEN);
        assert!(g
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')));
    }

    /// The trait's id is one opaque string; Windows' key is two. The tab is
    /// safe because `clamp_key` cannot emit one.
    #[test]
    fn the_two_part_key_round_trips_through_the_opaque_id() {
        let (g, t) = notification_key(Some("session-7"), 3);
        let id = pack_id(&g, &t);
        assert_eq!(unpack_id(&id), Some(("session-7", "n3")));
        // An id minted on another platform, or a hand-written one.
        assert_eq!(unpack_id("just-a-uuid"), None);
        assert_eq!(unpack_id("\tn3"), None);
        assert_eq!(unpack_id("g\t"), None);
    }
}
