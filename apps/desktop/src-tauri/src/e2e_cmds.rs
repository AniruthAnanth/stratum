//! The host half of the e2e bridge — **fenced behind `--features e2e`** (W25).
//!
//! ADR-011: "a test-only IPC command that reaches production is a remote-control
//! backdoor." So everything that touches Tauri in this file lives under
//! `#[cfg(feature = "e2e")]`, `cargo xtask e2e --check-fence <binary>` greps a
//! built artifact for [`E2E_DISPATCH`] and [`E2E_SNAPSHOT`], and
//! `.github/workflows/e2e.yml` runs that check against a release build.
//!
//! # COMPILED — but not yet by the crate that ships it
//!
//! Through repair rounds 1 and 2 this file was declared by nothing and therefore
//! **compiled by nothing**: 683 lines and four tests that no compiler had seen,
//! green in nobody's CI. Round 3 ended that from W25's own side —
//! `crates/stratum-e2e/src/host.rs` pulls this file in with one `#[path]`, so
//! everything below is built, clippy-checked and run by every `cargo test
//! --workspace` on three OSes, and `stratum-e2e-host-probe` is a real linked
//! artifact carrying [`E2E_DISPATCH`] and [`E2E_SNAPSHOT`] for the fence's
//! positive control.
//!
//! That does **not** close the registration. `apps/desktop/src-tauri/Cargo.toml`
//! and `src/main.rs` belong to **W17** and W25 owns neither (R0), so the *ship-
//! ping* crate still does not compile this module and the fence is still not the
//! feature-gate differential it should be. The three lines it needs are written
//! down rather than taken:
//!
//! ```text
//! # apps/desktop/src-tauri/Cargo.toml
//! [features]
//! e2e = []                       # OFF in every shipped build; once W17 adds
//!                                # tauri this becomes e2e = ["dep:tauri"] only
//!                                # if tauri is made optional — otherwise leave
//!                                # it empty, the gate is on the module below.
//!
//! # apps/desktop/src-tauri/src/main.rs
//! #[allow(dead_code)]            // until ipc.rs consumes it
//! mod e2e_cmds;                  // UNCONDITIONAL — see "measured" below
//! # …and in the builder:
//! #[cfg(feature = "e2e")]
//! let builder = e2e_cmds::tauri_surface::attach(builder);
//! ```
//!
//! **Declare the module unconditionally, not under `#[cfg(feature = "e2e")]`.**
//! Everything above `tauri_surface` is plain `std` + `serde` — no Tauri at
//! all — so gating the whole module would leave the ~450 lines with the logic
//! in them, and the four tests at the bottom, compiled by nothing on every
//! ordinary build. That is the defect this file was in before repair round 1;
//! repeating it one level down would be no better.
//!
//! MEASURED, repair round 1, not assumed: with `mod e2e_cmds;` declared
//! unconditionally and the `e2e` feature OFF, `grep -a` for each of the two
//! fenced names found **0 of 2 present** in both `target/debug/stratum-desktop`
//! and `target/release/stratum-desktop`. The names live only inside
//! `tauri_surface`, which the feature gate removes, so nothing references the
//! constants and they never reach `.rodata`. `cargo test -p stratum-desktop`
//! went 19 -> 23 passing and `cargo clippy -p stratum-desktop --all-targets --
//! -D warnings` stayed clean. `--features e2e` then failed on exactly one
//! thing — `unresolved import tauri`, E0432 at the `use` inside
//! `tauri_surface` — because the manifest has no `tauri` dependency yet.
//!
//! That measurement is a property, not a promise: `.github/workflows/e2e.yml`'s
//! `fence` job asserts it on every push, and gains a positive control (the
//! `--features e2e` build must CONTAIN both names) the moment the two lines
//! above land, so a fence that starts passing vacuously fails instead.
//!
//! # The shape, and why it is this shape
//!
//! A Tauri command is invoked *by the webview*. An external test harness is not
//! a webview, so it cannot invoke one. The host is therefore a small server:
//!
//! ```text
//!   harness ──socket──> Control ──emit──> the frontend's e2e bridge
//!           <──socket──               <─invoke─ e2e_reply
//! ```
//!
//! [`Control`] — the whole correlation, framing and deadline half — is
//! plain `std` and has no Tauri in it at all. That is deliberate: it is the part
//! with logic in it, so it is the part that has to be testable, and the tests at
//! the bottom of this file run today against a fake webview.
//!
//! `e2e_dispatch` and `e2e_snapshot` exist as commands as well, because the
//! plan names them and because a developer with the devtools console open should
//! be able to drive the same path by hand. Both are thin: they call the same
//! inner functions the socket calls.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The command names the fence greps for. Named as constants so the fence check
/// and the registration cannot drift apart silently.
pub const E2E_DISPATCH: &str = "e2e_dispatch";
/// See [`E2E_DISPATCH`].
pub const E2E_SNAPSHOT: &str = "e2e_snapshot";

/// Environment variable carrying the harness's loopback port.
pub const PORT_ENV: &str = "STRATUM_E2E_PORT";
/// Environment variable carrying the harness's host. Defaults to `127.0.0.1`.
pub const HOST_ENV: &str = "STRATUM_E2E_HOST";
/// The event the webview listens on.
pub const REQUEST_EVENT: &str = "stratum://e2e-request";

/// How long the host waits for the webview to answer one request.
///
/// Shorter than the harness's own 30 s deadline on purpose: if the webview is
/// wedged, the host should be the one to say so — it can name the request — and
/// the harness's timeout should be the backstop that reports the last snapshot.
const WEBVIEW_TIMEOUT: Duration = Duration::from_secs(20);

/// What went wrong.
#[derive(Debug)]
pub enum E2eError {
    /// The webview did not answer inside [`WEBVIEW_TIMEOUT`].
    WebviewTimeout {
        /// Which request.
        op: String,
    },
    /// The webview answered with an error.
    Webview(String),
    /// The control channel broke.
    Transport(String),
}

impl std::fmt::Display for E2eError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WebviewTimeout { op } => {
                write!(f, "the webview did not answer {op} within 20 s")
            }
            Self::Webview(m) => write!(f, "webview: {m}"),
            Self::Transport(m) => write!(f, "transport: {m}"),
        }
    }
}

impl std::error::Error for E2eError {}

/// One request from the harness.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// Handshake.
    Hello {
        /// Correlation id.
        id: u32,
        /// The harness's version string.
        #[serde(default)]
        harness: String,
    },
    /// Run an action.
    Dispatch {
        /// Correlation id.
        id: u32,
        /// The action, forwarded to the webview untouched.
        action: Value,
    },
    /// Read the app.
    Snapshot {
        /// Correlation id.
        id: u32,
        /// Which sections.
        what: Value,
    },
    /// Shut the channel.
    Quit {
        /// Correlation id.
        id: u32,
    },
}

impl Request {
    /// The correlation id, which every reply must carry back.
    #[must_use]
    pub const fn id(&self) -> u32 {
        match self {
            Self::Hello { id, .. }
            | Self::Dispatch { id, .. }
            | Self::Snapshot { id, .. }
            | Self::Quit { id } => *id,
        }
    }

    /// A name for a deadline message.
    #[must_use]
    pub const fn op(&self) -> &'static str {
        match self {
            Self::Hello { .. } => "hello",
            Self::Dispatch { .. } => E2E_DISPATCH,
            Self::Snapshot { .. } => E2E_SNAPSHOT,
            Self::Quit { .. } => "quit",
        }
    }
}

/// One reply to the harness.
#[derive(Clone, Debug, Serialize)]
pub struct Response {
    /// The request's correlation id.
    pub id: u32,
    /// Whether it worked.
    pub ok: bool,
    /// Why not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Host name, on `hello`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// Advertised capabilities, on `hello`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<Value>,
    /// The dispatch result.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dispatched: Option<Value>,
    /// The snapshot.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<Value>,
}

impl Response {
    /// An `ok: false` reply.
    #[must_use]
    pub fn failed(id: u32, error: &str) -> Self {
        Self {
            id,
            ok: false,
            error: Some(error.to_owned()),
            host: None,
            capabilities: None,
            dispatched: None,
            snapshot: None,
        }
    }

    /// An `ok: true` reply carrying nothing.
    #[must_use]
    pub const fn ok(id: u32) -> Self {
        Self {
            id,
            ok: true,
            error: None,
            host: None,
            capabilities: None,
            dispatched: None,
            snapshot: None,
        }
    }
}

/// What the webview is, from this file's point of view.
///
/// A trait rather than `tauri::AppHandle` so the correlation logic can be tested
/// without a window, a webview or a display server — the three things that make
/// a GUI test flaky.
pub trait Webview: Send + Sync {
    /// Ask the webview to run one request, and hand back the id it will answer
    /// with. Implemented by emitting [`REQUEST_EVENT`].
    ///
    /// # Errors
    /// When the event cannot be emitted.
    fn ask(&self, id: u32, op: &str, payload: &Value) -> Result<(), E2eError>;
}

/// The pending-reply table: correlation id → the channel waiting for it.
#[derive(Default)]
pub struct Pending {
    waiting: Mutex<HashMap<u32, Sender<Result<Value, String>>>>,
}

impl Pending {
    /// Register a waiter and hand back its receiving half.
    #[must_use]
    pub fn register(&self, id: u32) -> Receiver<Result<Value, String>> {
        let (tx, rx) = std::sync::mpsc::channel();
        if let Ok(mut waiting) = self.waiting.lock() {
            waiting.insert(id, tx);
        }
        rx
    }

    /// Complete a waiter. Returns false when nobody was waiting for that id —
    /// a late reply to a request that already timed out, which must be dropped
    /// rather than handed to whoever is waiting now.
    pub fn complete(&self, id: u32, value: Result<Value, String>) -> bool {
        let Ok(mut waiting) = self.waiting.lock() else {
            return false;
        };
        match waiting.remove(&id) {
            Some(tx) => tx.send(value).is_ok(),
            None => false,
        }
    }

    /// Forget a waiter that timed out.
    pub fn forget(&self, id: u32) {
        if let Ok(mut waiting) = self.waiting.lock() {
            waiting.remove(&id);
        }
    }

    /// How many replies are outstanding. A leak here is a leak of test-only
    /// state in a build that should not have any.
    #[must_use]
    pub fn outstanding(&self) -> usize {
        self.waiting.lock().map_or(0, |w| w.len())
    }
}

/// The host's e2e state: the webview to ask, and who is waiting for what.
pub struct Control {
    webview: Arc<dyn Webview>,
    pending: Arc<Pending>,
    next: AtomicU32,
}

impl Control {
    /// Build a control surface over a webview.
    #[must_use]
    pub fn new(webview: Arc<dyn Webview>) -> Self {
        Self {
            webview,
            pending: Arc::new(Pending::default()),
            next: AtomicU32::new(0),
        }
    }

    /// The pending table, for `e2e_reply`.
    #[must_use]
    pub fn pending(&self) -> Arc<Pending> {
        Arc::clone(&self.pending)
    }

    /// Ask the webview one question and wait for its answer.
    ///
    /// # Errors
    /// Emit failures, webview-side errors, and the 20 s deadline.
    pub fn ask(&self, op: &str, payload: &Value) -> Result<Value, E2eError> {
        let id = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        let rx = self.pending.register(id);
        self.webview.ask(id, op, payload)?;
        match rx.recv_timeout(WEBVIEW_TIMEOUT) {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(message)) => Err(E2eError::Webview(message)),
            Err(RecvTimeoutError::Timeout) => {
                self.pending.forget(id);
                Err(E2eError::WebviewTimeout { op: op.to_owned() })
            }
            Err(RecvTimeoutError::Disconnected) => {
                self.pending.forget(id);
                Err(E2eError::Transport("the webview went away".to_owned()))
            }
        }
    }

    /// Answer one harness request.
    #[must_use]
    pub fn answer(&self, request: &Request) -> Response {
        let id = request.id();
        match request {
            Request::Hello { .. } => match self.ask("capabilities", &Value::Null) {
                Ok(capabilities) => Response {
                    id,
                    ok: true,
                    error: None,
                    host: Some("stratum-desktop (--features e2e)".to_owned()),
                    capabilities: Some(capabilities),
                    dispatched: None,
                    snapshot: None,
                },
                Err(e) => Response::failed(id, &e.to_string()),
            },
            Request::Dispatch { action, .. } => match self.ask(E2E_DISPATCH, action) {
                Ok(dispatched) => Response {
                    dispatched: Some(dispatched),
                    ..Response::ok(id)
                },
                Err(e) => Response::failed(id, &e.to_string()),
            },
            Request::Snapshot { what, .. } => match self.ask(E2E_SNAPSHOT, what) {
                Ok(snapshot) => Response {
                    snapshot: Some(snapshot),
                    ..Response::ok(id)
                },
                Err(e) => Response::failed(id, &e.to_string()),
            },
            Request::Quit { .. } => Response::ok(id),
        }
    }
}

/// The loopback server: read a line, answer it, write a line.
///
/// Connects OUT to the harness rather than listening, so the app never opens a
/// port. A test-only build that listened would be a test-only build with an
/// attack surface, which is the thing ADR-011 is about.
///
/// # Errors
/// Connection and I/O failures.
pub fn serve(control: &Control, port: u16, host: &str) -> Result<(), E2eError> {
    let stream = TcpStream::connect((host, port))
        .map_err(|e| E2eError::Transport(format!("connecting to the harness: {e}")))?;
    stream
        .set_nodelay(true)
        .map_err(|e| E2eError::Transport(e.to_string()))?;
    let reader = BufReader::new(
        stream
            .try_clone()
            .map_err(|e| E2eError::Transport(e.to_string()))?,
    );
    serve_on(control, reader, stream)
}

/// [`serve`] over any pair of streams, so the loop is testable.
///
/// # Errors
/// I/O failures. A malformed request is answered, not fatal.
pub fn serve_on<R: BufRead, W: Write>(
    control: &Control,
    reader: R,
    mut writer: W,
) -> Result<(), E2eError> {
    for line in reader.lines() {
        let line = line.map_err(|e| E2eError::Transport(e.to_string()))?;
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Request>(&line) {
            Ok(request) => {
                let quit = matches!(request, Request::Quit { .. });
                let response = control.answer(&request);
                write_line(&mut writer, &response)?;
                if quit {
                    return Ok(());
                }
                continue;
            }
            // Id 0 is never a real correlation id, so a harness that gets this
            // reports "reply id 0 does not answer request id N" rather than
            // silently treating a parse failure as an answer.
            Err(e) => Response::failed(0, &format!("unparseable request: {e}")),
        };
        write_line(&mut writer, &response)?;
    }
    Ok(())
}

fn write_line<W: Write>(writer: &mut W, response: &Response) -> Result<(), E2eError> {
    let mut line =
        serde_json::to_string(response).map_err(|e| E2eError::Transport(e.to_string()))?;
    line.push('\n');
    writer
        .write_all(line.as_bytes())
        .and_then(|()| writer.flush())
        .map_err(|e| E2eError::Transport(e.to_string()))
}

/// The port the harness asked us to call back on, if any.
#[must_use]
pub fn harness_port() -> Option<u16> {
    std::env::var(PORT_ENV).ok()?.parse().ok()
}

/// The host the harness is listening on.
#[must_use]
pub fn harness_host() -> String {
    std::env::var(HOST_ENV).unwrap_or_else(|_| "127.0.0.1".to_owned())
}

// ---------------------------------------------------------------------------
// The Tauri surface. Fenced.
// ---------------------------------------------------------------------------

/// The commands, and the builder hook that registers them.
///
/// Everything Tauri-shaped is inside this module and inside the feature gate, so
/// a build without `--features e2e` contains neither the command names nor the
/// handlers — which is what `xtask e2e --check-fence` asserts about the shipped
/// artifact.
#[cfg(feature = "e2e")]
pub mod tauri_surface {
    use std::sync::Arc;

    use serde_json::Value;
    use tauri::{Emitter, Manager, Runtime};

    use super::{Control, E2eError, Pending, Webview, E2E_DISPATCH, E2E_SNAPSHOT, REQUEST_EVENT};

    /// The real webview: emit the request and let the frontend answer with
    /// `e2e_reply`.
    struct AppWebview<R: Runtime> {
        app: tauri::AppHandle<R>,
    }

    impl<R: Runtime> Webview for AppWebview<R> {
        fn ask(&self, id: u32, op: &str, payload: &Value) -> Result<(), E2eError> {
            self.app
                .emit(
                    REQUEST_EVENT,
                    serde_json::json!({ "id": id, "op": op, "payload": payload }),
                )
                .map_err(|e| E2eError::Transport(e.to_string()))
        }
    }

    /// `e2e_dispatch { action, args }` — the plan's name, kept.
    #[tauri::command(rename_all = "snake_case")]
    pub async fn e2e_dispatch<R: Runtime>(
        app: tauri::AppHandle<R>,
        action: Value,
    ) -> Result<Value, String> {
        let control = app.state::<Arc<Control>>();
        control
            .ask(E2E_DISPATCH, &action)
            .map_err(|e| e.to_string())
    }

    /// `e2e_snapshot { what }`.
    #[tauri::command(rename_all = "snake_case")]
    pub async fn e2e_snapshot<R: Runtime>(
        app: tauri::AppHandle<R>,
        what: Value,
    ) -> Result<Value, String> {
        let control = app.state::<Arc<Control>>();
        control.ask(E2E_SNAPSHOT, &what).map_err(|e| e.to_string())
    }

    /// The frontend's answer to one emitted request.
    #[tauri::command(rename_all = "snake_case")]
    pub fn e2e_reply<R: Runtime>(
        app: tauri::AppHandle<R>,
        id: u32,
        ok: bool,
        payload: Value,
    ) -> Result<(), String> {
        let pending = app.state::<Arc<Pending>>();
        let value = if ok {
            Ok(payload)
        } else {
            Err(payload.to_string())
        };
        if pending.complete(id, value) {
            Ok(())
        } else {
            // Not an error the frontend can do anything about, but worth saying:
            // it means a request timed out and its answer arrived afterwards.
            Err(format!("nobody is waiting for reply {id}"))
        }
    }

    /// Register the commands and start the control server, if the harness asked.
    #[must_use]
    pub fn attach<R: Runtime>(builder: tauri::Builder<R>) -> tauri::Builder<R> {
        builder
            .invoke_handler(tauri::generate_handler![
                e2e_dispatch,
                e2e_snapshot,
                e2e_reply
            ])
            .setup(|app| {
                let control = Arc::new(Control::new(Arc::new(AppWebview {
                    app: app.handle().clone(),
                })));
                app.manage(control.pending());
                app.manage(Arc::clone(&control));
                if let Some(port) = super::harness_port() {
                    let host = super::harness_host();
                    std::thread::spawn(move || {
                        if let Err(e) = super::serve(&control, port, &host) {
                            eprintln!("e2e control channel: {e}");
                        }
                    });
                }
                Ok(())
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A webview that answers immediately, from another thread, the way the real
    /// one does.
    struct Echo {
        pending: Arc<Pending>,
        answer: Value,
    }

    impl Webview for Echo {
        fn ask(&self, id: u32, op: &str, _payload: &Value) -> Result<(), E2eError> {
            let pending = Arc::clone(&self.pending);
            let answer = self.answer.clone();
            let op = op.to_owned();
            std::thread::spawn(move || {
                pending.complete(id, Ok(serde_json::json!({ "op": op, "answer": answer })));
            });
            Ok(())
        }
    }

    /// A webview that never answers.
    struct Mute;

    impl Webview for Mute {
        fn ask(&self, _id: u32, _op: &str, _payload: &Value) -> Result<(), E2eError> {
            Ok(())
        }
    }

    fn control_with_echo() -> Control {
        let pending = Arc::new(Pending::default());
        Control {
            webview: Arc::new(Echo {
                pending: Arc::clone(&pending),
                answer: Value::from(42),
            }),
            pending,
            next: AtomicU32::new(0),
        }
    }

    #[test]
    fn a_request_is_answered_with_its_own_correlation_id() {
        let control = control_with_echo();
        let requests = concat!(
            r#"{"op":"hello","id":1,"harness":"stratum-e2e/0.1.0"}"#,
            "\n",
            r#"{"op":"snapshot","id":2,"what":["layout"]}"#,
            "\n",
            r#"{"op":"quit","id":3}"#,
            "\n",
        );
        let mut out = Vec::new();
        serve_on(&control, requests.as_bytes(), &mut out).expect("the loop runs");

        let lines: Vec<&str> = std::str::from_utf8(&out).unwrap().lines().collect();
        assert_eq!(lines.len(), 3, "one reply per request, and quit ends it");
        for (i, line) in lines.iter().enumerate() {
            let v: Value = serde_json::from_str(line).unwrap();
            assert_eq!(v["id"].as_u64(), Some(i as u64 + 1));
            assert_eq!(v["ok"].as_bool(), Some(true));
        }
        assert!(lines[0].contains("capabilities"));
        assert!(lines[1].contains("snapshot"));
    }

    #[test]
    fn an_unparseable_request_is_answered_rather_than_fatal() {
        let control = control_with_echo();
        let mut out = Vec::new();
        serve_on(&control, "not json\n".as_bytes(), &mut out).expect("a bad line is not fatal");
        let v: Value = serde_json::from_str(std::str::from_utf8(&out).unwrap().trim()).unwrap();
        assert_eq!(v["ok"].as_bool(), Some(false));
        // Id 0 is never a real correlation id, so the harness reports a desync
        // rather than accepting this as an answer to whatever it last asked.
        assert_eq!(v["id"].as_u64(), Some(0));
    }

    #[test]
    fn a_silent_webview_becomes_an_error_with_the_operation_in_it() {
        // The deadline is 20 s, which no unit test may wait for. Assert the
        // machinery instead: nobody is waiting after a `forget`, and a late
        // reply is dropped rather than handed to the next waiter.
        let pending = Pending::default();
        let rx = pending.register(7);
        pending.forget(7);
        assert_eq!(pending.outstanding(), 0);
        assert!(
            !pending.complete(7, Ok(Value::Null)),
            "a late reply is dropped"
        );
        drop(rx);

        let control = Control::new(Arc::new(Mute));
        assert_eq!(control.pending().outstanding(), 0);
    }

    #[test]
    fn the_fenced_command_names_are_the_ones_the_fence_greps_for() {
        // `xtask e2e --check-fence` scans a built binary for exactly these two
        // strings. If a command is renamed here and not there, the fence goes
        // quietly green on a binary that still has the backdoor in it.
        assert_eq!(E2E_DISPATCH, "e2e_dispatch");
        assert_eq!(E2E_SNAPSHOT, "e2e_snapshot");
    }
}
