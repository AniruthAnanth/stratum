//! Live Keychain round-trip.
//!
//! Every item is written under a service name unique to this process, so a run
//! can never read an item some earlier build created — which matters on macOS,
//! where the file keychain's ACL is keyed on the *binary*, and reading another
//! binary's item is what makes a GUI prompt appear. Same-process write → read →
//! delete never prompts.
#![cfg(target_os = "macos")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use stratum_platform::{CredentialBackend, CredentialStore, ExposeSecret, SecretString};
use stratum_platform_macos::Keychain;

fn unique_service() -> String {
    format!(
        "dev.stratum.app/test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default()
    )
}

/// Deletes whatever the test wrote, pass or fail.
struct Cleanup<'a> {
    store: &'a Keychain,
    service: String,
    accounts: Vec<&'static str>,
}

impl Drop for Cleanup<'_> {
    fn drop(&mut self) {
        for a in &self.accounts {
            let _ = self.store.delete(&self.service, a);
        }
    }
}

#[test]
fn round_trip_set_get_list_delete() {
    let store = Keychain::new();
    let service = unique_service();
    let _guard = Cleanup {
        store: &store,
        service: service.clone(),
        accounts: vec!["openai", "anthropic"],
    };

    assert!(store.get(&service, "openai").unwrap().is_none());

    store
        .set(
            &service,
            "openai",
            &SecretString::from("sk-test-α-1".to_owned()),
        )
        .unwrap();
    assert_eq!(
        store
            .get(&service, "openai")
            .unwrap()
            .map(|s| s.expose_secret().to_owned()),
        Some("sk-test-α-1".to_owned())
    );

    // Overwriting an existing item updates in place rather than duplicating.
    store
        .set(
            &service,
            "openai",
            &SecretString::from("sk-test-2".to_owned()),
        )
        .unwrap();
    assert_eq!(
        store
            .get(&service, "openai")
            .unwrap()
            .map(|s| s.expose_secret().to_owned()),
        Some("sk-test-2".to_owned())
    );

    store
        .set(
            &service,
            "anthropic",
            &SecretString::from("sk-ant".to_owned()),
        )
        .unwrap();
    assert_eq!(
        store.list_accounts(&service).unwrap(),
        vec!["anthropic".to_owned(), "openai".to_owned()]
    );

    store.delete(&service, "openai").unwrap();
    assert!(store.get(&service, "openai").unwrap().is_none());
    assert_eq!(store.list_accounts(&service).unwrap(), vec!["anthropic"]);

    // Deleting an absent item is a state, not an event.
    store.delete(&service, "openai").unwrap();
}

#[test]
fn backend_is_surfaced_as_the_keychain() {
    assert_eq!(Keychain::new().backend(), CredentialBackend::MacosKeychain);
    assert!(CredentialBackend::MacosKeychain.is_os_store());
}

#[test]
fn an_unknown_service_lists_nothing_rather_than_failing() {
    let store = Keychain::new();
    assert!(store.list_accounts(&unique_service()).unwrap().is_empty());
}
