//! Credential storage on Linux — 08 §5.3, spec §22/§27.
//!
//! W24's acceptance, verbatim: **"Secret Service absence is a first-class
//! expected state, never a crash."** This module is that sentence as code.
//!
//! # The demotion, and why it happens exactly once
//!
//! We start on `org.freedesktop.secrets` and demote **permanently, on the first
//! failure that means "there is no keyring here"**, to
//! [`crate::secretfile::EncryptedFileStore`]. Permanently, because the
//! alternative — re-probing per call — costs a D-Bus round-trip and a service
//! activation attempt on every single credential read, on precisely the
//! machines that have no keyring to activate. `tests/credentials.rs` asserts
//! that as a counter: a session with no Secret Service pays **one** probe for
//! the life of the process, not one per key.
//!
//! The demotion is one-way on purpose. A keyring that appears mid-session
//! (the user starts `gnome-keyring-daemon` by hand) would otherwise silently
//! change where the next key is written, leaving half the user's providers in
//! one store and half in the other — and [`CredentialStore::backend`], which
//! §22 renders as a privacy statement, would be telling the truth about only
//! some of them.
//!
//! # Why `backend()` never says `KWallet`
//!
//! We speak `org.freedesktop.secrets`, and KDE's `kwalletd` implements exactly
//! that interface. Which daemon owns the bus name is not observable without
//! inspecting the owner's process, and it does not change the guarantee: the
//! secret is in an OS-managed store either way. Reporting
//! [`CredentialBackend::KWallet`] would additionally make
//! [`CredentialBackend::is_os_store`] answer `false` for a KDE user's wallet,
//! which would put a "weaker than an OS store" warning in front of someone
//! whose secret is in an OS store.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use camino::Utf8PathBuf;
use stratum_platform::{
    CredentialBackend, CredentialStore, Env, PlatformError, Result, SecretString,
};

use crate::secretfile::EncryptedFileStore;

/// The Secret Service half — `org.freedesktop.secrets` over D-Bus.
///
/// A trait rather than a concrete type so that the demotion policy above is
/// tested against an absent, a broken and a working keyring on any host. The
/// D-Bus implementation is [`crate::secretservice::SecretServiceClient`].
pub trait SecretStore: Send + Sync {
    /// Fetch. `Ok(None)` is "no such item".
    ///
    /// # Errors
    /// [`PlatformError::BackendUnavailable`] when the service is not on the
    /// bus — the one that triggers demotion.
    fn get(&self, service: &str, account: &str) -> Result<Option<SecretString>>;

    /// Store, replacing any existing item for the pair.
    ///
    /// # Errors
    /// As [`SecretStore::get`].
    fn set(&self, service: &str, account: &str, secret: &SecretString) -> Result<()>;

    /// Remove. Removing an absent item is `Ok(())`.
    ///
    /// # Errors
    /// As [`SecretStore::get`].
    fn delete(&self, service: &str, account: &str) -> Result<()>;

    /// Every account under `service`, sorted.
    ///
    /// # Errors
    /// As [`SecretStore::get`].
    fn list_accounts(&self, service: &str) -> Result<Vec<String>>;
}

/// [`CredentialStore`] for Linux.
pub struct LinuxCredentials {
    keyring: Option<Arc<dyn SecretStore>>,
    file: EncryptedFileStore,
    demoted: AtomicBool,
    keyring_calls: AtomicU64,
}

impl std::fmt::Debug for LinuxCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LinuxCredentials")
            .field("has_keyring", &self.keyring.is_some())
            .field("demoted", &self.demoted.load(Ordering::Relaxed))
            .field("keyring_calls", &self.keyring_calls.load(Ordering::Relaxed))
            .field("file", &self.file.path())
            .finish()
    }
}

impl LinuxCredentials {
    /// With a Secret Service client and an encrypted-file fallback.
    #[must_use]
    pub fn new(keyring: Option<Arc<dyn SecretStore>>, file: EncryptedFileStore) -> Self {
        Self {
            keyring,
            file,
            // A build with no keyring client at all starts demoted; there is
            // nothing to probe and no reason to pretend otherwise.
            demoted: AtomicBool::new(false),
            keyring_calls: AtomicU64::new(0),
        }
    }

    /// How many calls have reached the Secret Service. The counter that stands
    /// in for "a missing keyring is probed once, not once per key" — see the
    /// module docs.
    #[must_use]
    pub fn keyring_calls(&self) -> u64 {
        self.keyring_calls.load(Ordering::Relaxed)
    }

    /// Whether the fallback has taken over.
    #[must_use]
    pub fn is_demoted(&self) -> bool {
        self.keyring.is_none() || self.demoted.load(Ordering::Relaxed)
    }

    /// Whether this failure means "there is no keyring in this session".
    ///
    /// [`PlatformError::PermissionDenied`] deliberately does NOT: a locked
    /// keyring, or a user who pressed Deny on the unlock prompt, is a keyring
    /// that exists and said no. Writing their API key to a file instead would
    /// route around a decision the user just made.
    #[must_use]
    pub fn is_absent(e: &PlatformError) -> bool {
        matches!(
            e,
            PlatformError::BackendUnavailable(_) | PlatformError::Unsupported(_)
        )
    }

    /// Run `on_keyring` if we still have one, demoting to `on_file` when the
    /// keyring turns out not to be there.
    fn dispatch<T>(
        &self,
        on_keyring: impl FnOnce(&dyn SecretStore) -> Result<T>,
        on_file: impl FnOnce(&EncryptedFileStore) -> Result<T>,
    ) -> Result<T> {
        if !self.is_demoted() {
            if let Some(k) = self.keyring.as_deref() {
                self.keyring_calls.fetch_add(1, Ordering::Relaxed);
                match on_keyring(k) {
                    Ok(v) => return Ok(v),
                    Err(e) if Self::is_absent(&e) => {
                        self.demoted.store(true, Ordering::Relaxed);
                    }
                    Err(e) => return Err(e),
                }
            }
        }
        on_file(&self.file)
    }
}

impl CredentialStore for LinuxCredentials {
    fn get(&self, service: &str, account: &str) -> Result<Option<SecretString>> {
        self.dispatch(|k| k.get(service, account), |f| f.get(service, account))
    }

    fn set(&self, service: &str, account: &str, secret: &SecretString) -> Result<()> {
        self.dispatch(
            |k| k.set(service, account, secret),
            |f| f.set(service, account, secret),
        )
    }

    fn delete(&self, service: &str, account: &str) -> Result<()> {
        self.dispatch(
            |k| k.delete(service, account),
            |f| f.delete(service, account),
        )
    }

    fn list_accounts(&self, service: &str) -> Result<Vec<String>> {
        self.dispatch(|k| k.list_accounts(service), |f| f.list_accounts(service))
    }

    fn backend(&self) -> CredentialBackend {
        if self.is_demoted() {
            CredentialBackend::EncryptedFile
        } else {
            CredentialBackend::SecretService
        }
    }
}

/// Where the encrypted fallback lives: `state_dir/credentials.enc` (08 §5.3).
#[must_use]
pub fn fallback_path(state_dir: &camino::Utf8Path) -> Utf8PathBuf {
    state_dir.join("credentials.enc")
}

/// Key material bound to this machine and this user.
///
/// `/etc/machine-id` is the identifier every modern Linux has, `systemd` keeps
/// it stable across reboots, and the D-Bus machine id is the fallback for the
/// handful of systems that predate it. The uid is mixed in so two accounts on
/// one machine cannot decrypt each other's file even after copying it, and the
/// constant is there so that an empty machine id still produces a key rather
/// than an empty Argon2 password.
///
/// A missing machine id is not fatal — the file simply becomes bound to the
/// hostname and uid instead, which is weaker but still not portable. Refusing
/// to store the user's API key at all because `/etc/machine-id` is absent would
/// be the wrong trade.
#[must_use]
pub fn machine_secret(env: &dyn Env) -> Vec<u8> {
    let mut parts: Vec<String> = Vec::with_capacity(4);
    for path in ["/etc/machine-id", "/var/lib/dbus/machine-id"] {
        if let Ok(id) = std::fs::read_to_string(path) {
            let id = id.trim().to_owned();
            if !id.is_empty() {
                parts.push(id);
                break;
            }
        }
    }
    if let Ok(host) = std::fs::read_to_string("/etc/hostname") {
        let host = host.trim().to_owned();
        if !host.is_empty() {
            parts.push(host);
        }
    }
    if let Some(user) = env.var("USER").or_else(|| env.var("LOGNAME")) {
        parts.push(user);
    }
    // `target_os` rather than `unix`: `libc` is a Linux-only dependency of this
    // crate, and this function compiles on every host so that its tests do.
    #[cfg(target_os = "linux")]
    {
        // SAFETY: `getuid` takes no arguments, cannot fail, and touches no
        // shared state. It is the one input here that no missing file can
        // remove.
        parts.push(unsafe { libc::getuid() }.to_string());
    }
    if parts.is_empty() {
        // Never an empty password. A file encrypted under the empty string is
        // a file encrypted under a value an attacker can guess in one try.
        parts.push("no-machine-identity".to_owned());
    }
    parts.join("\u{1f}").into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A [`SecretStore`] that reports whatever it was told to, and counts.
    struct Fake {
        answer: fn() -> Result<Option<SecretString>>,
        calls: AtomicU64,
    }

    impl SecretStore for Fake {
        fn get(&self, _s: &str, _a: &str) -> Result<Option<SecretString>> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            (self.answer)()
        }
        fn set(&self, _s: &str, _a: &str, _v: &SecretString) -> Result<()> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            (self.answer)().map(|_| ())
        }
        fn delete(&self, _s: &str, _a: &str) -> Result<()> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            (self.answer)().map(|_| ())
        }
        fn list_accounts(&self, _s: &str) -> Result<Vec<String>> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            (self.answer)().map(|_| Vec::new())
        }
    }

    /// A locked keyring said no. That is an answer, not an absence, and routing
    /// around it would write the user's key to a file after they declined.
    #[test]
    fn a_locked_keyring_is_not_an_absent_one() {
        assert!(!LinuxCredentials::is_absent(
            &PlatformError::PermissionDenied("locked".to_owned())
        ));
        assert!(!LinuxCredentials::is_absent(&PlatformError::Cancelled));
        assert!(LinuxCredentials::is_absent(
            &PlatformError::BackendUnavailable("no such name on the bus".to_owned())
        ));
        assert!(LinuxCredentials::is_absent(&PlatformError::Unsupported(
            "no session bus"
        )));
    }

    #[test]
    fn with_no_keyring_client_at_all_the_backend_is_the_file_from_the_start() {
        let store = LinuxCredentials::new(
            None,
            EncryptedFileStore::new("/nonexistent/credentials.enc", b"k".to_vec()),
        );
        assert!(store.is_demoted());
        assert_eq!(store.backend(), CredentialBackend::EncryptedFile);
        assert_eq!(store.keyring_calls(), 0);
    }

    #[test]
    fn a_working_keyring_is_reported_as_the_os_store() {
        let fake = Arc::new(Fake {
            answer: || Ok(None),
            calls: AtomicU64::new(0),
        });
        let store = LinuxCredentials::new(
            Some(fake),
            EncryptedFileStore::new("/nonexistent/credentials.enc", b"k".to_vec()),
        );
        assert_eq!(store.backend(), CredentialBackend::SecretService);
        assert!(CredentialBackend::SecretService.is_os_store());
    }
}
