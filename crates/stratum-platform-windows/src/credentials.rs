//! Windows Credential Manager — 08 §5.3, spec §27/§22.
//!
//! `CRED_TYPE_GENERIC` items written with `CredWriteW` and read back with
//! `CredReadW`, exactly as 08 §5.3's table prescribes, and deliberately not
//! through the `keyring` crate (`deny.toml` bans it, ARCHITECTURE C17): the
//! Settings pane has to be able to tell the user *which* store holds their API
//! key, and an abstraction that erases the difference between the Credential
//! Manager and an encrypted file we wrote ourselves erases a privacy statement
//! (§22).
//!
//! # `CRED_PERSIST_LOCAL_MACHINE`, and why it is this platform's acceptance
//!
//! macOS's bullet is `kSecAttrAccessibleWhenUnlockedThisDeviceOnly` — "never
//! leaves the machine". The Windows analogue is the persistence class, and the
//! choice is between two that both look plausible:
//!
//! * `CRED_PERSIST_ENTERPRISE` roams with the user's profile. On a domain-joined
//!   university machine — which is most of our users — that copies the
//!   researcher's OpenAI key to every workstation they log into and to the
//!   roaming profile share.
//! * `CRED_PERSIST_LOCAL_MACHINE` stays on this computer for this user.
//!
//! So `LOCAL_MACHINE` is not a default we accepted, it is the same guarantee
//! the Keychain arm makes, spelled in the vocabulary Windows has.
//!
//! # The blob is UTF-16LE
//!
//! `CredentialBlob` is bytes, so any encoding "works". Windows' own
//! conventions, and the Credential Manager control panel that renders it, treat
//! a generic credential's blob as UTF-16 — and a user who opens
//! `control /name Microsoft.CredentialManager` to check what we stored should
//! see their key, not mojibake. The 2 560-byte ceiling
//! (`CRED_MAX_CREDENTIAL_BLOB_SIZE`) is therefore 1 280 characters, and it is
//! checked before the call rather than discovered as `ERROR_INVALID_PARAMETER`.

use stratum_platform::{PlatformError, Result};

/// `CRED_MAX_CREDENTIAL_BLOB_SIZE`, in bytes.
pub const MAX_BLOB_BYTES: usize = 2560;

/// The Credential Manager target name for a `(service, account)` pair.
///
/// `dev.stratum.app/ai-provider/openai`. One flat namespace, because
/// `CredEnumerateW`'s filter is a prefix match with a single trailing `*` and
/// nothing richer — [`enumerate_filter`] is the other half of this decision.
#[must_use]
pub fn target_name(service: &str, account: &str) -> String {
    format!("{service}/{account}")
}

/// The `CredEnumerateW` filter that matches every account under `service`.
///
/// One enumeration for `list_accounts`, never one `CredReadW` per candidate
/// account: the Settings pane calls this every time it opens, and a store that
/// probes provider ids one at a time turns a UI paint into N round-trips.
#[must_use]
pub fn enumerate_filter(service: &str) -> String {
    format!("{service}/*")
}

/// Recover the account from a target name, when it belongs to `service`.
///
/// `None` for a target that is not ours. The Credential Manager is a shared
/// namespace — every application on the machine writes into it — so an
/// enumeration result that does not match the prefix is somebody else's item
/// and must not be reported as a configured provider.
#[must_use]
pub fn account_of<'a>(target: &'a str, service: &str) -> Option<&'a str> {
    let rest = target.strip_prefix(service)?.strip_prefix('/')?;
    // A nested separator would mean a namespace we do not define; refuse it
    // rather than reporting `openai/extra` as an account.
    (!rest.is_empty() && !rest.contains('/')).then_some(rest)
}

/// Encode a secret for `CredentialBlob`.
///
/// # Errors
/// [`PlatformError::Unsupported`] when the secret exceeds
/// [`MAX_BLOB_BYTES`] — an answer, not a failure: no API key is 1 280
/// characters, and "the OS will not hold this" is something the settings pane
/// can say, whereas `ERROR_INVALID_PARAMETER` is not.
pub fn encode_blob(secret: &str) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(secret.len() * 2);
    for unit in secret.encode_utf16() {
        out.extend_from_slice(&unit.to_le_bytes());
    }
    if out.len() > MAX_BLOB_BYTES {
        return Err(PlatformError::Unsupported(
            "the Windows Credential Manager holds at most 2560 bytes per item",
        ));
    }
    Ok(out)
}

/// Decode a `CredentialBlob` we wrote.
///
/// # Errors
/// [`PlatformError::BackendUnavailable`] for a blob that is not well-formed
/// UTF-16LE, which means the item exists but was written by something other
/// than us. Reporting that as "no key configured" would send the user to enter
/// a key they already have.
pub fn decode_blob(bytes: &[u8]) -> Result<String> {
    if !bytes.len().is_multiple_of(2) {
        return Err(PlatformError::BackendUnavailable(
            "the stored credential has an odd byte count; it was not written by Stratum".to_owned(),
        ));
    }
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16(&units).map_err(|_| {
        PlatformError::BackendUnavailable(
            "the stored credential is not valid UTF-16; it was not written by Stratum".to_owned(),
        )
    })
}

#[cfg(target_os = "windows")]
pub use sys::CredentialManager;

#[cfg(target_os = "windows")]
mod sys {
    use stratum_platform::{
        CredentialBackend, CredentialStore, ExposeSecret, PlatformError, Result, SecretString,
    };
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::Security::Credentials::{
        CredDeleteW, CredEnumerateW, CredFree, CredReadW, CredWriteW, CREDENTIALW, CRED_FLAGS,
        CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC,
    };

    use crate::win;

    /// [`CredentialStore`] over the Win32 Credential Manager.
    #[derive(Clone, Copy, Debug, Default)]
    pub struct CredentialManager;

    impl CredentialManager {
        /// Construct. Touches nothing until the first call.
        #[must_use]
        pub const fn new() -> Self {
            Self
        }
    }

    /// `ERROR_NOT_FOUND` is what both `CredReadW` and `CredEnumerateW` return
    /// for "there is no such item", which is a state and not an error.
    fn is_absent(e: &windows::core::Error) -> bool {
        win::win32_code(e.code().0) == Some(win::ERROR_NOT_FOUND)
    }

    fn oops(e: &windows::core::Error) -> PlatformError {
        win::classify(e.code().0, e.message())
    }

    impl CredentialStore for CredentialManager {
        fn get(&self, service: &str, account: &str) -> Result<Option<SecretString>> {
            let target = win::wide(&super::target_name(service, account));
            let mut raw: *mut CREDENTIALW = std::ptr::null_mut();
            // SAFETY: `target` is NUL-terminated and outlives the call; `raw`
            // receives a buffer that `CredFree` releases below.
            let call = unsafe {
                CredReadW(
                    PCWSTR(target.as_ptr()),
                    CRED_TYPE_GENERIC,
                    None,
                    std::ptr::addr_of_mut!(raw),
                )
            };
            if let Err(e) = call {
                return if is_absent(&e) {
                    Ok(None)
                } else {
                    Err(oops(&e))
                };
            }
            if raw.is_null() {
                return Ok(None);
            }
            // SAFETY: the call succeeded, so `raw` points at one CREDENTIALW
            // whose blob is `CredentialBlobSize` bytes long. The slice is
            // copied out before `CredFree`.
            let bytes = unsafe {
                let cred = &*raw;
                let bytes = if cred.CredentialBlob.is_null() {
                    Vec::new()
                } else {
                    std::slice::from_raw_parts(
                        cred.CredentialBlob,
                        cred.CredentialBlobSize as usize,
                    )
                    .to_vec()
                };
                CredFree(raw.cast());
                bytes
            };
            Ok(Some(SecretString::from(super::decode_blob(&bytes)?)))
        }

        fn set(&self, service: &str, account: &str, secret: &SecretString) -> Result<()> {
            let mut target = win::wide(&super::target_name(service, account));
            let mut user = win::wide(account);
            let mut blob = super::encode_blob(secret.expose_secret())?;

            let cred = CREDENTIALW {
                Flags: CRED_FLAGS(0),
                Type: CRED_TYPE_GENERIC,
                TargetName: windows::core::PWSTR(target.as_mut_ptr()),
                Comment: windows::core::PWSTR::null(),
                LastWritten: FILETIME::default(),
                CredentialBlobSize: u32::try_from(blob.len()).unwrap_or(u32::MAX),
                CredentialBlob: blob.as_mut_ptr(),
                // See the module docs: the Windows spelling of "never leaves
                // this machine". ENTERPRISE would roam it.
                Persist: CRED_PERSIST_LOCAL_MACHINE,
                AttributeCount: 0,
                Attributes: std::ptr::null_mut(),
                TargetAlias: windows::core::PWSTR::null(),
                // Not a login: the Credential Manager UI shows this column, and
                // the provider id is the only thing there that means anything.
                UserName: windows::core::PWSTR(user.as_mut_ptr()),
            };
            // SAFETY: every pointer in `cred` borrows a local that outlives the
            // call, and `CredentialBlobSize` is `blob.len()`. `CredWriteW`
            // copies; nothing escapes.
            let r = unsafe { CredWriteW(std::ptr::addr_of!(cred), 0) };
            // Keep the buffers alive past the call in a way the optimiser
            // cannot reorder away.
            drop((target, user, blob));
            r.map_err(|e| oops(&e))
        }

        fn delete(&self, service: &str, account: &str) -> Result<()> {
            let target = win::wide(&super::target_name(service, account));
            // SAFETY: as `get`.
            let r = unsafe { CredDeleteW(PCWSTR(target.as_ptr()), CRED_TYPE_GENERIC, None) };
            match r {
                Ok(()) => Ok(()),
                // The caller asked for a state, not for an event.
                Err(e) if is_absent(&e) => Ok(()),
                Err(e) => Err(oops(&e)),
            }
        }

        fn list_accounts(&self, service: &str) -> Result<Vec<String>> {
            let filter = win::wide(&super::enumerate_filter(service));
            let mut count: u32 = 0;
            let mut items: *mut *mut CREDENTIALW = std::ptr::null_mut();
            // SAFETY: `filter` outlives the call; `count`/`items` receive the
            // enumeration, released by the single `CredFree` below.
            let call = unsafe {
                CredEnumerateW(
                    PCWSTR(filter.as_ptr()),
                    // NOT `CRED_ENUMERATE_ALL_CREDENTIALS`. That flag is
                    // documented as requiring a NULL filter, and passing both
                    // fails the call outright — so the Settings pane would have
                    // reported "no providers configured" for a user who had
                    // configured several.
                    None,
                    std::ptr::addr_of_mut!(count),
                    std::ptr::addr_of_mut!(items),
                )
            };
            if let Err(e) = call {
                // No item under this service is the normal state before the
                // user has entered any key at all.
                return if is_absent(&e) {
                    Ok(Vec::new())
                } else {
                    Err(oops(&e))
                };
            }
            if items.is_null() {
                return Ok(Vec::new());
            }

            let mut found = Vec::with_capacity(count as usize);
            // SAFETY: the call succeeded, so `items` is an array of `count`
            // non-null pointers, each valid until `CredFree`.
            unsafe {
                for i in 0..count as usize {
                    let cred = &*(*items.add(i));
                    if cred.TargetName.is_null() {
                        continue;
                    }
                    let target = cred.TargetName.to_string().unwrap_or_default();
                    if let Some(account) = super::account_of(&target, service) {
                        found.push(account.to_owned());
                    }
                }
                CredFree(items.cast());
            }
            found.sort_unstable();
            found.dedup();
            Ok(found)
        }

        fn backend(&self) -> CredentialBackend {
            CredentialBackend::WindowsCredentialManager
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn the_target_grammar_is_the_service_naming_rule_plus_the_account() {
        let service = stratum_platform::credentials::service(
            stratum_platform::credentials::PURPOSE_AI_PROVIDER,
        );
        assert_eq!(
            target_name(&service, "openai"),
            "dev.stratum.app/ai-provider/openai"
        );
        assert_eq!(enumerate_filter(&service), "dev.stratum.app/ai-provider/*");
    }

    /// The Credential Manager is a namespace shared with every other program on
    /// the machine. An enumeration that reported someone else's item as a
    /// configured provider would show a key the user never entered.
    #[test]
    fn only_our_own_targets_yield_an_account() {
        let s = "dev.stratum.app/ai-provider";
        assert_eq!(
            account_of("dev.stratum.app/ai-provider/openai", s),
            Some("openai")
        );
        assert_eq!(account_of("git:https://github.com", s), None);
        assert_eq!(account_of("dev.stratum.app/ai-provider", s), None);
        assert_eq!(account_of("dev.stratum.app/ai-provider/", s), None);
        assert_eq!(account_of("dev.stratum.app/ai-provider/a/b", s), None);
        // A prefix that merely starts the same way is not ours.
        assert_eq!(account_of("dev.stratum.app/ai-provider-evil/x", s), None);
    }

    #[test]
    fn a_secret_round_trips_through_the_blob_encoding() {
        for s in ["sk-abc123", "", "clé-\u{00e9}\u{1F511}", "a=b\nc"] {
            let bytes = encode_blob(s).unwrap();
            assert_eq!(bytes.len() % 2, 0);
            assert_eq!(decode_blob(&bytes).unwrap(), s);
        }
    }

    /// The blob is little-endian UTF-16, which is what the Credential Manager
    /// control panel renders. `A` is one code unit, an emoji is a surrogate
    /// pair and therefore four bytes.
    #[test]
    fn the_blob_is_utf16le_not_utf8() {
        assert_eq!(encode_blob("A").unwrap(), vec![0x41, 0x00]);
        assert_eq!(encode_blob("\u{1F511}").unwrap().len(), 4);
    }

    #[test]
    fn an_oversized_secret_is_refused_before_the_call_not_after() {
        let err = encode_blob(&"x".repeat(MAX_BLOB_BYTES / 2 + 1)).unwrap_err();
        assert!(err.is_unsupported(), "{err}");
        assert!(encode_blob(&"x".repeat(MAX_BLOB_BYTES / 2)).is_ok());
    }

    #[test]
    fn a_blob_we_did_not_write_is_reported_not_treated_as_absent() {
        let odd = decode_blob(&[0x41]).unwrap_err();
        assert!(matches!(odd, PlatformError::BackendUnavailable(_)), "{odd}");
        // An unpaired high surrogate: well-sized, not decodable.
        let bad = decode_blob(&[0x00, 0xD8]).unwrap_err();
        assert!(matches!(bad, PlatformError::BackendUnavailable(_)), "{bad}");
    }
}
