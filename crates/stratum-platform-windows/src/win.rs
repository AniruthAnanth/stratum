//! The two things every module in this crate needs: UTF-16 strings, and one
//! honest translation of an `HRESULT` into [`PlatformError`].
//!
//! # Why the taxonomy is a pure function
//!
//! 08 §5.1 makes [`PlatformError::Cancelled`], [`PlatformError::Unsupported`],
//! [`PlatformError::PermissionDenied`] and
//! [`PlatformError::BackendUnavailable`] *answers*, not failures. Deciding
//! which of them a raw Win32 status is turns out to be the single most
//! repeated judgement in this crate — the Credential Manager, the registry, the
//! job object and `ShellExecuteW` all return the same `ERROR_ACCESS_DENIED`
//! and it means the same thing in all four. Putting that decision in one pure
//! function over an `i32` means it can be exercised exhaustively from a machine
//! that cannot boot Windows, which is the only way this unit's acceptance is
//! evidenced at all before an installer exists.
//!
//! `windows::core::Error` carries exactly an `HRESULT` and a message, so
//! nothing is lost by taking those two as arguments instead of the error.

use stratum_platform::PlatformError;

/// `HRESULT_FROM_WIN32` sets this facility on a wrapped Win32 status.
const FACILITY_WIN32: u32 = 0x8007_0000;

// The Win32 statuses this crate has to tell apart. Named rather than inlined:
// a bare `1314` at a call site is unreviewable.
/// `ERROR_FILE_NOT_FOUND`.
pub const ERROR_FILE_NOT_FOUND: u32 = 2;
/// `ERROR_PATH_NOT_FOUND`.
pub const ERROR_PATH_NOT_FOUND: u32 = 3;
/// `ERROR_ACCESS_DENIED`.
pub const ERROR_ACCESS_DENIED: u32 = 5;
/// `ERROR_ELEVATION_REQUIRED` — the caller must relaunch elevated.
pub const ERROR_ELEVATION_REQUIRED: u32 = 740;
/// `ERROR_CANCELLED` — the user dismissed a UAC prompt or a shell dialog.
pub const ERROR_CANCELLED: u32 = 1223;
/// `ERROR_NOT_FOUND`.
pub const ERROR_NOT_FOUND: u32 = 1168;
/// `ERROR_NO_SUCH_LOGON_SESSION` — the Credential Manager has nothing to talk to.
pub const ERROR_NO_SUCH_LOGON_SESSION: u32 = 1312;
/// `ERROR_PRIVILEGE_NOT_HELD`.
pub const ERROR_PRIVILEGE_NOT_HELD: u32 = 1314;
/// `RPC_S_SERVER_UNAVAILABLE` — the notification or shell service is not running.
pub const RPC_S_SERVER_UNAVAILABLE: u32 = 1722;
/// `ERROR_PROCESS_MODE_ALREADY_BACKGROUND`.
pub const ERROR_PROCESS_MODE_ALREADY_BACKGROUND: u32 = 402;
/// `ERROR_PROCESS_MODE_NOT_BACKGROUND`.
pub const ERROR_PROCESS_MODE_NOT_BACKGROUND: u32 = 403;

/// The Win32 status inside an `HRESULT`, when there is one.
///
/// A raw `WIN32_ERROR` (as the registry API returns) is a small positive
/// integer and is its own code; anything wrapped by `HRESULT_FROM_WIN32`
/// carries facility 7 in the high half. Both spellings reach this crate, from
/// `RegSetValueExW` and from `windows::core::Error` respectively, and
/// collapsing them here is what lets [`classify`] have one arm per meaning
/// rather than two.
#[must_use]
pub const fn win32_code(hr: i32) -> Option<u32> {
    let raw = hr as u32;
    if raw & 0xFFFF_0000 == FACILITY_WIN32 {
        Some(raw & 0x0000_FFFF)
    } else if hr >= 0 {
        Some(raw)
    } else {
        None
    }
}

/// Map a Win32 status or `HRESULT` onto the platform error taxonomy.
///
/// The statuses that are *answers* rather than failures are classified; every
/// other one keeps its raw code in [`PlatformError::Os`], because a status we
/// have not thought about is exactly the one a bug report needs verbatim.
#[must_use]
pub fn classify(hr: i32, message: String) -> PlatformError {
    match win32_code(hr) {
        Some(ERROR_CANCELLED) => PlatformError::Cancelled,
        Some(ERROR_ACCESS_DENIED | ERROR_PRIVILEGE_NOT_HELD | ERROR_ELEVATION_REQUIRED) => {
            PlatformError::PermissionDenied(message)
        }
        // No logon session means the Credential Manager is not reachable from
        // this process (a service account, a stripped session); no RPC server
        // means the shell or the notification platform is not running. Both are
        // states a headless CI box is legitimately in.
        Some(ERROR_NO_SUCH_LOGON_SESSION | RPC_S_SERVER_UNAVAILABLE) => {
            PlatformError::BackendUnavailable(message)
        }
        Some(ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND) => {
            PlatformError::Io(std::io::Error::new(std::io::ErrorKind::NotFound, message))
        }
        _ => PlatformError::Os {
            code: i64::from(hr),
            message,
        },
    }
}

/// NUL-terminated UTF-16, the only string shape the `*W` entry points accept.
///
/// Every Win32 call in this crate goes through here rather than through
/// `HSTRING`, because a `PCWSTR` must stay valid for the duration of the call
/// and a temporary would be dropped at the end of the argument expression.
#[must_use]
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Decode a UTF-16 slice that may or may not carry a trailing NUL.
///
/// Lossy on purpose: this reads values the *user's* registry and the *user's*
/// credential store hold, which we did not write and cannot assume are
/// well-formed UTF-16. Refusing to render a `Path` because one entry has a lone
/// surrogate would be a worse answer than a replacement character.
#[must_use]
pub fn from_wide(units: &[u16]) -> String {
    let end = units.iter().position(|u| *u == 0).unwrap_or(units.len());
    String::from_utf16_lossy(&units[..end])
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn a_wrapped_win32_status_and_a_bare_one_decode_to_the_same_code() {
        // HRESULT_FROM_WIN32(ERROR_ACCESS_DENIED)
        assert_eq!(
            win32_code(0x8007_0005_u32 as i32),
            Some(ERROR_ACCESS_DENIED)
        );
        // What RegSetValueExW hands back directly.
        assert_eq!(win32_code(5), Some(ERROR_ACCESS_DENIED));
        // A genuine non-Win32 HRESULT (E_NOINTERFACE) has no Win32 code at all.
        assert_eq!(win32_code(0x8000_4002_u32 as i32), None);
    }

    /// The four statuses that must never surface as a generic OS error,
    /// because each one has a distinct affordance in the UI: retry elevated,
    /// hide the button, say nothing, do nothing.
    #[test]
    fn the_answers_are_classified_and_everything_else_keeps_its_code() {
        assert!(classify(0x8007_04C7_u32 as i32, String::new()).is_cancelled());
        assert!(matches!(
            classify(0x8007_0005_u32 as i32, "denied".into()),
            PlatformError::PermissionDenied(_)
        ));
        assert!(matches!(
            classify(0x8007_02E4_u32 as i32, "elevate".into()),
            PlatformError::PermissionDenied(_)
        ));
        assert!(matches!(
            classify(0x8007_0520_u32 as i32, "no session".into()),
            PlatformError::BackendUnavailable(_)
        ));
        assert!(matches!(
            classify(0x8007_06BA_u32 as i32, "no rpc".into()),
            PlatformError::BackendUnavailable(_)
        ));
        assert!(matches!(
            classify(0x8007_0002_u32 as i32, "missing".into()),
            PlatformError::Io(_)
        ));

        // ERROR_SHARING_VIOLATION (32) is not one we have an affordance for;
        // it keeps its number so a bug report carries it.
        let other = classify(0x8007_0020_u32 as i32, "sharing".into());
        assert!(
            matches!(other, PlatformError::Os { code, .. } if code == 0x8007_0020_u32 as i32 as i64)
        );
    }

    #[test]
    fn wide_strings_round_trip_including_non_bmp() {
        let s = "C:\\Users\\ada\\\u{1F4C8}";
        let w = wide(s);
        assert_eq!(w.last(), Some(&0));
        assert_eq!(from_wide(&w), s);
        // No NUL at all is also a shape the registry hands us.
        assert_eq!(from_wide(&s.encode_utf16().collect::<Vec<_>>()), s);
    }

    #[test]
    fn a_lone_surrogate_in_the_users_registry_is_rendered_not_refused() {
        assert_eq!(from_wide(&[0xD800, 0x0041, 0]), "\u{FFFD}A");
    }
}
