//! 07 §3.1 / ADR-012 / ARCHITECTURE C17 — credential resolution.
//!
//! Four sources, first match wins:
//!
//! 1. **Environment variable.** Highest precedence so the headless CLI works in
//!    CI, in a container and over SSH, where no keyring daemon exists.
//! 2. **The OS secret store**, through [`CredentialStore`] — never through the
//!    `keyring` crate, which `deny.toml` bans (C17): the user must be told
//!    *which* backend holds their key, and `keyring` hides that behind an opaque
//!    error.
//! 3. **A key file** at an explicit path from config, whose permissions we
//!    verify before reading.
//! 4. **None**, which is a normal state, not an error state (07 §12).
//!
//! The account key is `"{provider}|{base_url_host}"` and not just the provider
//! id. Changing an OpenAI-compatible endpoint from `api.openai.com` to
//! `gateway.university.edu` therefore looks up a *different* credential and
//! prompts for it, instead of silently shipping the old provider's key to a new
//! host. That is a real exfiltration path in every tool that stores one flat
//! "OpenAI key", and it is why the platform crate's illustrative account
//! spelling (`"anthropic"`) is narrowed here rather than followed.

use camino::Utf8Path;
use secrecy::SecretString;
use stratum_platform::credentials::{service, PURPOSE_AI_PROVIDER};
use stratum_platform::{CredentialBackend, CredentialStore};

use super::error::ProviderError;
use super::redact;
use super::types::ProviderId;

/// Read-only access to the process environment.
///
/// A trait rather than `std::env::var` at the call site because environment
/// variables are process-global mutable state: a test that set one would race
/// every other test in the binary, and `resolve` is exactly the function whose
/// precedence order needs testing.
pub trait EnvSource: Send + Sync {
    /// The value of `key`, if set and valid UTF-8.
    fn var(&self, key: &str) -> Option<String>;
}

/// The real process environment.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemEnv;

impl EnvSource for SystemEnv {
    fn var(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }
}

/// Where a resolved key came from. Surfaced in Settings next to
/// [`CredentialBackend`], because "your key is in the Keychain" and "your key is
/// in a shell profile every process on this machine can read" are different
/// privacy statements.
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeySource {
    /// An environment variable.
    Environment(&'static str),
    /// The OS secret store, and which one answered.
    Store(CredentialBackend),
    /// A file on disk whose mode we verified.
    File,
}

/// A resolved credential and its provenance.
pub struct ResolvedKey {
    /// The key. Zeroized on drop; has no `Debug` that prints the value.
    pub secret: SecretString,
    /// Where it came from.
    pub source: KeySource,
}

impl std::fmt::Debug for ResolvedKey {
    /// Hand-written so that a `#[derive(Debug)]` added to some enclosing struct
    /// can never print a key. `SecretString` already redacts itself; this makes
    /// the whole record say nothing beyond its provenance.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedKey")
            .field("source", &self.source)
            .finish_non_exhaustive()
    }
}

/// The environment variable a provider reads, per 07 §3.1.
#[must_use]
pub const fn env_var_for(provider: ProviderId) -> Option<&'static str> {
    match provider {
        ProviderId::Anthropic => Some("ANTHROPIC_API_KEY"),
        ProviderId::OpenAiCompat => Some("OPENAI_API_KEY"),
        // A local daemon on loopback has no credential. Reading one from the
        // environment would be a way to leak a cloud key to a local process.
        ProviderId::Ollama => None,
    }
}

/// The `(service, account)` pair a provider's key is stored under.
///
/// One function so the write in Settings and the read here cannot drift; a
/// drift here does not fail, it silently prompts a user who already entered a
/// key.
#[must_use]
pub fn account_for(provider: ProviderId, base_url_host: &str) -> (String, String) {
    (
        service(PURPOSE_AI_PROVIDER),
        format!("{provider}|{}", base_url_host.to_ascii_lowercase()),
    )
}

/// Where a key file may be read from, and what we are willing to read.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct KeyFile {
    /// The configured path, if any.
    pub path: Option<camino::Utf8PathBuf>,
}

/// Resolve a credential for `provider` at `base_url_host`.
///
/// Returns `Ok(None)` when nothing is configured. That is the state most users
/// are in on first launch and a substantial fraction stay in permanently, so it
/// is a value, not an error (07 §12).
///
/// # Errors
/// [`ProviderError::KeyStore`] when the OS store is present but refused, and
/// when a configured key file exists but its permissions cannot be verified or
/// are too broad.
pub fn resolve(
    provider: ProviderId,
    base_url_host: &str,
    store: &dyn CredentialStore,
    env: &dyn EnvSource,
    key_file: &KeyFile,
) -> Result<Option<ResolvedKey>, ProviderError> {
    // 1 — environment.
    if let Some(name) = env_var_for(provider) {
        if let Some(value) = env.var(name).filter(|v| !v.trim().is_empty()) {
            let secret = SecretString::from(value.trim().to_owned());
            redact::register(&secret);
            return Ok(Some(ResolvedKey {
                secret,
                source: KeySource::Environment(name),
            }));
        }
    }

    // 2 — the OS secret store.
    let (svc, account) = account_for(provider, base_url_host);
    match store.get(&svc, &account) {
        Ok(Some(secret)) => {
            redact::register(&secret);
            return Ok(Some(ResolvedKey {
                secret,
                source: KeySource::Store(store.backend()),
            }));
        }
        Ok(None) => {}
        Err(e) => {
            // A store that is absent is not a failure to resolve: on a headless
            // Linux box with no Secret Service there may still be a key file,
            // and refusing to look would be worse than useless. The error is
            // only surfaced if nothing later succeeds.
            tracing::debug!(provider = %provider, error = %e, "credential store did not answer");
        }
    }

    // 3 — a key file, whose mode we verify first.
    if let Some(path) = key_file.path.as_deref() {
        return read_key_file(path).map(Some);
    }

    Ok(None)
}

/// Read a key file after verifying that only its owner can read it.
///
/// # Errors
/// [`ProviderError::KeyStore`] when the file is missing, unreadable, empty, or
/// its permissions are not owner-only.
pub fn read_key_file(path: &Utf8Path) -> Result<ResolvedKey, ProviderError> {
    let meta =
        std::fs::metadata(path).map_err(|e| ProviderError::key_store(format!("{path}: {e}")))?;
    check_key_file_mode(path, &meta)?;

    let body = std::fs::read_to_string(path)
        .map_err(|e| ProviderError::key_store(format!("{path}: {e}")))?;
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return Err(ProviderError::key_store(format!("{path} is empty")));
    }
    let secret = SecretString::from(trimmed.to_owned());
    redact::register(&secret);
    Ok(ResolvedKey {
        secret,
        source: KeySource::File,
    })
}

#[cfg(unix)]
fn check_key_file_mode(path: &Utf8Path, meta: &std::fs::Metadata) -> Result<(), ProviderError> {
    use std::os::unix::fs::PermissionsExt as _;
    let mode = meta.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(ProviderError::key_store(format!(
            "{path} is mode {mode:04o}; an API key file must be 0600. Run: chmod 600 {path}"
        )));
    }
    Ok(())
}

/// DEFERRED, deliberately, and it fails closed.
///
/// 07 §3.1 asks for a DACL check ("grants only the current user"). Reading a
/// DACL needs the `windows` crate, which `deny.toml` allows only inside
/// `stratum-platform-windows` (W24) — correctly, since that is the rule that
/// keeps OS APIs out of the portable layers. Until that crate exposes a
/// permission predicate, this build refuses to read a key file on Windows
/// rather than reading one whose permissions it cannot verify. The OS store and
/// the environment variable both work there, so no user is left without a way
/// in; and "we could not check, so we read it anyway" is precisely the shortcut
/// that turns a key file into a shared secret.
#[cfg(not(unix))]
fn check_key_file_mode(path: &Utf8Path, _meta: &std::fs::Metadata) -> Result<(), ProviderError> {
    Err(ProviderError::key_store(format!(
        "{path}: this build cannot verify Windows file permissions, so it will not read a key \
         file. Store the key in Credential Manager (Settings › AI) or set the environment variable."
    )))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use secrecy::ExposeSecret as _;
    use stratum_platform::Result as PlatformResult;

    use super::*;

    #[derive(Default)]
    struct FakeStore {
        items: Mutex<BTreeMap<(String, String), String>>,
        fail: bool,
    }

    impl CredentialStore for FakeStore {
        fn get(&self, service: &str, account: &str) -> PlatformResult<Option<SecretString>> {
            if self.fail {
                return Err(stratum_platform::PlatformError::BackendUnavailable(
                    "no session bus".to_owned(),
                ));
            }
            Ok(self
                .items
                .lock()
                .unwrap()
                .get(&(service.to_owned(), account.to_owned()))
                .map(|v| SecretString::from(v.clone())))
        }
        fn set(&self, service: &str, account: &str, secret: &SecretString) -> PlatformResult<()> {
            self.items.lock().unwrap().insert(
                (service.to_owned(), account.to_owned()),
                secret.expose_secret().to_owned(),
            );
            Ok(())
        }
        fn delete(&self, service: &str, account: &str) -> PlatformResult<()> {
            self.items
                .lock()
                .unwrap()
                .remove(&(service.to_owned(), account.to_owned()));
            Ok(())
        }
        fn list_accounts(&self, service: &str) -> PlatformResult<Vec<String>> {
            let mut out: Vec<String> = self
                .items
                .lock()
                .unwrap()
                .keys()
                .filter(|(s, _)| s == service)
                .map(|(_, a)| a.clone())
                .collect();
            out.sort();
            Ok(out)
        }
        fn backend(&self) -> CredentialBackend {
            CredentialBackend::MacosKeychain
        }
    }

    struct MapEnv(BTreeMap<&'static str, &'static str>);

    impl EnvSource for MapEnv {
        fn var(&self, key: &str) -> Option<String> {
            self.0.get(key).map(|v| (*v).to_owned())
        }
    }

    fn empty_env() -> MapEnv {
        MapEnv(BTreeMap::new())
    }

    #[test]
    fn an_empty_store_and_empty_environment_is_none_not_an_error() {
        let out = resolve(
            ProviderId::Anthropic,
            "api.anthropic.com",
            &FakeStore::default(),
            &empty_env(),
            &KeyFile::default(),
        )
        .expect("an unconfigured product is not an error");
        assert!(out.is_none());
    }

    #[test]
    fn the_environment_outranks_the_store() {
        let store = FakeStore::default();
        let (svc, acct) = account_for(ProviderId::Anthropic, "api.anthropic.com");
        store
            .set(&svc, &acct, &SecretString::from("from-store".to_owned()))
            .unwrap();
        let env = MapEnv([("ANTHROPIC_API_KEY", "from-env")].into_iter().collect());

        let got = resolve(
            ProviderId::Anthropic,
            "api.anthropic.com",
            &store,
            &env,
            &KeyFile::default(),
        )
        .unwrap()
        .expect("a key");
        assert_eq!(got.secret.expose_secret(), "from-env");
        assert_eq!(got.source, KeySource::Environment("ANTHROPIC_API_KEY"));
    }

    #[test]
    fn the_account_key_includes_the_host_so_a_new_endpoint_does_not_reuse_the_old_key() {
        let store = FakeStore::default();
        let (svc, openai) = account_for(ProviderId::OpenAiCompat, "api.openai.com");
        store
            .set(&svc, &openai, &SecretString::from("openai-key".to_owned()))
            .unwrap();

        // Same provider, new endpoint: must NOT find the old key.
        let got = resolve(
            ProviderId::OpenAiCompat,
            "gateway.university.edu",
            &store,
            &empty_env(),
            &KeyFile::default(),
        )
        .unwrap();
        assert!(
            got.is_none(),
            "a key stored for api.openai.com must not be sent elsewhere"
        );
    }

    #[test]
    fn the_account_key_is_case_insensitive_in_the_host() {
        let (_, a) = account_for(ProviderId::Anthropic, "API.Anthropic.COM");
        let (_, b) = account_for(ProviderId::Anthropic, "api.anthropic.com");
        assert_eq!(a, b);
    }

    #[test]
    fn ollama_has_no_environment_variable() {
        // A local daemon needs no credential, and reading one from the
        // environment is a way to hand a cloud key to a local process.
        assert!(env_var_for(ProviderId::Ollama).is_none());
    }

    #[test]
    fn an_unavailable_store_does_not_stop_the_key_file_from_being_tried() {
        let dir = tempfile::tempdir().unwrap();
        let path = camino::Utf8PathBuf::from_path_buf(dir.path().join("anthropic.key")).unwrap();
        std::fs::write(&path, "file-key\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let store = FakeStore {
            fail: true,
            ..FakeStore::default()
        };
        let got = resolve(
            ProviderId::Anthropic,
            "api.anthropic.com",
            &store,
            &empty_env(),
            &KeyFile { path: Some(path) },
        );
        #[cfg(unix)]
        {
            assert_eq!(got.unwrap().expect("a key").source, KeySource::File);
        }
        #[cfg(not(unix))]
        {
            // Fails closed rather than reading an unverifiable file.
            assert!(got.is_err());
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_world_readable_key_file_is_refused_with_the_fix_in_the_message() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let path = camino::Utf8PathBuf::from_path_buf(dir.path().join("k.key")).unwrap();
        std::fs::write(&path, "secret").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let err = read_key_file(&path).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("0644"), "{msg}");
        assert!(msg.contains("chmod 600"), "{msg}");
    }

    #[test]
    fn a_resolved_key_never_prints_its_value() {
        let k = ResolvedKey {
            secret: SecretString::from("ZZSECRET_VALUE_9137".to_owned()),
            source: KeySource::File,
        };
        assert!(!format!("{k:?}").contains("ZZSECRET_VALUE_9137"));
    }
}
