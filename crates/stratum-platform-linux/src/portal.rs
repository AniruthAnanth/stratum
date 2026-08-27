//! `org.freedesktop.portal.FileChooser` over D-Bus — 08 §5.4.
//!
//! The portal is the right primary on every modern desktop: it is the only file
//! chooser that works identically under Wayland, under X11, inside a Flatpak
//! and inside a Snap, and it is the one the user's own desktop draws, so the
//! panel looks like every other panel on their machine rather than like a
//! toolkit we happened to link.
//!
//! # The request/response dance, and the race it has
//!
//! A portal method does not return the answer. It returns an **object path**,
//! and the answer arrives later as a `Response` signal on that path. The
//! obvious implementation — call, then subscribe to the returned path — has a
//! race: on a fast desktop the user can answer a pre-populated dialog before
//! our `AddMatch` lands, and the reply is then delivered to nobody and the
//! Open panel hangs forever. The portal specification's remedy is to compute
//! the path *before* calling, from our own unique bus name and a token we
//! choose, and subscribe first. That is what [`XdgPortal::request_path`] and
//! the ordering in [`XdgPortal::choose`] are for.
//!
//! # Where [`crate::dialogs::PORTAL_DEADLINE`] applies
//!
//! To the **method call**, never to the signal. Once the portal has answered
//! with a request handle it exists and is working, and the human in front of
//! the dialog gets as long as they need.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use camino::{Utf8Path, Utf8PathBuf};
use futures_lite::StreamExt;
use stratum_platform::{
    FileFilter, FolderRequest, OpenRequest, PlatformError, Result, SaveRequest,
};
use zbus::zvariant::{Array, OwnedObjectPath, OwnedValue, Structure, Value};

use crate::bus;
use crate::dialogs::{DesktopPortal, PORTAL_DEADLINE};
use crate::uri;

const PORTAL_BUS: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const FILE_CHOOSER: &str = "org.freedesktop.portal.FileChooser";
const REQUEST_IFACE: &str = "org.freedesktop.portal.Request";

/// `org.freedesktop.FileManager1` — the interface that *selects* an item in the
/// user's file manager, which is what Reveal means. Not a portal interface:
/// it is implemented by Nautilus, Dolphin, Nemo, Thunar and PCManFM directly.
const FILE_MANAGER_BUS: &str = "org.freedesktop.FileManager1";
const FILE_MANAGER_PATH: &str = "/org/freedesktop/FileManager1";

/// The portal's own response codes.
const RESPONSE_OK: u32 = 0;
const RESPONSE_CANCELLED: u32 = 1;

/// [`DesktopPortal`] over the real bus.
#[derive(Debug, Default)]
pub struct XdgPortal {
    /// Monotonic suffix for `handle_token`, so two panels opened in the same
    /// millisecond cannot collide on a request path.
    seq: AtomicU64,
}

impl XdgPortal {
    /// Construct. Opens nothing until the first call, so building a
    /// [`crate::LinuxPlatform`] never blocks on D-Bus.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            seq: AtomicU64::new(0),
        }
    }

    /// A token unique within this connection.
    fn token(&self) -> String {
        format!("stratum{}", self.seq.fetch_add(1, Ordering::Relaxed))
    }

    /// The object path the portal will emit `Response` on, computed the way the
    /// specification says so we can subscribe before calling.
    ///
    /// `:1.42` becomes `1_42`: the leading colon is dropped and every `.` is an
    /// `_`, because an object path element may contain neither.
    fn request_path(unique_name: &str, token: &str) -> String {
        let sender = unique_name.trim_start_matches(':').replace('.', "_");
        format!("/org/freedesktop/portal/desktop/request/{sender}/{token}")
    }

    /// Subscribe, call, then wait. See the module docs for why in that order.
    async fn choose(
        &self,
        method: &'static str,
        title: &str,
        parent: &str,
        options: HashMap<&str, Value<'_>>,
    ) -> Result<HashMap<String, OwnedValue>> {
        let conn = bus::session().await?;
        let unique = conn
            .unique_name()
            .map(|n| n.as_str().to_owned())
            .ok_or_else(|| {
                PlatformError::BackendUnavailable(
                    "this D-Bus connection has no unique name; it is not a bus connection"
                        .to_owned(),
                )
            })?;

        let mut options = options;
        let token = self.token();
        let expected = Self::request_path(&unique, &token);
        options.insert("handle_token", Value::from(token.clone()));

        // `sender` is the WELL-KNOWN name, and that is correct in both halves
        // of the filtering. On the wire a signal's sender header is always a
        // unique name (`:1.7`), so a naive local comparison against
        // `org.freedesktop.portal.Desktop` would reject every message — but the
        // bus daemon resolves well-known names in match rules itself, and
        // `zbus::MatchRule::matches` documents and implements the other side of
        // that: a well-known `sender` in the rule always passes the local
        // check. The path, interface and member below are what actually narrow
        // it, and the path is unique to this request.
        let rule = zbus::MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .sender(PORTAL_BUS)
            .map_err(|e| bus::classify(&e))?
            .interface(REQUEST_IFACE)
            .map_err(|e| bus::classify(&e))?
            .member("Response")
            .map_err(|e| bus::classify(&e))?
            .path(expected.as_str())
            .map_err(|e| bus::classify(&e))?
            .build();
        // Queue depth 1: there is exactly one Response per request, and a
        // deeper queue would only hold messages for a stream we are about to
        // drop.
        let mut responses = zbus::MessageStream::for_match_rule(rule, &conn, Some(1))
            .await
            .map_err(|e| bus::classify(&e))?;

        let proxy = zbus::Proxy::new(&conn, PORTAL_BUS, PORTAL_PATH, FILE_CHOOSER)
            .await
            .map_err(|e| bus::classify(&e))?;

        // THE DEADLINE, and only here. A portal that is not on the bus and
        // cannot be activated normally answers with `ServiceUnknown` in
        // microseconds; the timer is for the pathological case where
        // `xdg-desktop-portal` is installed, is being activated, and wedges —
        // which otherwise leaves the user looking at an application that has
        // stopped responding to File → Open with no explanation.
        //
        // `args` is bound rather than inlined because the call future borrows
        // it, and a temporary would be dropped at the end of the statement that
        // created it — before `timeout` ever polls the future.
        let args = (parent, title, &options);
        let call = proxy.call::<_, _, OwnedObjectPath>(method, &args);
        let handle = match timeout(PORTAL_DEADLINE, call).await {
            Some(r) => r.map_err(|e| bus::classify(&e))?,
            None => {
                return Err(PlatformError::BackendUnavailable(format!(
                    "{PORTAL_BUS} did not answer {method} within {PORTAL_DEADLINE:?}"
                )))
            }
        };
        if handle.as_str() != expected {
            // Portals older than the `handle_token` option (xdg-desktop-portal
            // < 0.9) chose the path themselves. We cannot have subscribed to
            // it, so the answer would never arrive; saying so beats hanging.
            return Err(PlatformError::BackendUnavailable(format!(
                "this xdg-desktop-portal is too old: it answered on {handle} rather than {expected}"
            )));
        }

        // No deadline from here on: the human is choosing a file.
        let msg = responses.next().await.ok_or_else(|| {
            PlatformError::BackendUnavailable(
                "the portal closed the connection before answering".to_owned(),
            )
        })?;
        let msg = msg.map_err(|e| bus::classify(&e))?;
        let (code, results): (u32, HashMap<String, OwnedValue>) =
            msg.body().deserialize().map_err(|e| bus::classify(&e))?;

        match code {
            RESPONSE_OK => Ok(results),
            RESPONSE_CANCELLED => Err(PlatformError::Cancelled),
            // "Ended in some other way" per the spec. It is not a cancel — the
            // user did not decide anything — and it is not our failure either.
            other => Err(PlatformError::BackendUnavailable(format!(
                "the portal ended the request with response code {other}"
            ))),
        }
    }
}

/// Pull `uris` out of a portal result.
fn uris(results: &HashMap<String, OwnedValue>) -> Result<Vec<Utf8PathBuf>> {
    let raw: Vec<String> = results
        .get("uris")
        .ok_or(PlatformError::Unsupported(
            "the portal returned no uris; this desktop's file chooser is not usable",
        ))
        .and_then(|v| {
            Vec::<String>::try_from(v.clone()).map_err(|e| {
                PlatformError::BackendUnavailable(format!(
                    "the portal's uris field was not an array of strings: {e}"
                ))
            })
        })?;

    let mut out = Vec::with_capacity(raw.len());
    for u in &raw {
        // A `sftp://` or `smb://` result is a GVFS location that `std::fs`
        // cannot open. Reporting it beats handing the engine a path that does
        // not exist.
        out.push(uri::file_uri_to_path(u).ok_or(PlatformError::Unsupported(
            "the chosen file is not on this machine's filesystem",
        ))?);
    }
    if out.is_empty() {
        // The portal answered OK with nothing selected. Per 08 §5.4 that is
        // still a cancel: an empty selection and a dismissal must not be told
        // apart by the caller, because only one of them is possible.
        return Err(PlatformError::Cancelled);
    }
    Ok(out)
}

/// `a(sa(us))`, the portal's filter encoding: a list of
/// `(human name, [(kind, pattern)])` where kind 0 is a glob and 1 is a MIME
/// type.
///
/// `None` for an empty filter list, and the key is then omitted entirely rather
/// than sent empty — an empty `filters` array makes some portal backends show a
/// dropdown containing nothing, which is worse than no dropdown.
fn portal_filters(filters: &[FileFilter]) -> Option<Value<'static>> {
    let mut array: Option<Array<'static>> = None;
    for f in filters {
        let patterns: Vec<(u32, String)> = f
            .extensions
            .iter()
            .map(|e| (0u32, format!("*.{e}")))
            .collect();
        if patterns.is_empty() {
            continue;
        }
        let entry: Structure<'static> = (f.name.clone(), Array::from(patterns)).into();
        if array.is_none() {
            array = Some(Array::new(entry.signature()));
        }
        // `append` type-checks the element against the array's declared
        // signature; every entry here is built the same way, so it cannot fail.
        if let Some(a) = array.as_mut() {
            a.append(Value::from(entry)).ok()?;
        }
    }
    array.map(Value::from)
}

/// The portal wants `current_folder` as a NUL-terminated byte array, not a
/// string: it is a filesystem path, which on Linux is bytes.
fn path_bytes(path: &Utf8Path) -> Value<'static> {
    let mut bytes = path.as_str().as_bytes().to_vec();
    bytes.push(0);
    Value::from(bytes)
}

/// `parent_window`. The empty string means "no parent", which is correct and
/// honest until the shell can hand us an `x11:<xid>` or `wayland:<handle>`
/// token — those come from the toolkit, and this crate does not link one.
const NO_PARENT: &str = "";

#[async_trait::async_trait]
impl DesktopPortal for XdgPortal {
    async fn open_files(&self, req: &OpenRequest) -> Result<Vec<Utf8PathBuf>> {
        let mut options: HashMap<&str, Value<'_>> = HashMap::new();
        options.insert("modal", Value::from(true));
        options.insert("multiple", Value::from(req.multiple));
        if let Some(f) = portal_filters(&req.filters) {
            options.insert("filters", f);
        }
        if let Some(dir) = &req.start_dir {
            options.insert("current_folder", path_bytes(dir));
        }
        let results = self
            .choose("OpenFile", &req.title, NO_PARENT, options)
            .await?;
        uris(&results)
    }

    async fn save_file(&self, req: &SaveRequest) -> Result<Utf8PathBuf> {
        let mut options: HashMap<&str, Value<'_>> = HashMap::new();
        options.insert("modal", Value::from(true));
        if let Some(f) = portal_filters(&req.filters) {
            options.insert("filters", f);
        }
        if let Some(name) = &req.suggested_name {
            options.insert("current_name", Value::from(name.clone()));
        }
        if let Some(dir) = &req.start_dir {
            options.insert("current_folder", path_bytes(dir));
        }
        let results = self
            .choose("SaveFile", &req.title, NO_PARENT, options)
            .await?;
        uris(&results)?
            .into_iter()
            .next()
            .ok_or(PlatformError::Cancelled)
    }

    async fn pick_folder(&self, req: &FolderRequest) -> Result<Utf8PathBuf> {
        let mut options: HashMap<&str, Value<'_>> = HashMap::new();
        options.insert("modal", Value::from(true));
        // `OpenFile` with `directory`, which is how the portal spells a folder
        // picker; there is no separate method.
        options.insert("directory", Value::from(true));
        if let Some(dir) = &req.start_dir {
            options.insert("current_folder", path_bytes(dir));
        }
        let results = self
            .choose("OpenFile", &req.title, NO_PARENT, options)
            .await?;
        uris(&results)?
            .into_iter()
            .next()
            .ok_or(PlatformError::Cancelled)
    }

    fn show_item_in_folder(&self, path: &Utf8Path) -> Result<()> {
        let conn = bus::session_blocking()?;
        let proxy = zbus::blocking::Proxy::new(
            &conn,
            FILE_MANAGER_BUS,
            FILE_MANAGER_PATH,
            FILE_MANAGER_BUS,
        )
        .map_err(|e| bus::classify(&e))?;
        let uris = vec![uri::path_to_file_uri(path)];
        // The startup id is used for focus stealing prevention; we have none
        // to offer, and every implementation accepts an empty one.
        proxy
            .call::<_, _, ()>("ShowItems", &(uris, ""))
            .map_err(|e| bus::classify(&e))
    }
}

/// Resolve `f`, or give up after `d`.
///
/// `async_io::Timer` rather than a `tokio::time::timeout`: the D-Bus connection
/// runs on the `async-io` reactor (see this crate's `Cargo.toml`), and mixing a
/// tokio timer into a future driven by that reactor requires a tokio runtime to
/// be entered, which is not something a library can assume.
async fn timeout<F: std::future::Future>(d: std::time::Duration, f: F) -> Option<F::Output> {
    futures_lite::future::or(async { Some(f.await) }, async {
        async_io::Timer::after(d).await;
        None
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole point of computing the path ourselves: it must match what the
    /// portal will choose, or the answer arrives on a path nobody is listening
    /// to and File → Open hangs forever.
    #[test]
    fn the_request_path_matches_the_portal_specifications_rule() {
        assert_eq!(
            XdgPortal::request_path(":1.42", "stratum0"),
            "/org/freedesktop/portal/desktop/request/1_42/stratum0"
        );
        // Multi-component unique names exist on a busy bus.
        assert_eq!(
            XdgPortal::request_path(":1.2183", "stratum17"),
            "/org/freedesktop/portal/desktop/request/1_2183/stratum17"
        );
    }

    #[test]
    fn tokens_do_not_repeat_within_one_process() {
        let p = XdgPortal::new();
        let a = p.token();
        let b = p.token();
        assert_ne!(a, b);
    }

    #[test]
    fn an_empty_filter_list_sends_no_filters_key() {
        assert!(portal_filters(&[]).is_none());
        // A filter with no extensions is not a filter; sending it produces an
        // entry in the dropdown that matches nothing.
        assert!(portal_filters(&[FileFilter::new("Everything", &[])]).is_none());
    }

    #[test]
    fn filters_are_encoded_as_globs_with_the_portals_kind_tag() {
        let v = portal_filters(&[FileFilter::new("Stata do-files", &["do", "ado"])]);
        // `a(sa(us))` — the exact signature the FileChooser interface declares.
        assert_eq!(
            v.map(|v| v.value_signature().to_string()),
            Some("a(sa(us))".to_owned())
        );
    }

    #[test]
    fn a_current_folder_is_nul_terminated_bytes_not_a_string() {
        let v = path_bytes(Utf8Path::new("/home/jo"));
        assert_eq!(v.value_signature().to_string(), "ay");
        let bytes = Vec::<u8>::try_from(v);
        assert_eq!(bytes.as_deref(), Ok(b"/home/jo\0".as_slice()));
    }
}
