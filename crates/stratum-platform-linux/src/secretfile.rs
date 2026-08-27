//! The `EncryptedFile` credential fallback — 08 §5.3.
//!
//! AES-256-GCM over a file at `state_dir/credentials.enc`, with the key derived
//! from a machine-bound secret through Argon2id. This is what answers when
//! there is no `org.freedesktop.secrets` on the session bus, which on Linux is
//! not an exotic configuration: a plain i3/sway session, a server with no
//! keyring daemon, an SSH-forwarded session, and a Docker container all lack
//! one, and W24's acceptance makes that a **first-class expected state**.
//!
//! # What this protects against, stated honestly
//!
//! The file is readable by the user, and so is `/etc/machine-id`. Anything
//! running as the user can therefore recover the key. What the encryption buys
//! is that the file is useless **off this machine** — in a backup, in a synced
//! `~`, in a support bundle, on a stolen disk that is not also the running
//! machine. That is a real and common threat for an API key, and it is the
//! whole of what we claim. [`stratum_platform::CredentialBackend::is_os_store`]
//! returns false for this backend precisely so the Settings pane can say so
//! (§22); the UI must not describe it as equivalent to the Keychain.
//!
//! # Format
//!
//! ```text
//! 0..11   b"STRATUMCRED"
//! 11      format version, currently 1
//! 12..44  Argon2id salt, 32 random bytes, generated once per file
//! 44..56  AES-GCM nonce, 12 random bytes, REGENERATED ON EVERY WRITE
//! 56..    ciphertext ‖ 16-byte tag
//! ```
//!
//! The header is authenticated as associated data, so a downgrade of the
//! version byte or a swap of the salt fails the tag check rather than silently
//! decrypting under a different key. The nonce is fresh per write because
//! GCM nonce reuse under one key is a catastrophic failure, not a degradation:
//! two messages under the same (key, nonce) leak their XOR and the
//! authentication key.

use std::collections::BTreeMap;
use std::sync::Mutex;

// `aead::Nonce<A>` (parameterised by the cipher) rather than `aes_gcm::Nonce<N>`
// (parameterised by the nonce SIZE): the two aliases share a name, and the one
// that takes a size silently accepts the cipher type and then fails deep inside
// hybrid-array with an unreadable bound error.
use aes_gcm::aead::{Aead, Generate, Nonce, Payload};
use aes_gcm::{Aes256Gcm, Key, KeyInit};
use camino::{Utf8Path, Utf8PathBuf};
use stratum_platform::{ExposeSecret, PlatformError, Result, SecretString};

/// File magic. Eleven bytes so the version byte lands on 11 and the whole
/// header is a round 56.
const MAGIC: &[u8; 11] = b"STRATUMCRED";
/// Format version. Bumping this is a migration, and the version byte is inside
/// the authenticated header so an old build cannot be tricked into reading a
/// new file as if it were its own.
const VERSION: u8 = 1;
const SALT_LEN: usize = 32;
const NONCE_LEN: usize = 12;
const HEADER_LEN: usize = MAGIC.len() + 1 + SALT_LEN + NONCE_LEN;

/// The Argon2 context string. Mixed into the key material so that the same
/// machine id used for another purpose cannot produce the same key.
const KDF_CONTEXT: &[u8] = b"dev.stratum.app/credentials/v1";

/// One `(service, account)` pair. `BTreeMap` and not a hash map: the plaintext
/// is re-serialised on every write, and a stable order means a write that
/// changes nothing produces identical bytes — which is what makes "did this
/// actually change?" answerable by looking at the file.
type Entries = BTreeMap<(String, String), String>;

/// The AES-256-GCM credential file.
#[derive(Debug)]
pub struct EncryptedFileStore {
    path: Utf8PathBuf,
    /// Machine-bound key material, before the Argon2 pass.
    machine_secret: Vec<u8>,
    /// `(salt, derived key)`. Argon2id at the default parameters is ~50 ms of
    /// deliberate work; doing it on every `get` would put 50 ms into the AI
    /// settings pane's first paint and into every provider call. Derived once
    /// per salt and held for the process lifetime.
    cached_key: Mutex<Option<([u8; SALT_LEN], Key<Aes256Gcm>)>>,
}

impl EncryptedFileStore {
    /// Construct over `path`, keyed by `machine_secret`.
    ///
    /// `machine_secret` is an argument rather than something this type reads
    /// for itself so that the round-trip, the wrong-key refusal and the
    /// tamper-detection can all be asserted on any host — none of them is
    /// Linux-specific, and none of them should be untested until someone runs
    /// CI on a machine with no keyring.
    #[must_use]
    pub fn new(path: impl Into<Utf8PathBuf>, machine_secret: Vec<u8>) -> Self {
        Self {
            path: path.into(),
            machine_secret,
            cached_key: Mutex::new(None),
        }
    }

    /// Where the file is.
    #[must_use]
    pub fn path(&self) -> &Utf8Path {
        &self.path
    }

    /// Read a secret.
    ///
    /// # Errors
    /// See [`EncryptedFileStore::load`].
    pub fn get(&self, service: &str, account: &str) -> Result<Option<SecretString>> {
        let entries = self.load()?;
        Ok(entries
            .get(&(service.to_owned(), account.to_owned()))
            .map(|v| SecretString::from(v.clone())))
    }

    /// Write a secret, replacing any existing one for the pair.
    ///
    /// # Errors
    /// See [`EncryptedFileStore::load`]; plus [`PlatformError::Io`] on a write
    /// failure.
    pub fn set(&self, service: &str, account: &str, secret: &SecretString) -> Result<()> {
        let mut entries = self.load()?;
        entries.insert(
            (service.to_owned(), account.to_owned()),
            secret.expose_secret().to_owned(),
        );
        self.store(&entries)
    }

    /// Remove a secret. Removing one that is not there is `Ok(())`: the caller
    /// asked for a state, not for an event.
    ///
    /// # Errors
    /// As [`EncryptedFileStore::set`].
    pub fn delete(&self, service: &str, account: &str) -> Result<()> {
        let mut entries = self.load()?;
        if entries
            .remove(&(service.to_owned(), account.to_owned()))
            .is_none()
        {
            return Ok(());
        }
        self.store(&entries)
    }

    /// Every account under `service`, sorted, without decrypting into a
    /// `String` any value the caller did not ask for.
    ///
    /// # Errors
    /// See [`EncryptedFileStore::load`].
    pub fn list_accounts(&self, service: &str) -> Result<Vec<String>> {
        let entries = self.load()?;
        // Already sorted: `Entries` is a BTreeMap keyed by (service, account).
        Ok(entries
            .keys()
            .filter(|(s, _)| s == service)
            .map(|(_, a)| a.clone())
            .collect())
    }

    /// Decrypt the whole file. An absent file is an empty store, which is the
    /// normal state before the user has entered their first key.
    ///
    /// # Errors
    /// [`PlatformError::BackendUnavailable`] when the file is not one of ours
    /// or is truncated; [`PlatformError::PermissionDenied`] when the tag check
    /// fails, which means the machine id changed or the file came from another
    /// machine; [`PlatformError::Io`] otherwise.
    pub fn load(&self) -> Result<Entries> {
        let raw = match std::fs::read(&self.path) {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Entries::new()),
            Err(e) => return Err(PlatformError::Io(e)),
        };
        if raw.len() < HEADER_LEN {
            return Err(PlatformError::BackendUnavailable(format!(
                "{} is too short to be a credential store",
                self.path
            )));
        }
        if &raw[..MAGIC.len()] != MAGIC || raw[MAGIC.len()] != VERSION {
            return Err(PlatformError::BackendUnavailable(format!(
                "{} was not written by this version of Stratum",
                self.path
            )));
        }
        let (header, body) = raw.split_at(HEADER_LEN);
        let mut salt = [0u8; SALT_LEN];
        salt.copy_from_slice(&header[MAGIC.len() + 1..MAGIC.len() + 1 + SALT_LEN]);
        let nonce = Nonce::<Aes256Gcm>::try_from(&header[MAGIC.len() + 1 + SALT_LEN..])
            .map_err(|_| PlatformError::BackendUnavailable("malformed nonce".to_owned()))?;

        let key = self.derive(&salt)?;
        let plain = Aes256Gcm::new(&key)
            .decrypt(
                &nonce,
                Payload {
                    msg: body,
                    aad: header,
                },
            )
            .map_err(|_| {
                PlatformError::PermissionDenied(format!(
                    "{} could not be decrypted on this machine; it was written elsewhere, \
                     or /etc/machine-id changed",
                    self.path
                ))
            })?;
        decode(&plain).ok_or_else(|| {
            PlatformError::BackendUnavailable(format!("{} decrypted to garbage", self.path))
        })
    }

    /// Encrypt and replace the file, with a fresh nonce.
    fn store(&self, entries: &Entries) -> Result<()> {
        let salt = match *self.cached_key.lock().map_err(poisoned)? {
            Some((salt, _)) => salt,
            // First write to a file that does not exist yet.
            None => <[u8; SALT_LEN] as Generate>::try_generate()
                .map_err(|e| PlatformError::BackendUnavailable(format!("no system RNG: {e}")))?,
        };
        let nonce = Nonce::<Aes256Gcm>::try_generate()
            .map_err(|e| PlatformError::BackendUnavailable(format!("no system RNG: {e}")))?;

        let mut out = Vec::with_capacity(HEADER_LEN + 256);
        out.extend_from_slice(MAGIC);
        out.push(VERSION);
        out.extend_from_slice(&salt);
        out.extend_from_slice(&nonce);
        let header_len = out.len();

        let key = self.derive(&salt)?;
        let plain = encode(entries);
        let sealed = Aes256Gcm::new(&key)
            .encrypt(
                &nonce,
                Payload {
                    msg: &plain,
                    aad: &out[..header_len],
                },
            )
            .map_err(|_| {
                PlatformError::BackendUnavailable("the credential store could not be sealed".into())
            })?;
        out.extend_from_slice(&sealed);

        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        write_private(&self.path, &out)
    }

    /// Argon2id over the machine secret, cached per salt. See the field docs
    /// for why the cache is not an optimisation but a latency requirement.
    fn derive(&self, salt: &[u8; SALT_LEN]) -> Result<Key<Aes256Gcm>> {
        let mut cache = self.cached_key.lock().map_err(poisoned)?;
        if let Some((cached_salt, key)) = cache.as_ref() {
            if cached_salt == salt {
                return Ok(*key);
            }
        }
        let mut material = Vec::with_capacity(KDF_CONTEXT.len() + self.machine_secret.len());
        material.extend_from_slice(KDF_CONTEXT);
        material.extend_from_slice(&self.machine_secret);

        let mut raw = [0u8; 32];
        argon2::Argon2::default()
            .hash_password_into(&material, salt, &mut raw)
            .map_err(|e| {
                PlatformError::BackendUnavailable(format!("credential key derivation failed: {e}"))
            })?;
        let key = Key::<Aes256Gcm>::from(raw);
        *cache = Some((*salt, key));
        Ok(key)
    }
}

/// A poisoned mutex means another thread panicked while holding the credential
/// map. Reporting it beats `unwrap`, which is denied in this crate.
fn poisoned<T>(_: std::sync::PoisonError<T>) -> PlatformError {
    PlatformError::BackendUnavailable("the credential store lock was poisoned".to_owned())
}

/// Write `bytes` to `path` with mode 0600, through a temporary file in the same
/// directory so a crash mid-write cannot leave a half-encrypted store.
fn write_private(path: &Utf8Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;

    let tmp = path.with_extension("enc.tmp");
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Before any byte is written, not with a chmod afterwards: the window
        // between create and chmod is a window in which the file is 0644.
        opts.mode(0o600);
    }
    let mut f = opts.open(&tmp)?;
    f.write_all(bytes)?;
    // The rename below is atomic, but only orders against data that has
    // reached the disk. Without this a power loss can leave the new name
    // pointing at a zero-length file, i.e. every stored key gone.
    f.sync_all()?;
    drop(f);
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// `count ‖ (len ‖ service, len ‖ account, len ‖ secret)*`, all lengths u32 LE.
///
/// Hand-rolled rather than JSON: this buffer holds plaintext API keys, and a
/// serialiser that can reallocate its scratch space leaves copies of them
/// around. One `Vec` we sized ourselves is one buffer to think about.
fn encode(entries: &Entries) -> Vec<u8> {
    let size: usize = 4 + entries
        .iter()
        .map(|((s, a), v)| 12 + s.len() + a.len() + v.len())
        .sum::<usize>();
    let mut out = Vec::with_capacity(size);
    out.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    for ((service, account), secret) in entries {
        for field in [service.as_str(), account.as_str(), secret.as_str()] {
            out.extend_from_slice(&(field.len() as u32).to_le_bytes());
            out.extend_from_slice(field.as_bytes());
        }
    }
    out
}

/// The inverse. `None` on any inconsistency — a truncated length, a field that
/// runs past the end, trailing bytes. The tag check has already passed by the
/// time this runs, so a failure here means our own format changed, not an
/// attacker.
fn decode(bytes: &[u8]) -> Option<Entries> {
    let mut cur = 0usize;
    let mut take = |n: usize| -> Option<&[u8]> {
        let end = cur.checked_add(n)?;
        let slice = bytes.get(cur..end)?;
        cur = end;
        Some(slice)
    };
    let count = u32::from_le_bytes(take(4)?.try_into().ok()?);
    let mut out = Entries::new();
    for _ in 0..count {
        let mut field = || -> Option<String> {
            let len = u32::from_le_bytes(take(4)?.try_into().ok()?) as usize;
            String::from_utf8(take(len)?.to_vec()).ok()
        };
        let service = field()?;
        let account = field()?;
        let secret = field()?;
        out.insert((service, account), secret);
    }
    (cur == bytes.len()).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wire_form_round_trips_including_empty_and_unicode_fields() {
        let mut e = Entries::new();
        e.insert(
            ("dev.stratum.app/ai-provider".into(), "openai".into()),
            "sk-\u{2713}".into(),
        );
        e.insert(("s".into(), "".into()), String::new());
        assert_eq!(decode(&encode(&e)), Some(e));
    }

    #[test]
    fn a_truncated_buffer_decodes_to_nothing_rather_than_a_partial_map() {
        let mut e = Entries::new();
        e.insert(("s".into(), "a".into()), "v".into());
        let full = encode(&e);
        assert_eq!(decode(&full[..full.len() - 1]), None);
        // Trailing junk is a format error too: silently ignoring it is how a
        // format grows an undocumented extension.
        let mut extra = full;
        extra.push(0);
        assert_eq!(decode(&extra), None);
    }

    #[test]
    fn an_empty_map_is_a_valid_encoding_and_is_not_an_empty_buffer() {
        let encoded = encode(&Entries::new());
        assert_eq!(encoded, vec![0, 0, 0, 0]);
        assert_eq!(decode(&encoded), Some(Entries::new()));
    }
}
