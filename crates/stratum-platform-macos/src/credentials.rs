//! macOS Keychain — 08 §5.3, spec §27/§22.
//!
//! Generic password items, written through `SecItemAdd`/`SecItemCopyMatching`
//! with a dictionary built by hand rather than through
//! `security_framework::item::ItemAddOptions`, because that builder has no way
//! to set `kSecAttrAccessible` — and setting it is this unit's acceptance:
//!
//! * `kSecAttrAccessible = kSecAttrAccessibleWhenUnlockedThisDeviceOnly` pins
//!   the item to *this* device and to the unlocked state.
//! * `kSecAttrSynchronizable = false` is the attribute that actually decides
//!   iCloud Keychain participation. It is set on every write **and on every
//!   query**: an item stored with it false is not matched by a query that
//!   leaves it unspecified, and "the key I just saved is gone" is the worst
//!   failure mode a credential store has.
//!
//! # Two keychains, one backend
//!
//! macOS has two implementations behind `SecItem*`. The **data-protection**
//! keychain is the one that honours `kSecAttrAccessible`, and it requires the
//! calling binary to be code-signed with a keychain-access group — true of the
//! notarised app, false of `cargo test` and of every unsigned developer build,
//! where a write returns `errSecMissingEntitlement` (-34018). The **file**
//! (login) keychain accepts the same dictionary from any binary; it ignores
//! `kSecAttrAccessible`, and there the "never leaves this machine" guarantee is
//! carried by `kSecAttrSynchronizable = false` alone, because iCloud Keychain
//! sync is a data-protection-keychain feature keyed on exactly that attribute.
//!
//! So we start on the data-protection keychain and **demote once**, on the
//! first `errSecMissingEntitlement`, retrying the same operation against the
//! file keychain. Demote-on-error rather than probe-then-choose: a probe that
//! is a read succeeds without the entitlement and proves nothing (measured), a
//! probe that is a write leaves an item behind, and neither is needed when the
//! error itself is unambiguous. [`CredentialStore::backend`] reports
//! [`CredentialBackend::MacosKeychain`] either way — both ARE the Keychain, and
//! saying otherwise would misinform the privacy statement in Settings (§22).

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};

use core_foundation::array::CFArray;
use core_foundation::base::{CFType, TCFType, ToVoid};
use core_foundation::boolean::CFBoolean;
use core_foundation::data::CFData;
use core_foundation::dictionary::{CFDictionary, CFMutableDictionary};
use core_foundation::string::CFString;
use core_foundation_sys::base::{CFTypeRef, OSStatus};
use core_foundation_sys::string::CFStringRef;
use security_framework::base::Error as SecError;
use security_framework_sys::access_control::kSecAttrAccessibleWhenUnlockedThisDeviceOnly;
use security_framework_sys::base::{errSecAuthFailed, errSecItemNotFound, errSecSuccess};
use security_framework_sys::item::{
    kSecAttrAccount, kSecAttrService, kSecAttrSynchronizable, kSecClass, kSecClassGenericPassword,
    kSecMatchLimit, kSecMatchLimitAll, kSecReturnAttributes, kSecReturnData,
    kSecUseDataProtectionKeychain, kSecValueData,
};
use security_framework_sys::keychain_item::{
    SecItemAdd, SecItemCopyMatching, SecItemDelete, SecItemUpdate,
};
use stratum_platform::{
    CredentialBackend, CredentialStore, ExposeSecret, PlatformError, Result, SecretString,
};

// Not exported by `security-framework-sys`, which exports the VALUE
// (`kSecAttrAccessibleWhenUnlockedThisDeviceOnly`) but not the key. Both are
// Security.framework globals and that framework is already linked by
// `security-framework-sys`.
extern "C" {
    #[link_name = "kSecAttrAccessible"]
    static K_SEC_ATTR_ACCESSIBLE: CFStringRef;
}

// OSStatus values `security-framework-sys` does not name.
const ERR_SEC_USER_CANCELED: OSStatus = -128;
const ERR_SEC_NOT_AVAILABLE: OSStatus = -25291;
const ERR_SEC_DUPLICATE_ITEM: OSStatus = -25299;
const ERR_SEC_INTERACTION_NOT_ALLOWED: OSStatus = -25308;
const ERR_SEC_MISSING_ENTITLEMENT: OSStatus = -34018;

/// Which macOS keychain implementation an operation is addressed to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Scope {
    /// Honours `kSecAttrAccessible`; needs a keychain-access entitlement.
    DataProtection,
    /// The login keychain. Works unsigned.
    File,
}

/// [`CredentialStore`] over the macOS Keychain.
#[derive(Debug)]
pub struct Keychain {
    data_protection: AtomicBool,
}

impl Default for Keychain {
    fn default() -> Self {
        Self::new()
    }
}

impl Keychain {
    /// Construct. Touches nothing until the first call, so building a
    /// [`crate::MacosPlatform`] can never prompt.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            data_protection: AtomicBool::new(true),
        }
    }

    fn scope(&self) -> Scope {
        if self.data_protection.load(Ordering::Relaxed) {
            Scope::DataProtection
        } else {
            Scope::File
        }
    }

    /// Run `op` against the current scope, demoting to the file keychain and
    /// retrying exactly once if the OS says we lack the entitlement.
    fn run<T>(&self, mut op: impl FnMut(Scope) -> std::result::Result<T, OSStatus>) -> Result<T> {
        let scope = self.scope();
        match op(scope) {
            Ok(v) => Ok(v),
            Err(ERR_SEC_MISSING_ENTITLEMENT) if scope == Scope::DataProtection => {
                self.data_protection.store(false, Ordering::Relaxed);
                op(Scope::File).map_err(classify)
            }
            Err(s) => Err(classify(s)),
        }
    }
}

impl CredentialStore for Keychain {
    fn get(&self, service: &str, account: &str) -> Result<Option<SecretString>> {
        let bytes = self.run(|scope| {
            let mut q = base_query(service, account, scope);
            // No kSecMatchLimit: the default is one item, which is exactly what
            // a (service, account) pair identifies.
            unsafe {
                q.add(
                    &kSecReturnData.to_void(),
                    &CFBoolean::true_value().to_void(),
                )
            };

            let mut out: CFTypeRef = std::ptr::null();
            let status =
                unsafe { SecItemCopyMatching(q.to_immutable().as_concrete_TypeRef(), &mut out) };
            if status == errSecItemNotFound {
                return Ok(None);
            }
            if status != errSecSuccess {
                if !out.is_null() {
                    drop(unsafe { CFType::wrap_under_create_rule(out) });
                }
                return Err(status);
            }
            if out.is_null() {
                return Ok(None);
            }
            // SAFETY: kSecReturnData was requested and the call succeeded, so
            // `out` is a +1 CFData.
            Ok(Some(
                unsafe { CFData::wrap_under_create_rule(out.cast()) }.to_vec(),
            ))
        })?;

        let Some(bytes) = bytes else {
            return Ok(None);
        };
        let secret = String::from_utf8(bytes).map_err(|_| {
            PlatformError::BackendUnavailable(
                "keychain item is not valid UTF-8; it was not written by Stratum".to_owned(),
            )
        })?;
        Ok(Some(SecretString::from(secret)))
    }

    fn set(&self, service: &str, account: &str, secret: &SecretString) -> Result<()> {
        let value = CFData::from_buffer(secret.expose_secret().as_bytes());
        self.run(|scope| {
            let mut add = base_query(service, account, scope);
            unsafe {
                add.add(&value_key(), &value.to_void());
                add.add(&K_SEC_ATTR_ACCESSIBLE.to_void(), &accessible_value());
            }
            let status = unsafe {
                SecItemAdd(
                    add.to_immutable().as_concrete_TypeRef(),
                    std::ptr::null_mut(),
                )
            };
            match status {
                s if s == errSecSuccess => Ok(()),
                ERR_SEC_DUPLICATE_ITEM => {
                    // Update in place rather than delete-then-add: deleting
                    // drops the item's ACL, and the next read from a signed
                    // build would prompt the user for no reason.
                    let query = base_query(service, account, scope);
                    let mut attrs: CFMutableDictionary<*const c_void, *const c_void> =
                        CFMutableDictionary::new();
                    unsafe {
                        attrs.add(&value_key(), &value.to_void());
                        attrs.add(&K_SEC_ATTR_ACCESSIBLE.to_void(), &accessible_value());
                    }
                    let status = unsafe {
                        SecItemUpdate(
                            query.to_immutable().as_concrete_TypeRef(),
                            attrs.to_immutable().as_concrete_TypeRef(),
                        )
                    };
                    if status == errSecSuccess {
                        Ok(())
                    } else {
                        Err(status)
                    }
                }
                s => Err(s),
            }
        })
    }

    fn delete(&self, service: &str, account: &str) -> Result<()> {
        self.run(|scope| {
            let q = base_query(service, account, scope);
            let status = unsafe { SecItemDelete(q.to_immutable().as_concrete_TypeRef()) };
            // The caller asked for a state, not for an event.
            if status == errSecSuccess || status == errSecItemNotFound {
                Ok(())
            } else {
                Err(status)
            }
        })
    }

    fn list_accounts(&self, service: &str) -> Result<Vec<String>> {
        let mut accounts = self.run(|scope| {
            let mut q: CFMutableDictionary<*const c_void, *const c_void> =
                CFMutableDictionary::new();
            unsafe {
                q.add(&kSecClass.to_void(), &kSecClassGenericPassword.to_void());
                q.add(
                    &kSecAttrService.to_void(),
                    &CFString::new(service).to_void(),
                );
                q.add(
                    &kSecAttrSynchronizable.to_void(),
                    &CFBoolean::false_value().to_void(),
                );
                q.add(
                    &kSecReturnAttributes.to_void(),
                    &CFBoolean::true_value().to_void(),
                );
                q.add(&kSecMatchLimit.to_void(), &kSecMatchLimitAll.to_void());
                if scope == Scope::DataProtection {
                    q.add(
                        &kSecUseDataProtectionKeychain.to_void(),
                        &CFBoolean::true_value().to_void(),
                    );
                }
            }

            let mut out: CFTypeRef = std::ptr::null();
            let status =
                unsafe { SecItemCopyMatching(q.to_immutable().as_concrete_TypeRef(), &mut out) };
            if status == errSecItemNotFound {
                return Ok(Vec::new());
            }
            if status != errSecSuccess {
                if !out.is_null() {
                    drop(unsafe { CFType::wrap_under_create_rule(out) });
                }
                return Err(status);
            }
            if out.is_null() {
                return Ok(Vec::new());
            }
            // SAFETY: kSecMatchLimitAll with kSecReturnAttributes returns a +1
            // CFArray of CFDictionary.
            let items = unsafe { CFArray::<CFDictionary>::wrap_under_create_rule(out.cast()) };
            let account_key = unsafe { CFString::wrap_under_get_rule(kSecAttrAccount) };
            let mut found = Vec::with_capacity(items.len() as usize);
            for item in items.iter() {
                if let Some(v) = item.find(account_key.to_void()) {
                    // SAFETY: kSecAttrAccount is always a CFString.
                    found.push(unsafe { CFString::wrap_under_get_rule((*v).cast()) }.to_string());
                }
            }
            Ok(found)
        })?;
        accounts.sort_unstable();
        accounts.dedup();
        Ok(accounts)
    }

    fn backend(&self) -> CredentialBackend {
        CredentialBackend::MacosKeychain
    }
}

/// `kSecValueData`, as a void-pointer key.
fn value_key() -> *const c_void {
    unsafe { kSecValueData }.to_void()
}

/// `kSecAttrAccessibleWhenUnlockedThisDeviceOnly`, as a void-pointer value.
fn accessible_value() -> *const c_void {
    unsafe { kSecAttrAccessibleWhenUnlockedThisDeviceOnly }.to_void()
}

/// The attributes that identify one item, plus the one that keeps it off
/// iCloud. See the module docs for why `kSecAttrSynchronizable` is on queries.
fn base_query(
    service: &str,
    account: &str,
    scope: Scope,
) -> CFMutableDictionary<*const c_void, *const c_void> {
    let mut d = CFMutableDictionary::new();
    unsafe {
        d.add(&kSecClass.to_void(), &kSecClassGenericPassword.to_void());
        d.add(
            &kSecAttrService.to_void(),
            &CFString::new(service).to_void(),
        );
        d.add(
            &kSecAttrAccount.to_void(),
            &CFString::new(account).to_void(),
        );
        d.add(
            &kSecAttrSynchronizable.to_void(),
            &CFBoolean::false_value().to_void(),
        );
        if scope == Scope::DataProtection {
            d.add(
                &kSecUseDataProtectionKeychain.to_void(),
                &CFBoolean::true_value().to_void(),
            );
        }
    }
    d
}

/// Map an `OSStatus` onto the platform error taxonomy. The four that are
/// *answers* rather than failures — cancelled, locked, unentitled, unavailable
/// — are classified rather than folded into `Os`.
fn classify(status: OSStatus) -> PlatformError {
    match status {
        ERR_SEC_USER_CANCELED => PlatformError::Cancelled,
        ERR_SEC_INTERACTION_NOT_ALLOWED => PlatformError::PermissionDenied(
            "the keychain is locked and this session cannot prompt".to_owned(),
        ),
        ERR_SEC_MISSING_ENTITLEMENT | ERR_SEC_NOT_AVAILABLE => {
            PlatformError::BackendUnavailable(describe(status))
        }
        s if s == errSecAuthFailed => PlatformError::PermissionDenied(describe(status)),
        s => PlatformError::Os {
            code: i64::from(s),
            message: describe(s),
        },
    }
}

fn describe(status: OSStatus) -> String {
    SecError::from_code(status).to_string()
}
