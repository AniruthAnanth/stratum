//! A hand-rolled `org.freedesktop.secrets` client — 08 §5.3.
//!
//! Hand-rolled, as the design says, and not through the `secret-service` crate:
//! that crate depends directly on `zbus`, and `deny.toml`'s
//! `{ name = "zbus", wrappers = ["stratum-platform-linux"] }` is a check on the
//! *direct* dependent, so pulling it in turns `cargo deny check bans` red on a
//! rule this unit does not own. The crate's manifest records the measurement.
//!
//! # The session algorithm is `plain`, deliberately
//!
//! The Secret Service specification offers `plain` and
//! `dh-ietf1024-sha256-aes128-cbc-pkcs7`. We negotiate `plain`, which means the
//! secret crosses the **session bus socket** in the clear — a `AF_UNIX` socket
//! under `/run/user/$UID`, mode 0700, to a daemon running as the same user.
//! Anything that can read that socket can already read this process's memory.
//!
//! The alternative would buy nothing against that threat and costs a 1024-bit
//! modular exponentiation, i.e. a bignum dependency. The workspace table
//! carries none, `deny.toml` sanctions none for this crate, and hand-rolling
//! modexp on the path that carries a user's API key is a far worse trade than
//! the exposure it would remove. If a future desktop refuses `plain`, the
//! failure is a clean [`PlatformError::BackendUnavailable`] and
//! [`crate::credentials`] demotes to the encrypted file — which is the
//! behaviour that unit's acceptance already demands.
//!
//! # Locked collections
//!
//! A login keyring whose password differs from the login password is locked at
//! session start, and every read of it returns nothing until it is unlocked.
//! [`SecretServiceClient::unlock`] performs the unlock, including the
//! `Prompt`/`Completed` round trip that puts the daemon's password dialog on
//! screen. A user who dismisses that dialog gets
//! [`PlatformError::PermissionDenied`] — **not** a demotion to the file store,
//! because they made a decision and writing their key somewhere else would
//! route around it.

use std::collections::HashMap;
use std::sync::Mutex;

use stratum_platform::{ExposeSecret, PlatformError, Result, SecretString};
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

use crate::bus;
use crate::credentials::SecretStore;

const SERVICE_BUS: &str = "org.freedesktop.secrets";
const SERVICE_PATH: &str = "/org/freedesktop/secrets";
const SERVICE_IFACE: &str = "org.freedesktop.Secret.Service";
const COLLECTION_IFACE: &str = "org.freedesktop.Secret.Collection";
const ITEM_IFACE: &str = "org.freedesktop.Secret.Item";
const PROMPT_IFACE: &str = "org.freedesktop.Secret.Prompt";

/// The attribute keys we store items under. `xdg:schema` is what makes
/// `secret-tool` and Seahorse render the item as a generic secret rather than
/// as an unlabelled blob.
const ATTR_SERVICE: &str = "service";
const ATTR_ACCOUNT: &str = "account";
const ATTR_SCHEMA: &str = "xdg:schema";
const SCHEMA_VALUE: &str = "org.freedesktop.Secret.Generic";

/// `text/plain; charset=utf8` — the content type we write, and the only one we
/// can hand back as a [`SecretString`].
const CONTENT_TYPE: &str = "text/plain; charset=utf8";

/// The D-Bus path that means "no prompt was needed".
const NO_PROMPT: &str = "/";

/// One secret as the Secret Service carries it:
/// `(session, parameters, value, content_type)`.
type Secret = (OwnedObjectPath, Vec<u8>, Vec<u8>, String);

/// [`SecretStore`] over `org.freedesktop.secrets`.
#[derive(Debug, Default)]
pub struct SecretServiceClient {
    /// The negotiated session path. One per connection, opened on first use and
    /// reused: `OpenSession` is a round trip, and doing it per credential read
    /// would double the cost of every AI provider call.
    session: Mutex<Option<OwnedObjectPath>>,
}

impl SecretServiceClient {
    /// Construct. Touches no bus until the first call.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            session: Mutex::new(None),
        }
    }

    fn proxy(path: &str, iface: &'static str) -> Result<zbus::blocking::Proxy<'static>> {
        let conn = bus::session_blocking()?;
        zbus::blocking::Proxy::new(&conn, SERVICE_BUS, path.to_owned(), iface)
            .map_err(|e| bus::classify(&e))
    }

    fn service() -> Result<zbus::blocking::Proxy<'static>> {
        Self::proxy(SERVICE_PATH, SERVICE_IFACE)
    }

    /// The negotiated session, opened once.
    fn session(&self) -> Result<OwnedObjectPath> {
        if let Some(s) = self.session.lock().map_err(poisoned)?.as_ref() {
            return Ok(s.clone());
        }
        let service = Self::service()?;
        let (_output, session): (OwnedValue, OwnedObjectPath) = service
            .call("OpenSession", &("plain", Value::from("")))
            .map_err(|e| bus::classify(&e))?;
        *self.session.lock().map_err(poisoned)? = Some(session.clone());
        Ok(session)
    }

    fn attributes(service: &str, account: &str) -> HashMap<String, String> {
        HashMap::from([
            (ATTR_SERVICE.to_owned(), service.to_owned()),
            (ATTR_ACCOUNT.to_owned(), account.to_owned()),
            (ATTR_SCHEMA.to_owned(), SCHEMA_VALUE.to_owned()),
        ])
    }

    /// Every item matching `attrs`, with any locked ones unlocked first.
    fn search(&self, attrs: &HashMap<String, String>) -> Result<Vec<OwnedObjectPath>> {
        let service = Self::service()?;
        let (mut unlocked, locked): (Vec<OwnedObjectPath>, Vec<OwnedObjectPath>) = service
            .call("SearchItems", attrs)
            .map_err(|e| bus::classify(&e))?;
        if !locked.is_empty() {
            unlocked.extend(self.unlock(&locked)?);
        }
        Ok(unlocked)
    }

    /// Unlock `objects`, driving the daemon's password prompt if it asks for
    /// one. Returns the objects that are now unlocked.
    fn unlock(&self, objects: &[OwnedObjectPath]) -> Result<Vec<OwnedObjectPath>> {
        let service = Self::service()?;
        let (unlocked, prompt): (Vec<OwnedObjectPath>, OwnedObjectPath) = service
            .call("Unlock", &objects)
            .map_err(|e| bus::classify(&e))?;
        if prompt.as_str() == NO_PROMPT {
            return Ok(unlocked);
        }
        let result = self.run_prompt(&prompt)?;
        // The prompt's result is the list of objects it actually unlocked.
        Vec::<OwnedObjectPath>::try_from(result).map_err(|e| {
            PlatformError::BackendUnavailable(format!("malformed unlock prompt result: {e}"))
        })
    }

    /// Show a `org.freedesktop.Secret.Prompt` and wait for `Completed`.
    ///
    /// No timeout: the user is typing a password. The subscription is created
    /// before `Prompt` is called for the same reason as in [`crate::portal`] —
    /// the signal can otherwise arrive before the match rule does, and the
    /// application then waits forever for something already delivered.
    fn run_prompt(&self, prompt: &OwnedObjectPath) -> Result<OwnedValue> {
        let conn = bus::session_blocking()?;
        let rule = zbus::MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .sender(SERVICE_BUS)
            .map_err(|e| bus::classify(&e))?
            .interface(PROMPT_IFACE)
            .map_err(|e| bus::classify(&e))?
            .member("Completed")
            .map_err(|e| bus::classify(&e))?
            .path(prompt.as_str().to_owned())
            .map_err(|e| bus::classify(&e))?
            .build();
        let mut completed = zbus::blocking::MessageIterator::for_match_rule(rule, &conn, Some(1))
            .map_err(|e| bus::classify(&e))?;

        let proxy = Self::proxy(prompt.as_str(), PROMPT_IFACE)?;
        // The window id is used for transient-for; we have none to offer, and
        // every implementation accepts an empty one.
        proxy
            .call::<_, _, ()>("Prompt", &"")
            .map_err(|e| bus::classify(&e))?;

        let msg = completed.next().ok_or_else(|| {
            PlatformError::BackendUnavailable(
                "the secret service closed the connection during the unlock prompt".to_owned(),
            )
        })?;
        let msg = msg.map_err(|e| bus::classify(&e))?;
        let (dismissed, result): (bool, OwnedValue) =
            msg.body().deserialize().map_err(|e| bus::classify(&e))?;
        if dismissed {
            // A decision, not an absence. See the module docs.
            return Err(PlatformError::PermissionDenied(
                "the keyring unlock prompt was dismissed".to_owned(),
            ));
        }
        Ok(result)
    }

    /// The default collection's object path, via the `default` alias.
    fn default_collection(&self) -> Result<OwnedObjectPath> {
        let service = Self::service()?;
        let path: OwnedObjectPath = service
            .call("ReadAlias", &"default")
            .map_err(|e| bus::classify(&e))?;
        if path.as_str() == NO_PROMPT {
            return Err(PlatformError::BackendUnavailable(
                "the secret service has no default collection; the login keyring \
                 has not been created"
                    .to_owned(),
            ));
        }
        // A locked collection cannot be written to; unlock before CreateItem
        // rather than after the write fails with a bare error.
        self.unlock(std::slice::from_ref(&path))?;
        Ok(path)
    }
}

impl SecretStore for SecretServiceClient {
    fn get(&self, service: &str, account: &str) -> Result<Option<SecretString>> {
        let items = self.search(&Self::attributes(service, account))?;
        let Some(item) = items.first() else {
            return Ok(None);
        };
        let session = self.session()?;
        let proxy = Self::proxy(item.as_str(), ITEM_IFACE)?;
        let secret: Secret = proxy
            .call("GetSecret", &session)
            .map_err(|e| bus::classify(&e))?;
        let value = String::from_utf8(secret.2).map_err(|_| {
            PlatformError::BackendUnavailable(
                "the stored secret is not valid UTF-8; it was not written by Stratum".to_owned(),
            )
        })?;
        Ok(Some(SecretString::from(value)))
    }

    fn set(&self, service: &str, account: &str, secret: &SecretString) -> Result<()> {
        let collection = self.default_collection()?;
        let session = self.session()?;
        let attrs = Self::attributes(service, account);

        let properties: HashMap<&str, Value<'_>> = HashMap::from([
            (
                "org.freedesktop.Secret.Item.Label",
                // What the user sees in Seahorse. Naming the account matters:
                // "Stratum" alone gives four identical rows once they have
                // configured four providers.
                Value::from(format!("Stratum: {account}")),
            ),
            (
                "org.freedesktop.Secret.Item.Attributes",
                Value::from(attrs.clone()),
            ),
        ]);
        let value: Secret = (
            session,
            Vec::new(),
            secret.expose_secret().as_bytes().to_vec(),
            CONTENT_TYPE.to_owned(),
        );

        let proxy = Self::proxy(collection.as_str(), COLLECTION_IFACE)?;
        // `replace = true`: the caller asked for a state. Without it a second
        // save of the same provider leaves two items and the reader picks one
        // of them arbitrarily.
        let (item, prompt): (OwnedObjectPath, OwnedObjectPath) = proxy
            .call("CreateItem", &(properties, value, true))
            .map_err(|e| bus::classify(&e))?;
        if prompt.as_str() != NO_PROMPT {
            self.run_prompt(&prompt)?;
        }
        if item.as_str() == NO_PROMPT {
            return Err(PlatformError::BackendUnavailable(
                "the secret service accepted the item but returned no path".to_owned(),
            ));
        }
        Ok(())
    }

    fn delete(&self, service: &str, account: &str) -> Result<()> {
        for item in self.search(&Self::attributes(service, account))? {
            let proxy = Self::proxy(item.as_str(), ITEM_IFACE)?;
            let prompt: OwnedObjectPath =
                proxy.call("Delete", &()).map_err(|e| bus::classify(&e))?;
            if prompt.as_str() != NO_PROMPT {
                self.run_prompt(&prompt)?;
            }
        }
        // Deleting an item that is not there is Ok: the caller asked for a
        // state, not for an event.
        Ok(())
    }

    fn list_accounts(&self, service: &str) -> Result<Vec<String>> {
        let attrs = HashMap::from([
            (ATTR_SERVICE.to_owned(), service.to_owned()),
            (ATTR_SCHEMA.to_owned(), SCHEMA_VALUE.to_owned()),
        ]);
        let mut out = Vec::new();
        for item in self.search(&attrs)? {
            let proxy = Self::proxy(item.as_str(), ITEM_IFACE)?;
            // The attributes, not the secret: "which providers are configured"
            // must never decrypt a value.
            let attrs: HashMap<String, String> = proxy
                .get_property("Attributes")
                .map_err(|e| bus::classify(&e))?;
            if let Some(account) = attrs.get(ATTR_ACCOUNT) {
                out.push(account.clone());
            }
        }
        out.sort_unstable();
        out.dedup();
        Ok(out)
    }
}

fn poisoned<T>(_: std::sync::PoisonError<T>) -> PlatformError {
    PlatformError::BackendUnavailable("the secret service session lock was poisoned".to_owned())
}
