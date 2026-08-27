//! The four registry operations this crate needs, and nothing else.
//!
//! Deliberately not a general-purpose registry wrapper. Everything here exists
//! for [`crate::shell`], and the shape of each function is dictated by one
//! requirement that a general wrapper would have abstracted away: **the value
//! type travels with the value**. A `Path` stored as `REG_EXPAND_SZ` that we
//! read, edit and write back as `REG_SZ` has silently frozen every
//! `%USERPROFILE%`-style entry in it to this user's home directory.
//!
//! Windows-only; there is no portable half to test, so the tests for the
//! behaviour that matters live on the pure functions in [`crate::shell`] that
//! decide *what* to write.
#![cfg(target_os = "windows")]

use std::collections::BTreeMap;

use stratum_platform::{PlatformError, Result};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{
    ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, ERROR_NO_MORE_ITEMS, ERROR_SUCCESS,
};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegEnumValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
    HKEY, KEY_READ, KEY_WRITE, REG_EXPAND_SZ, REG_OPTION_NON_VOLATILE, REG_SZ, REG_VALUE_TYPE,
};

use crate::win;

/// The ceiling on one registry value this crate will read. A `Path` is the
/// largest thing here and is measured in kilobytes.
const MAX_VALUE_BYTES: usize = 4 * 1024 * 1024;

/// An open key, closed on drop.
struct Key(HKEY);

impl Drop for Key {
    fn drop(&mut self) {
        if !self.0 .0.is_null() {
            // SAFETY: a key this type opened and still owns.
            let _ = unsafe { RegCloseKey(self.0) };
        }
    }
}

fn fail(code: u32, what: &str) -> PlatformError {
    win::classify(code as i32, format!("{what} (registry error {code})"))
}

fn open_read(hive: HKEY, subkey: &str) -> Result<Option<Key>> {
    let sub = win::wide(subkey);
    let mut out = HKEY::default();
    // SAFETY: `sub` outlives the call; `out` receives a key we own and close.
    let rc = unsafe {
        RegOpenKeyExW(
            hive,
            PCWSTR(sub.as_ptr()),
            None,
            KEY_READ,
            std::ptr::addr_of_mut!(out),
        )
    };
    if rc == ERROR_FILE_NOT_FOUND {
        // A key that is not there is a state, not a failure: no `Path` value
        // has ever been set for this user is the factory condition.
        return Ok(None);
    }
    if rc != ERROR_SUCCESS {
        return Err(fail(rc.0, subkey));
    }
    Ok(Some(Key(out)))
}

fn create_write(hive: HKEY, subkey: &str) -> Result<Key> {
    let sub = win::wide(subkey);
    let mut out = HKEY::default();
    // SAFETY: as `open_read`; `RegCreateKeyExW` opens an existing key or
    // creates it, which is what "register this association" means.
    let rc = unsafe {
        RegCreateKeyExW(
            hive,
            PCWSTR(sub.as_ptr()),
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            std::ptr::addr_of_mut!(out),
            None,
        )
    };
    if rc != ERROR_SUCCESS {
        return Err(fail(rc.0, subkey));
    }
    Ok(Key(out))
}

/// Read one string value, **with its type**.
///
/// `Ok(None)` for a key or value that is not there. The type comes back so the
/// caller can write it again unchanged; see the module docs.
///
/// # Errors
/// [`PlatformError::PermissionDenied`] on a hive we may not read (HKLM under a
/// restrictive policy), [`PlatformError::Os`] otherwise.
pub fn read_value(
    hive: HKEY,
    subkey: &str,
    value: &str,
) -> Result<Option<(String, REG_VALUE_TYPE)>> {
    let Some(key) = open_read(hive, subkey)? else {
        return Ok(None);
    };
    let name = win::wide(value);
    let mut ty = REG_VALUE_TYPE::default();
    let mut len: u32 = 0;

    // Sizing pass. A `Path` routinely exceeds 2 KB and there is no ceiling
    // worth guessing at.
    // SAFETY: a null data pointer with a live length pointer is the documented
    // way to ask for the required byte count.
    let rc = unsafe {
        RegQueryValueExW(
            key.0,
            PCWSTR(name.as_ptr()),
            None,
            Some(std::ptr::addr_of_mut!(ty)),
            None,
            Some(std::ptr::addr_of_mut!(len)),
        )
    };
    if rc == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    if rc != ERROR_SUCCESS && rc != ERROR_MORE_DATA {
        return Err(fail(rc.0, &format!(r"{subkey}\{value}")));
    }
    if len == 0 {
        return Ok(Some((String::new(), ty)));
    }

    let mut buf = vec![0u8; len as usize + 2];
    let mut cap = len;
    // SAFETY: `buf` is at least `cap` bytes and stays alive across the call.
    let rc = unsafe {
        RegQueryValueExW(
            key.0,
            PCWSTR(name.as_ptr()),
            None,
            Some(std::ptr::addr_of_mut!(ty)),
            Some(buf.as_mut_ptr()),
            Some(std::ptr::addr_of_mut!(cap)),
        )
    };
    if rc != ERROR_SUCCESS {
        return Err(fail(rc.0, &format!(r"{subkey}\{value}")));
    }
    Ok(Some((decode_utf16(&buf[..cap as usize]), ty)))
}

/// Every value under a key, as strings. Used for the environment blocks.
///
/// Values that are not string-shaped are skipped rather than rendered: the
/// environment block is all `REG_SZ`/`REG_EXPAND_SZ`, and a `REG_DWORD` that
/// someone put there is not a variable.
///
/// # Errors
/// As [`read_value`].
pub fn read_all(hive: HKEY, subkey: &str) -> Result<BTreeMap<String, String>> {
    let Some(key) = open_read(hive, subkey)? else {
        return Ok(BTreeMap::new());
    };
    let mut out = BTreeMap::new();
    // 32 767 is the documented maximum value-name length; the data buffer
    // grows on demand, because a `Path` is unbounded in practice.
    let mut name = vec![0u16; 32_768];
    let mut data = vec![0u8; 8192];
    let mut index: u32 = 0;

    loop {
        let mut name_len = u32::try_from(name.len()).unwrap_or(u32::MAX);
        let mut data_len = u32::try_from(data.len()).unwrap_or(u32::MAX);
        let mut ty: u32 = 0;
        // SAFETY: both buffers are live and their lengths are passed by
        // pointer exactly as `RegEnumValueW` requires; it updates both.
        let rc = unsafe {
            RegEnumValueW(
                key.0,
                index,
                Some(windows::core::PWSTR(name.as_mut_ptr())),
                std::ptr::addr_of_mut!(name_len),
                None,
                Some(std::ptr::addr_of_mut!(ty)),
                Some(data.as_mut_ptr()),
                Some(std::ptr::addr_of_mut!(data_len)),
            )
        };
        if rc == ERROR_NO_MORE_ITEMS {
            break;
        }
        if rc == ERROR_MORE_DATA {
            // Only the data buffer can plausibly be short: the name buffer is
            // already the documented maximum. Grow and retry the SAME index —
            // advancing here would silently drop a variable.
            //
            // Bounded, because the loop condition is the kernel's answer and
            // not ours: an unbounded doubling on a value that never fits is a
            // hang in the Settings pane, and no environment variable is 4 MiB.
            if data.len() >= MAX_VALUE_BYTES {
                return Err(PlatformError::BackendUnavailable(format!(
                    "a value under {subkey} exceeds {MAX_VALUE_BYTES} bytes and is not an \
                     environment variable"
                )));
            }
            data.resize((data.len() * 2).min(MAX_VALUE_BYTES), 0);
            continue;
        }
        if rc != ERROR_SUCCESS {
            return Err(fail(rc.0, subkey));
        }
        index += 1;

        let ty = REG_VALUE_TYPE(ty);
        if ty != REG_SZ && ty != REG_EXPAND_SZ {
            continue;
        }
        let key_name = win::from_wide(&name[..name_len as usize]);
        if key_name.is_empty() {
            continue;
        }
        out.insert(key_name, decode_utf16(&data[..data_len as usize]));
    }
    Ok(out)
}

/// Write one string value with an explicit type.
///
/// An empty `value` name writes the key's default value, which is how a ProgID
/// carries its description and its `shell\open\command`.
///
/// # Errors
/// [`PlatformError::PermissionDenied`] for HKLM without elevation, which is the
/// expected outcome of [`stratum_platform::InstallScope::System`] for a
/// standard user and has a UI affordance.
pub fn write_string(
    hive: HKEY,
    subkey: &str,
    value: &str,
    data: &str,
    ty: REG_VALUE_TYPE,
) -> Result<()> {
    let key = create_write(hive, subkey)?;
    let name = win::wide(value);
    let wide = win::wide(data);
    let bytes: Vec<u8> = wide.iter().flat_map(|u| u.to_le_bytes()).collect();
    // SAFETY: `name` and `bytes` outlive the call; the length is the byte count
    // including the terminating NUL, which is what the *W registry API expects.
    let rc = unsafe {
        RegSetValueExW(
            key.0,
            PCWSTR(name.as_ptr()),
            None,
            ty,
            Some(bytes.as_slice()),
        )
    };
    if rc != ERROR_SUCCESS {
        return Err(fail(rc.0, &format!(r"{subkey}\{value}")));
    }
    Ok(())
}

/// Write a value that carries no data — the shape `OpenWithProgids` uses,
/// where the *name* is the ProgID and the value itself is empty.
///
/// # Errors
/// As [`write_string`].
pub fn write_empty(hive: HKEY, subkey: &str, value: &str, ty: REG_VALUE_TYPE) -> Result<()> {
    let key = create_write(hive, subkey)?;
    let name = win::wide(value);
    // SAFETY: `name` outlives the call; `None` is an empty value.
    let rc = unsafe { RegSetValueExW(key.0, PCWSTR(name.as_ptr()), None, ty, None) };
    if rc != ERROR_SUCCESS {
        return Err(fail(rc.0, &format!(r"{subkey}\{value}")));
    }
    Ok(())
}

/// Decode a `REG_SZ`/`REG_EXPAND_SZ` byte buffer.
///
/// The registry does not promise a terminating NUL and does not promise an even
/// byte count; both have been observed on real machines with hand-edited hives.
fn decode_utf16(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    win::from_wide(&units)
}
