//! W24's credential acceptance: **"Secret Service absence is a first-class
//! expected state, never a crash."**
//!
//! Three things are asserted here, and the third is the counter (ADR-017):
//!
//! 1. A session with no keyring still stores and retrieves secrets, through
//!    the encrypted file, and `backend()` says so — which is what makes §22's
//!    privacy statement in Settings true rather than decorative.
//! 2. A keyring that exists and **refused** is not routed around. A locked
//!    keyring, or a user who pressed Deny, made a decision.
//! 3. **A missing keyring costs exactly one probe for the life of the
//!    process**, not one per credential read. Ten reads on a keyring-less box
//!    perform one D-Bus attempt, and that is asserted as a count.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use camino::Utf8PathBuf;
use stratum_platform::{
    credentials::{service, PURPOSE_AI_PROVIDER},
    CredentialBackend, CredentialStore, ExposeSecret, PlatformError, Result, SecretString,
};
use stratum_platform_linux::{EncryptedFileStore, LinuxCredentials, SecretStore};

/// A keyring that always answers the same way, and counts every call.
struct Keyring {
    answer: fn() -> PlatformError,
    calls: AtomicU64,
}

impl Keyring {
    fn new(answer: fn() -> PlatformError) -> Arc<Self> {
        Arc::new(Self {
            answer,
            calls: AtomicU64::new(0),
        })
    }
    fn fail(&self) -> PlatformError {
        self.calls.fetch_add(1, Ordering::Relaxed);
        (self.answer)()
    }
}

impl SecretStore for Keyring {
    fn get(&self, _s: &str, _a: &str) -> Result<Option<SecretString>> {
        Err(self.fail())
    }
    fn set(&self, _s: &str, _a: &str, _v: &SecretString) -> Result<()> {
        Err(self.fail())
    }
    fn delete(&self, _s: &str, _a: &str) -> Result<()> {
        Err(self.fail())
    }
    fn list_accounts(&self, _s: &str) -> Result<Vec<String>> {
        Err(self.fail())
    }
}

fn store_in(dir: &camino::Utf8Path) -> EncryptedFileStore {
    EncryptedFileStore::new(dir.join("credentials.enc"), b"machine-id/uid".to_vec())
}

fn tmp() -> (tempfile::TempDir, Utf8PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
    (dir, path)
}

/// THE ACCEPTANCE BULLET. No keyring anywhere, and the user's API key is still
/// stored, retrieved, listed and deleted — with the backend reported honestly.
#[test]
fn with_no_secret_service_everything_still_works_through_the_file() {
    let (_guard, dir) = tmp();
    let keyring = Keyring::new(|| PlatformError::BackendUnavailable("no such name".to_owned()));
    let creds = LinuxCredentials::new(Some(keyring.clone()), store_in(&dir));

    let svc = service(PURPOSE_AI_PROVIDER);
    assert_eq!(creds.backend(), CredentialBackend::SecretService);
    assert!(creds.get(&svc, "openai").unwrap().is_none());
    // The first call demoted us; from here the file is authoritative.
    assert_eq!(creds.backend(), CredentialBackend::EncryptedFile);
    assert!(!CredentialBackend::EncryptedFile.is_os_store());

    creds
        .set(
            &svc,
            "openai",
            &SecretString::from("sk-test-value".to_owned()),
        )
        .unwrap();
    creds
        .set(
            &svc,
            "anthropic",
            &SecretString::from("sk-ant-value".to_owned()),
        )
        .unwrap();

    let got = creds.get(&svc, "openai").unwrap().unwrap();
    assert_eq!(got.expose_secret(), "sk-test-value");
    assert_eq!(creds.list_accounts(&svc).unwrap(), ["anthropic", "openai"]);

    creds.delete(&svc, "openai").unwrap();
    assert!(creds.get(&svc, "openai").unwrap().is_none());
    assert_eq!(creds.list_accounts(&svc).unwrap(), ["anthropic"]);
    // Deleting what is already gone is a state, not an event.
    assert!(creds.delete(&svc, "openai").is_ok());
}

/// THE COUNTER (ADR-017). A keyring-less session probes once, not once per
/// read. The alternative costs a D-Bus round trip and a service-activation
/// attempt on every single credential access, on exactly the machines that
/// have nothing to activate.
#[test]
fn a_missing_keyring_is_probed_once_for_the_life_of_the_process() {
    let (_guard, dir) = tmp();
    let keyring = Keyring::new(|| PlatformError::BackendUnavailable("no such name".to_owned()));
    let creds = LinuxCredentials::new(Some(keyring.clone()), store_in(&dir));
    let svc = service(PURPOSE_AI_PROVIDER);

    for _ in 0..10 {
        let _ = creds.get(&svc, "openai");
        let _ = creds.list_accounts(&svc);
    }
    assert_eq!(
        keyring.calls.load(Ordering::Relaxed),
        1,
        "20 credential operations must cost exactly one keyring probe"
    );
    assert_eq!(creds.keyring_calls(), 1);
    assert!(creds.is_demoted());
}

/// A locked keyring is a keyring that exists and said no. Writing the user's
/// API key to a file instead would route around a decision they just made.
#[test]
fn a_refusal_is_reported_rather_than_worked_around() {
    let (_guard, dir) = tmp();
    let keyring = Keyring::new(|| PlatformError::PermissionDenied("locked".to_owned()));
    let creds = LinuxCredentials::new(Some(keyring.clone()), store_in(&dir));
    let svc = service(PURPOSE_AI_PROVIDER);

    let err = creds.get(&svc, "openai").unwrap_err();
    assert!(matches!(err, PlatformError::PermissionDenied(_)), "{err}");
    assert!(!creds.is_demoted());
    assert_eq!(creds.backend(), CredentialBackend::SecretService);

    // Still not demoted after repeated refusals, and still asking the keyring.
    let _ = creds.get(&svc, "openai");
    assert_eq!(keyring.calls.load(Ordering::Relaxed), 2);
}

/// The file store on its own: a round trip, an absent file, and the two
/// failure modes that must be distinguishable.
#[test]
fn the_encrypted_file_round_trips_and_is_bound_to_its_machine() {
    let (_guard, dir) = tmp();
    let a = store_in(&dir);
    assert!(
        a.get("s", "acct").unwrap().is_none(),
        "an absent file is empty"
    );

    a.set("s", "acct", &SecretString::from("value".to_owned()))
        .unwrap();
    assert!(a.path().exists());
    let raw = std::fs::read(a.path()).unwrap();
    assert!(raw.starts_with(b"STRATUMCRED"));
    // The whole point: the secret is not in the file.
    assert!(
        !String::from_utf8_lossy(&raw).contains("value"),
        "the plaintext must not be recoverable from the file"
    );

    // A fresh handle, same machine secret: reads it back.
    let b = store_in(&dir);
    assert_eq!(
        b.get("s", "acct").unwrap().unwrap().expose_secret(),
        "value"
    );

    // A different machine: the file is useless, and says why rather than
    // pretending to be empty.
    let elsewhere = EncryptedFileStore::new(dir.join("credentials.enc"), b"other-machine".to_vec());
    let err = elsewhere.get("s", "acct").unwrap_err();
    assert!(
        matches!(err, PlatformError::PermissionDenied(ref m) if m.contains("machine")),
        "{err}"
    );
}

/// A file that is not ours, and a file that has been tampered with, are
/// different failures and must not be reported as "no credentials stored".
#[test]
fn a_foreign_or_tampered_file_is_reported_not_silently_ignored() {
    let (_guard, dir) = tmp();
    let path = dir.join("credentials.enc");

    std::fs::write(&path, b"this is somebody else's file entirely").unwrap();
    let s = EncryptedFileStore::new(path.clone(), b"k".to_vec());
    let err = s.get("s", "a").unwrap_err();
    assert!(matches!(err, PlatformError::BackendUnavailable(_)), "{err}");

    // A real file with one ciphertext byte flipped: the GCM tag must catch it.
    // In a fresh directory, because the foreign file above is not a store this
    // one could load in order to rewrite.
    let (_guard2, dir) = tmp();
    let path = dir.join("credentials.enc");
    let good = store_in(&dir);
    good.set("s", "a", &SecretString::from("v".to_owned()))
        .unwrap();
    let mut bytes = std::fs::read(&path).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    std::fs::write(&path, &bytes).unwrap();

    let reopened = store_in(&dir);
    let err = reopened.get("s", "a").unwrap_err();
    assert!(matches!(err, PlatformError::PermissionDenied(_)), "{err}");
}

/// Two writes must not reuse the AES-GCM nonce. Reuse under one key is a
/// catastrophic failure rather than a degradation: it leaks the XOR of the two
/// plaintexts and the authentication key.
#[test]
fn every_write_uses_a_fresh_nonce() {
    let (_guard, dir) = tmp();
    let s = store_in(&dir);
    let mut nonces = std::collections::BTreeSet::new();
    for i in 0..8 {
        s.set("s", "a", &SecretString::from(format!("v{i}")))
            .unwrap();
        let raw = std::fs::read(s.path()).unwrap();
        // Header: 11 magic + 1 version + 32 salt, then the 12-byte nonce.
        nonces.insert(raw[44..56].to_vec());
    }
    assert_eq!(nonces.len(), 8, "a nonce was reused across writes");
}

/// The salt, by contrast, is per FILE and must be stable — a new salt on every
/// write means a new Argon2 pass on every read, which is 50 ms of deliberate
/// work in front of the AI settings pane.
#[test]
fn the_salt_is_stable_across_writes() {
    let (_guard, dir) = tmp();
    let s = store_in(&dir);
    s.set("s", "a", &SecretString::from("v".to_owned()))
        .unwrap();
    let first = std::fs::read(s.path()).unwrap()[12..44].to_vec();
    s.set("s", "b", &SecretString::from("w".to_owned()))
        .unwrap();
    let second = std::fs::read(s.path()).unwrap()[12..44].to_vec();
    assert_eq!(first, second);
}
