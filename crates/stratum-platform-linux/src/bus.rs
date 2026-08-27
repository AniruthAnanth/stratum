//! The one session-bus connection this crate opens.
//!
//! Three adapters talk D-Bus — the Secret Service, `org.freedesktop.Notifications`
//! and the FileChooser portal — and they share a single [`zbus::Connection`]
//! rather than opening one each. A connection is a socket, an authentication
//! handshake and a reader task; three of them is three times the handshake at
//! startup and three sockets held for the life of an IDE session that is
//! measured in hours.
//!
//! # The absence is cached, deliberately
//!
//! If there is no session bus — a headless CI runner, a server, `ssh` without
//! `-X`, a container — that is cached too, and every later call gets the same
//! [`PlatformError::BackendUnavailable`] without touching the filesystem or the
//! socket again. This is the same reasoning as [`crate::credentials`]'
//! demote-once: a machine with no bus must pay the probe **once per process**,
//! not once per credential read, and `tests/credentials.rs` asserts that shape
//! as a counter.
//!
//! A session bus that starts up after we looked is therefore not picked up
//! until restart. That is the right trade: a bus appearing mid-session is rare,
//! and the alternative is that every keystroke-adjacent path that touches a
//! credential retries a connect that will fail.

use std::sync::OnceLock;

use stratum_platform::{PlatformError, Result};

/// The shared connection, or the reason there is none.
static SESSION: OnceLock<std::result::Result<zbus::Connection, String>> = OnceLock::new();

/// Blocking accessor, for the three synchronous traits.
///
/// # Errors
/// [`PlatformError::BackendUnavailable`] when there is no session bus.
pub fn session_blocking() -> Result<zbus::blocking::Connection> {
    let conn = SESSION.get_or_init(|| {
        zbus::blocking::Connection::session()
            .map(zbus::blocking::Connection::into_inner)
            .map_err(|e| e.to_string())
    });
    match conn {
        Ok(c) => Ok(zbus::blocking::Connection::from(c.clone())),
        Err(e) => Err(no_bus(e)),
    }
}

/// Async accessor, for the FileChooser portal.
///
/// # Errors
/// As [`session_blocking`].
pub async fn session() -> Result<zbus::Connection> {
    if let Some(cached) = SESSION.get() {
        return match cached {
            Ok(c) => Ok(c.clone()),
            Err(e) => Err(no_bus(e)),
        };
    }
    let built = zbus::Connection::session().await.map_err(|e| e.to_string());
    // Losing the race is fine: the other thread's connection is equivalent and
    // ours is dropped.
    let _ = SESSION.set(built);
    match SESSION.get() {
        Some(Ok(c)) => Ok(c.clone()),
        Some(Err(e)) => Err(no_bus(e)),
        None => Err(PlatformError::BackendUnavailable(
            "the session bus connection vanished".to_owned(),
        )),
    }
}

fn no_bus(detail: &str) -> PlatformError {
    PlatformError::BackendUnavailable(format!(
        "no D-Bus session bus in this session ({detail}); \
         keyring, notifications and portal dialogs are unavailable"
    ))
}

/// Map a `zbus` error onto the platform taxonomy.
///
/// The classification is the point. `ServiceUnknown` — the bus name is not
/// owned and cannot be activated — is exactly "this desktop does not have that
/// service", which is [`PlatformError::BackendUnavailable`] and is what makes
/// the credential demotion and the dialog fallback fire. An
/// `AccessDenied`/`AuthFailed` is a service that exists and refused, which must
/// NOT trigger either of them.
#[must_use]
pub fn classify(e: &zbus::Error) -> PlatformError {
    if let zbus::Error::MethodError(name, detail, _) = e {
        let name = name.as_str();
        let detail = detail.clone().unwrap_or_default();
        return match name {
            "org.freedesktop.DBus.Error.ServiceUnknown"
            | "org.freedesktop.DBus.Error.NameHasNoOwner"
            | "org.freedesktop.DBus.Error.UnknownInterface"
            | "org.freedesktop.DBus.Error.UnknownMethod"
            | "org.freedesktop.DBus.Error.NotSupported"
            | "org.freedesktop.DBus.Error.Spawn.ServiceNotFound" => {
                PlatformError::BackendUnavailable(format!("{name}: {detail}"))
            }
            "org.freedesktop.DBus.Error.AccessDenied"
            | "org.freedesktop.DBus.Error.AuthFailed"
            | "org.freedesktop.DBus.Error.InteractiveAuthorizationRequired" => {
                PlatformError::PermissionDenied(format!("{name}: {detail}"))
            }
            _ => PlatformError::Os {
                // D-Bus has no numeric errors; the name is the whole diagnosis
                // and inventing a code would be noise.
                code: 0,
                message: format!("{name}: {detail}"),
            },
        };
    }
    match e {
        // No socket, no address, handshake refused: there is no bus for us.
        zbus::Error::Address(_) | zbus::Error::Handshake(_) | zbus::Error::InputOutput(_) => {
            PlatformError::BackendUnavailable(e.to_string())
        }
        other => PlatformError::Os {
            code: 0,
            message: other.to_string(),
        },
    }
}
