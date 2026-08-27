//! Double-click → running session: the OS file-open path (spec §0's LS
//! registration, plan U7a).
//!
//! macOS delivers file-association opens as `RunEvent::Opened` Apple Events;
//! Windows and Linux pass the path in argv (Tauri v2 exposes no runtime event
//! for them without the single-instance plugin, which this host does not
//! carry). Both funnel here, and the two extensions the Info.plist registers
//! take two different routes:
//!
//! - a `.do` is routed to the frontend as [`OPEN_PATH_EVENT`] — the same
//!   host→webview emit channel the menu uses, carrying the path a menu id
//!   cannot — and the frontend opens it through `doc_open` like any other
//!   editor open;
//! - a `.dta` becomes an `exec_submit` of `use "<path>", clear` as
//!   `RunIntent::CommandBar`: the exact route a typed command takes, so the
//!   engine loads it, the ledger records it, and every pane updates from the
//!   same event stream. The host never touches the file itself — reading
//!   datasets is the engine's job (ARCHITECTURE §8.2 keeps this crate from
//!   even linking it).
//!
//! **Cold start.** An open request can arrive before the webview has booted
//! and opened a session: on macOS the Apple Event lands mid-launch, and argv
//! is read before the builder runs. Requests wait in a [`PendingOpens`] queue
//! and are drained on `session_open`'s success path — the first moment a
//! session exists to submit into and a window exists to route to.

use camino::{Utf8Path, Utf8PathBuf};
use serde::Serialize;
use stratum_proto::engine::{EngineRequest, EngineResponse, InlineResultsMode};
use stratum_proto::exec::RunIntent;
use stratum_proto::ids::SessionId;
use tauri::{Emitter, Manager, Runtime};

use crate::ipc::HostState;

/// The event the frontend listens on for host-initiated file opens. Payload:
/// [`OpenPathPayload`]. Disjoint from `menu.rs`'s `stratum://menu-action`,
/// which carries an action id and no path.
pub const OPEN_PATH_EVENT: &str = "stratum://open-path";

/// What one OS open request means. Classification is by extension alone —
/// the OS already resolved the double-click through the UTIs the Info.plist
/// imports, and probing the file here would race the engine's own open.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum FileAction {
    /// Route to the editor; the frontend opens it through `doc_open`.
    OpenDo(Utf8PathBuf),
    /// Submit `use "<path>", clear` through the session's command route.
    UseDta(Utf8PathBuf),
}

/// [`OPEN_PATH_EVENT`]'s payload, camelCase like every §11 reply.
#[derive(Clone, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenPathPayload {
    /// `"do"` — the one kind the frontend is asked to handle; a `.dta`
    /// never becomes this event.
    pub kind: &'static str,
    pub path: String,
}

/// Classify one OS-supplied path. Extensions compare case-insensitively
/// because every filesystem the OS opens from preserves case without
/// requiring it; anything else is refused with the path in the message.
pub fn classify(raw: &str) -> Result<FileAction, String> {
    let path = Utf8PathBuf::from(raw);
    let ext = path.extension().map(str::to_ascii_lowercase);
    match ext.as_deref() {
        Some("do") => Ok(FileAction::OpenDo(path)),
        Some("dta") => Ok(FileAction::UseDta(path)),
        _ => Err(format!("{raw} is not a .do or .dta file")),
    }
}

/// The command a `.dta` open submits — exactly what a user would type, so the
/// engine's `use` semantics (banner, r(4)-free replace via `clear`, errors)
/// apply unchanged. Stata's quoted-filename syntax has no escape for a `"`,
/// and a control character would splice the command line itself, so such
/// paths are refused rather than submitted garbled.
pub fn use_command(path: &Utf8Path) -> Result<String, String> {
    if path.as_str().contains('"') || path.as_str().chars().any(char::is_control) {
        return Err(format!(
            "the path {path:?} cannot be spelled inside Stata's quoted filename syntax"
        ));
    }
    Ok(format!("use \"{path}\", clear"))
}

/// Open requests that arrived before a session existed. One static instance;
/// the type is separate so tests can exercise a queue of their own.
pub struct PendingOpens(std::sync::Mutex<Vec<FileAction>>);

impl PendingOpens {
    #[must_use]
    pub const fn new() -> Self {
        Self(std::sync::Mutex::new(Vec::new()))
    }

    pub fn push(&self, action: FileAction) {
        self.0.lock().expect("pending opens").push(action);
    }

    /// Everything queued, in arrival order; the queue is left empty.
    pub fn take(&self) -> Vec<FileAction> {
        std::mem::take(&mut *self.0.lock().expect("pending opens"))
    }
}

impl Default for PendingOpens {
    fn default() -> Self {
        Self::new()
    }
}

static PENDING: PendingOpens = PendingOpens::new();

/// Queue an open request that classified at argv time (Windows/Linux file
/// associations, or a plain `stratum-desktop foo.do`). No session can exist
/// yet — `session_open` drains the queue.
pub fn enqueue(action: FileAction) {
    PENDING.push(action);
}

/// One OS open request at runtime (macOS `RunEvent::Opened`). Dispatched at
/// once when a session is up; queued for `session_open`'s drain otherwise —
/// the Apple Event for a cold-start double-click arrives before the webview
/// has booted, and dropping it would make exactly that launch path the one
/// that loses the file.
#[cfg(target_os = "macos")]
pub fn open_request<R: Runtime>(app: &tauri::AppHandle<R>, raw: &str) {
    let action = match classify(raw) {
        Ok(action) => action,
        Err(why) => {
            eprintln!("stratum-desktop: refusing OS open request: {why}");
            return;
        }
    };
    let session = app
        .try_state::<HostState>()
        .and_then(|state| state.registry.only_session());
    match session {
        Some(session) => {
            let target = focused_label(app);
            dispatch(app, session, &target, action);
        }
        None => PENDING.push(action),
    }
}

/// Where a runtime open routes: the focused window, `main` when none reports
/// focus — the same rule `menu.rs` applies to menu clicks.
#[cfg(target_os = "macos")]
fn focused_label<R: Runtime>(app: &tauri::AppHandle<R>) -> String {
    app.webview_windows()
        .into_iter()
        .find(|(_, w)| w.is_focused().unwrap_or(false))
        .map(|(label, _)| label)
        .unwrap_or_else(|| "main".to_owned())
}

/// Drain the cold-start queue into a session that just opened. `target` is
/// the window that opened it — the one whose webview is provably booted.
pub fn drain<R: Runtime>(app: &tauri::AppHandle<R>, session: SessionId, target: &str) {
    for action in PENDING.take() {
        dispatch(app, session, target, action);
    }
}

fn dispatch<R: Runtime>(
    app: &tauri::AppHandle<R>,
    session: SessionId,
    target: &str,
    action: FileAction,
) {
    match action {
        FileAction::OpenDo(path) => {
            let path = path.into_string();
            let payload = OpenPathPayload {
                kind: "do",
                path: path.clone(),
            };
            if let Err(e) = app.emit_to(target, OPEN_PATH_EVENT, payload) {
                eprintln!("stratum-desktop: could not route {path} to the editor: {e}");
            }
        }
        FileAction::UseDta(path) => {
            let text = match use_command(&path) {
                Ok(text) => text,
                Err(why) => {
                    eprintln!("stratum-desktop: refusing to open {path}: {why}");
                    return;
                }
            };
            // The submit is a bounded engine round trip; the reply is only a
            // RunPlan — output, the load banner and the pane updates all
            // travel the event stream, which the pump is already fanning out.
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                let state = app.state::<HostState>();
                let submitted = state
                    .request(EngineRequest::ExecSubmit {
                        session,
                        intent: RunIntent::CommandBar { text },
                        inline_mode: InlineResultsMode::Always,
                    })
                    .await;
                match submitted {
                    Ok(EngineResponse::Submitted { .. }) => {}
                    Ok(EngineResponse::Error(e)) => {
                        eprintln!("stratum-desktop: the engine refused to load {path}: {e}");
                    }
                    Ok(_) => {
                        eprintln!("stratum-desktop: unexpected engine reply while loading {path}");
                    }
                    Err(e) => {
                        eprintln!("stratum-desktop: could not submit `use` for {path}: {e}");
                    }
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification_is_by_extension_case_insensitively() {
        assert_eq!(
            classify("/a/model.do"),
            Ok(FileAction::OpenDo("/a/model.do".into()))
        );
        assert_eq!(
            classify("/a/MODEL.DO"),
            Ok(FileAction::OpenDo("/a/MODEL.DO".into()))
        );
        assert_eq!(
            classify("/data/auto.dta"),
            Ok(FileAction::UseDta("/data/auto.dta".into()))
        );
        assert_eq!(
            classify("C:\\data\\AUTO.Dta"),
            Ok(FileAction::UseDta("C:\\data\\AUTO.Dta".into()))
        );
    }

    #[test]
    fn other_files_are_refused_with_the_path_in_the_message() {
        for raw in ["/a/notes.txt", "/a/do", "/a/dta", "/a/archive.dta.gz"] {
            let err = classify(raw).expect_err("only .do and .dta open");
            assert!(err.contains(raw), "{err} names {raw}");
        }
    }

    #[test]
    fn the_use_command_is_what_a_user_would_type() {
        assert_eq!(
            use_command(Utf8Path::new("/data/auto.dta")).as_deref(),
            Ok("use \"/data/auto.dta\", clear")
        );
        // Spaces are exactly why the path is quoted.
        assert_eq!(
            use_command(Utf8Path::new("/My Data/auto 2.dta")).as_deref(),
            Ok("use \"/My Data/auto 2.dta\", clear")
        );
    }

    #[test]
    fn a_path_stata_cannot_quote_is_refused_not_garbled() {
        assert!(use_command(Utf8Path::new("/a/\"quoted\".dta")).is_err());
        assert!(use_command(Utf8Path::new("/a/line\nbreak.dta")).is_err());
    }

    #[test]
    fn pending_opens_drain_in_arrival_order_and_only_once() {
        let queue = PendingOpens::new();
        queue.push(FileAction::UseDta("/data/auto.dta".into()));
        queue.push(FileAction::OpenDo("/a/model.do".into()));
        assert_eq!(
            queue.take(),
            vec![
                FileAction::UseDta("/data/auto.dta".into()),
                FileAction::OpenDo("/a/model.do".into()),
            ]
        );
        assert_eq!(queue.take(), Vec::new(), "a drain leaves the queue empty");
    }
}
