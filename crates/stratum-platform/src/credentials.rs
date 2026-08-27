//! Credential storage — 08 §5.3, spec §27/§28.
//!
//! # Why [`CredentialBackend`] is part of the API and not an implementation
//! detail
//!
//! We deliberately do not use the `keyring` crate (`deny.toml` bans it,
//! ARCHITECTURE C17). `keyring` hides which store answered behind an opaque
//! error, and the user has to be told whether their OpenAI key is in the
//! Keychain or in an encrypted file we wrote ourselves — the second is
//! *explicitly weaker*, and §22 makes that a privacy statement the Settings
//! pane renders. An abstraction that erases it is the wrong abstraction.

use secrecy::SecretString;
use serde::{Deserialize, Serialize};

use crate::Result;

/// The fixed service-name prefix. Service names are
/// `dev.stratum.app/<purpose>`; the account is the provider id.
pub const SERVICE_PREFIX: &str = "dev.stratum.app";

/// The AI provider key service: `dev.stratum.app/ai-provider`, with account
/// `"openai"`, `"anthropic"`, … (spec §21/§22).
pub const PURPOSE_AI_PROVIDER: &str = "ai-provider";

/// Build a service name. One function so the naming rule in 08 §5.3 cannot
/// drift between the AI settings pane and the store that reads it.
#[must_use]
pub fn service(purpose: &str) -> String {
    format!("{SERVICE_PREFIX}/{purpose}")
}

/// Where a secret actually lives. Surfaced to the user verbatim.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum CredentialBackend {
    /// macOS Keychain generic password items, written with
    /// `kSecAttrAccessibleWhenUnlockedThisDeviceOnly` and
    /// `kSecAttrSynchronizable = false`: never leaves the machine, never syncs
    /// to iCloud.
    MacosKeychain,
    /// Win32 Credential Manager, `CRED_TYPE_GENERIC`.
    WindowsCredentialManager,
    /// `org.freedesktop.secrets` over D-Bus.
    SecretService,
    /// KDE's wallet.
    KWallet,
    /// AES-256-GCM file under `state_dir`, key derived from a machine id.
    /// Explicitly and visibly weaker than an OS store; the UI **must** say so.
    EncryptedFile,
}

impl CredentialBackend {
    /// True for the three backends that are an OS-managed secret store. The
    /// Settings pane renders a warning for everything else.
    #[must_use]
    pub const fn is_os_store(self) -> bool {
        matches!(
            self,
            Self::MacosKeychain | Self::WindowsCredentialManager | Self::SecretService
        )
    }
}

/// Read/write access to the platform's secret store.
///
/// Every method can legitimately fail with
/// [`crate::PlatformError::BackendUnavailable`] (no Secret Service on the bus)
/// or [`crate::PlatformError::PermissionDenied`] (a locked keychain, a user who
/// clicked Deny). Neither is exceptional.
pub trait CredentialStore: Send + Sync {
    /// Fetch a secret. `Ok(None)` means "no such item", which is different from
    /// an error and is the normal state before the user has entered a key.
    ///
    /// # Errors
    /// See the trait docs.
    fn get(&self, service: &str, account: &str) -> Result<Option<SecretString>>;

    /// Store a secret, replacing any existing item for the same
    /// `(service, account)` pair.
    ///
    /// # Errors
    /// See the trait docs.
    fn set(&self, service: &str, account: &str, secret: &SecretString) -> Result<()>;

    /// Remove a secret. Deleting an item that is not there is `Ok(())`: the
    /// caller asked for a state, not for an event.
    ///
    /// # Errors
    /// See the trait docs.
    fn delete(&self, service: &str, account: &str) -> Result<()>;

    /// Every account with a stored secret under `service`, sorted. Drives the
    /// "which providers are configured" list without ever reading a value.
    ///
    /// # Errors
    /// See the trait docs.
    fn list_accounts(&self, service: &str) -> Result<Vec<String>>;

    /// Which store answered. Never `Result`: this is a property of the build
    /// and the running session, and a UI that cannot render it has nothing
    /// useful to say instead.
    fn backend(&self) -> CredentialBackend;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_names_follow_the_fixed_rule() {
        assert_eq!(service(PURPOSE_AI_PROVIDER), "dev.stratum.app/ai-provider");
    }

    #[test]
    fn the_fallback_is_not_an_os_store() {
        assert!(CredentialBackend::MacosKeychain.is_os_store());
        assert!(!CredentialBackend::EncryptedFile.is_os_store());
        assert!(!CredentialBackend::KWallet.is_os_store());
    }

    #[test]
    fn backend_serialises_snake_case_for_the_settings_pane() {
        let j = serde_json::to_string(&CredentialBackend::MacosKeychain).unwrap();
        assert_eq!(j, "\"macos_keychain\"");
    }
}
