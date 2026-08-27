//! **Tier 1 — the host harness.** macOS, Windows and Linux, inside `cargo nextest`.
//!
//! The harness talks to the app over a line-oriented JSON control channel on
//! loopback, and the app answers from inside the same command registry the
//! keymap and the command palette use. Two things about that are deliberate.
//!
//! **A socket rather than stdio.** The packaged app's stdout belongs to the app;
//! a webview, a native menu or a crash reporter may write to it at any time, and
//! a control protocol multiplexed onto a stream somebody else also writes to is
//! a protocol with an intermittent failure mode. The harness binds
//! `127.0.0.1:0`, hands the port to the child in `STRATUM_E2E_PORT`, and the
//! child connects back.
//!
//! **The child is chosen at run time, not compiled in.** Today there is no
//! packaged app: W17's `stratum-desktop` binary is a placeholder that exits 64.
//! So [`HostSpec::PreHost`] runs the *same* `apps/desktop/src/e2e` bridge inside
//! node, over the real W12 stores, and advertises exactly the capabilities that
//! actually exist there — no editor, no cards, no panes. [`HostSpec::Packaged`]
//! is the same protocol against the real binary and is what the acceptance
//! bullets ultimately mean; it is selected the moment a binary is handed to it.
//! Neither one is a mock of the other: they are two hosts of one protocol, and
//! the report always says which answered.
//!
//! # Waiting without sleeping (ADR-017)
//!
//! Every wait in this file is a blocking read with a deadline — `recv_timeout`
//! for the connect, `set_read_timeout` for each reply. Nothing sleeps and
//! nothing polls, which is why [`crate::Counters::sleeps`] and
//! [`crate::Counters::polls`] can be asserted to be zero rather than merely
//! believed to be small.

use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::actions::{Action, Dispatched};
use crate::snapshot::{Snapshot, What};
use crate::{Capabilities, Counters, Driver, DriverError, Tier};

/// How long to wait for the child to call back. Node has to boot vite and
/// transform the frontend's module graph, which on a cold cache is not fast.
///
/// Not a performance budget (ADR-017 forbids reading it as one): it is the
/// deadline that turns "the host never started" into a sentence instead of a
/// hang. When it fires, [`Tier1Driver::launch`] asks the child once whether it
/// has already exited, so the message says *why* nobody connected.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(60);
/// How long any single request may take. Generous: this is a deadline that
/// exists to turn a hang into a *report*, not a performance budget (ADR-017
/// forbids the second reading).
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Which host to drive.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum HostSpec {
    /// The frontend e2e bridge under node, over the real W12 stores. Available
    /// today; advertises no editor, no cards and no panes, because there are
    /// none.
    PreHost {
        /// Repo root; `apps/desktop` beneath it is the working directory.
        root: PathBuf,
    },
    /// The packaged app, built with `--features e2e`. What the acceptance
    /// bullets mean by "the host". Needs W17.
    Packaged {
        /// The binary to launch.
        binary: PathBuf,
    },
}

impl HostSpec {
    /// A short name for the report.
    #[must_use]
    pub fn name(&self) -> String {
        match self {
            Self::PreHost { .. } => "pre-host bridge (node + the real W12 stores)".to_owned(),
            Self::Packaged { binary } => format!("packaged app {}", binary.display()),
        }
    }
}

// ---------------------------------------------------------------------------
// The wire
// ---------------------------------------------------------------------------

/// Harness → host. One JSON object per line.
#[derive(Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Request<'a> {
    Hello { id: u32, harness: &'a str },
    Dispatch { id: u32, action: &'a Action },
    Snapshot { id: u32, what: &'a What },
    Quit { id: u32 },
}

/// Host → harness.
#[derive(Deserialize)]
struct Response {
    id: u32,
    ok: bool,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    host: Option<String>,
    #[serde(default)]
    capabilities: Option<Capabilities>,
    #[serde(default)]
    dispatched: Option<Dispatched>,
    #[serde(default)]
    snapshot: Option<Box<Snapshot>>,
}

/// A Tier-1 connection to a host.
pub struct Tier1Driver {
    spec: HostSpec,
    host_name: String,
    caps: Capabilities,
    stream: TcpStream,
    reader: BufReader<TcpStream>,
    child: Option<Child>,
    next_id: u32,
    counters: Counters,
}

impl Tier1Driver {
    /// Launch a host and complete the handshake.
    ///
    /// # Errors
    /// Spawn failures, a child that never connects, and a malformed handshake.
    pub fn launch(spec: HostSpec) -> Result<Self, DriverError> {
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .map_err(|e| DriverError::Transport(format!("binding loopback: {e}")))?;
        let port = listener
            .local_addr()
            .map_err(|e| DriverError::Transport(format!("reading the bound port: {e}")))?
            .port();

        let mut child = spawn(&spec, port)?;

        // A blocking accept on a worker, collected with a deadline. The obvious
        // alternative — set_nonblocking plus a retry loop — is a poll, and a
        // harness that polls cannot honestly assert `polls == 0`.
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(listener.accept().map(|(s, _)| s));
        });
        let stream = match rx.recv_timeout(CONNECT_TIMEOUT) {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => {
                let _ = child.kill();
                return Err(DriverError::Transport(format!("accept: {e}")));
            }
            Err(_) => {
                // One `try_wait`, not a loop: "the host exited with status 64
                // before connecting" and "the host is running but never called
                // back" are different bugs and deserve different sentences.
                let exited = child.try_wait().ok().flatten();
                let _ = child.kill();
                return Err(match exited {
                    Some(status) => DriverError::Unsupported(format!(
                        "{} exited with {status} without connecting back on port {port}",
                        spec.name()
                    )),
                    None => DriverError::Timeout {
                        op: format!("{} to connect back on port {port}", spec.name()),
                        after_ms: as_ms(CONNECT_TIMEOUT),
                    },
                });
            }
        };
        stream
            .set_read_timeout(Some(REQUEST_TIMEOUT))
            .map_err(|e| DriverError::Transport(format!("set_read_timeout: {e}")))?;
        stream
            .set_nodelay(true)
            .map_err(|e| DriverError::Transport(format!("set_nodelay: {e}")))?;
        let reader = BufReader::new(
            stream
                .try_clone()
                .map_err(|e| DriverError::Transport(format!("cloning the socket: {e}")))?,
        );

        let mut driver = Self {
            host_name: spec.name(),
            spec,
            caps: Capabilities::default(),
            stream,
            reader,
            child: Some(child),
            next_id: 0,
            counters: Counters::default(),
        };

        let id = driver.next_id();
        let hello = driver.exchange(&Request::Hello {
            id,
            harness: concat!("stratum-e2e/", env!("CARGO_PKG_VERSION")),
        })?;
        driver.caps = hello.capabilities.unwrap_or_default();
        if let Some(name) = hello.host {
            driver.host_name = name;
        }
        Ok(driver)
    }

    /// What the host said it can do.
    #[must_use]
    pub fn advertised(&self) -> &Capabilities {
        &self.caps
    }

    /// Which host this is.
    #[must_use]
    pub const fn spec(&self) -> &HostSpec {
        &self.spec
    }

    fn next_id(&mut self) -> u32 {
        self.next_id += 1;
        self.next_id
    }

    fn exchange(&mut self, req: &Request<'_>) -> Result<Response, DriverError> {
        let mut line = serde_json::to_string(req)
            .map_err(|e| DriverError::Transport(format!("encoding a request: {e}")))?;
        line.push('\n');
        self.counters.bytes_tx += line.len() as u64;
        self.counters.round_trips += 1;
        self.stream
            .write_all(line.as_bytes())
            .and_then(|()| self.stream.flush())
            .map_err(|e| DriverError::Transport(format!("writing a request: {e}")))?;

        let mut buf = String::new();
        match self.reader.read_line(&mut buf) {
            Ok(0) => {
                return Err(DriverError::Transport(
                    "the host closed the control channel".to_owned(),
                ))
            }
            Ok(n) => self.counters.bytes_rx += n as u64,
            Err(e) if is_timeout(&e) => {
                return Err(DriverError::Timeout {
                    op: op_of(req),
                    after_ms: as_ms(REQUEST_TIMEOUT),
                })
            }
            Err(e) => return Err(DriverError::Transport(format!("reading a reply: {e}"))),
        }

        let resp: Response = serde_json::from_str(buf.trim_end())
            .map_err(|e| DriverError::Transport(format!("decoding a reply: {e}: {buf}")))?;
        // A reply carrying somebody else's id means the channel has desynced —
        // a reply to a request we gave up on, say. Reading it as the answer to
        // THIS request would make every later assertion a lie about the wrong
        // moment, so it is a transport error rather than a value.
        let want = id_of(req);
        if resp.id != want {
            return Err(DriverError::Transport(format!(
                "reply id {} does not answer request id {want}",
                resp.id
            )));
        }
        if !resp.ok {
            return Err(DriverError::Host(
                resp.error.unwrap_or_else(|| "unspecified".to_owned()),
            ));
        }
        Ok(resp)
    }
}

const fn id_of(req: &Request<'_>) -> u32 {
    match req {
        Request::Hello { id, .. }
        | Request::Dispatch { id, .. }
        | Request::Snapshot { id, .. }
        | Request::Quit { id } => *id,
    }
}

fn op_of(req: &Request<'_>) -> String {
    match req {
        Request::Hello { .. } => "hello",
        Request::Dispatch { .. } => "e2e_dispatch",
        Request::Snapshot { .. } => "e2e_snapshot",
        Request::Quit { .. } => "quit",
    }
    .to_owned()
}

fn as_ms(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

fn is_timeout(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    )
}

fn spawn(spec: &HostSpec, port: u16) -> Result<Child, DriverError> {
    let mut cmd = match spec {
        HostSpec::PreHost { root } => {
            let app = root.join("apps/desktop");
            if !app.join("node_modules").is_dir() {
                return Err(DriverError::Unsupported(format!(
                    "{} has no node_modules; run `pnpm install --frozen-lockfile` in \
                     apps/desktop before `cargo xtask e2e --tier 1`",
                    app.display()
                )));
            }
            let runner = std::env::var("STRATUM_E2E_RUNNER").unwrap_or_else(|_| "pnpm".to_owned());
            let mut c = Command::new(runner);
            // `vitest run <file>` filters by path against the config's own
            // `include`, so the bridge is transformed by the same vite pipeline
            // — and the same `resolve.conditions` — the app itself uses. A
            // second module resolver would be a second answer to "what does
            // `../state/results` mean".
            c.args(["exec", "vitest", "run", "src/e2e/serve.test.ts"]);
            c.current_dir(&app);
            c
        }
        HostSpec::Packaged { binary } => {
            if !binary.is_file() {
                return Err(DriverError::Unsupported(format!(
                    "no such binary: {}",
                    binary.display()
                )));
            }
            Command::new(binary)
        }
    };
    cmd.env("STRATUM_E2E_PORT", port.to_string())
        .env("STRATUM_E2E_HOST", "127.0.0.1")
        // A host that opens a real window during a headless CI run is a host
        // that hangs. W17's `e2e` feature honours this.
        .env("STRATUM_E2E", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    cmd.spawn().map_err(|e| {
        DriverError::Unsupported(format!("could not start the host ({}): {e}", spec.name()))
    })
}

impl Driver for Tier1Driver {
    fn tier(&self) -> Tier {
        Tier::One
    }

    fn host(&self) -> String {
        self.host_name.clone()
    }

    fn capabilities(&self) -> Capabilities {
        self.caps.clone()
    }

    fn dispatch(&mut self, action: &Action) -> Result<Dispatched, DriverError> {
        let id = self.next_id();
        self.counters.dispatches += 1;
        let resp = self.exchange(&Request::Dispatch { id, action })?;
        let dispatched = resp
            .dispatched
            .ok_or_else(|| DriverError::Host("a dispatch reply with no result".to_owned()))?;
        self.counters.events_fed += dispatched.events_applied;
        Ok(dispatched)
    }

    fn snapshot(&mut self, what: &What) -> Result<Snapshot, DriverError> {
        let id = self.next_id();
        self.counters.snapshots += 1;
        let resp = self.exchange(&Request::Snapshot { id, what })?;
        resp.snapshot
            .map(|b| *b)
            .ok_or_else(|| DriverError::Host("a snapshot reply with no snapshot".to_owned()))
    }

    fn counters(&self) -> Counters {
        self.counters
    }
}

impl Drop for Tier1Driver {
    fn drop(&mut self) {
        let id = self.next_id();
        // Best effort: ask, then insist. A host left running holds the port and
        // the next `cargo nextest` run inherits somebody else's app.
        let _ = self.exchange(&Request::Quit { id });
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Locate the repo root the same way [`crate::fixtures::repo_root`] does.
///
/// # Errors
/// When no ancestor of this crate contains `docs/ownership.toml`.
pub fn repo_root() -> Result<PathBuf, DriverError> {
    crate::fixtures::repo_root().map_err(|e| DriverError::Unsupported(e.to_string()))
}

/// The name of the dev-only dispatch command, which is also how a binary is
/// recognised as e2e-capable. Kept in step with
/// `apps/desktop/src-tauri/src/e2e_cmds.rs::E2E_DISPATCH` and with
/// `xtask e2e --check-fence`, which greps a *shipped* binary for the same
/// string and fails if it is there.
pub const E2E_DISPATCH: &str = "e2e_dispatch";

/// Whether a built binary was compiled with `--features e2e`.
///
/// A byte scan for [`E2E_DISPATCH`], which is the exact inverse of the fence
/// check. Presence of the file is NOT enough and the difference is not
/// theoretical: `stratum-desktop`'s `main` is currently W17's placeholder, which
/// prints one line and exits 64. Choosing it because it exists cost a 120-second
/// timeout per scenario and reported it as "the host never connected", which is
/// true and useless.
#[must_use]
pub fn is_e2e_capable(binary: &Path) -> bool {
    let Ok(bytes) = std::fs::read(binary) else {
        return false;
    };
    let needle = E2E_DISPATCH.as_bytes();
    bytes.windows(needle.len()).any(|w| w == needle)
}

/// The host to use when nobody named one.
///
/// `STRATUM_E2E_APP` wins (that is what `xtask e2e --app` sets); then a built
/// binary that is actually e2e-capable; then the pre-host bridge.
///
/// # Errors
/// When the repo root cannot be found.
pub fn default_host() -> Result<HostSpec, DriverError> {
    let root = repo_root()?;
    if let Some(app) = std::env::var_os("STRATUM_E2E_APP") {
        return Ok(HostSpec::Packaged {
            binary: PathBuf::from(app),
        });
    }
    let packaged = packaged_binary(&root);
    if is_e2e_capable(&packaged) {
        Ok(HostSpec::Packaged { binary: packaged })
    } else {
        Ok(HostSpec::PreHost { root })
    }
}

/// Where `cargo build -p stratum-desktop --features e2e` leaves the binary.
#[must_use]
pub fn packaged_binary(root: &Path) -> PathBuf {
    let name = if cfg!(windows) {
        "stratum-desktop.exe"
    } else {
        "stratum-desktop"
    };
    root.join("target").join("debug").join(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_packaged_binary_is_unsupported_not_a_panic() {
        let launched = Tier1Driver::launch(HostSpec::Packaged {
            binary: PathBuf::from("/nonexistent/stratum-desktop"),
        });
        // `let ... else` rather than `expect_err`, which would need the driver
        // itself to be Debug — and a Debug that prints a live socket is noise.
        let Err(err) = launched else {
            panic!("there is no such binary; launching it must not succeed")
        };
        assert!(matches!(err, DriverError::Unsupported(_)), "{err}");
    }

    #[test]
    fn a_binary_is_only_a_host_if_it_was_built_with_the_e2e_feature() {
        let dir = std::env::temp_dir().join(format!("stratum-e2e-probe-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a scratch directory");

        let placeholder = dir.join("placeholder");
        std::fs::write(
            &placeholder,
            b"stratum-desktop: window and IPC wiring lands with W17.",
        )
        .expect("write");
        assert!(
            !is_e2e_capable(&placeholder),
            "W17's placeholder must not be mistaken for a host: it exits 64 and never connects"
        );

        let built = dir.join("built");
        std::fs::write(&built, b"\x7fELF...e2e_dispatch...e2e_snapshot...").expect("write");
        assert!(is_e2e_capable(&built));
        assert!(!is_e2e_capable(&dir.join("absent")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_default_host_is_the_pre_host_bridge_until_a_capable_binary_exists() {
        // `STRATUM_E2E_APP` is not set in a plain `cargo test`, and no e2e-built
        // binary exists until W17 lands, so the rule below is the one in force.
        // Asserted as the rule rather than the answer, because the answer
        // depends on what has been built.
        let root = repo_root().expect("repo root");
        match default_host().expect("a host") {
            HostSpec::Packaged { binary } => {
                assert!(is_e2e_capable(&binary) || std::env::var_os("STRATUM_E2E_APP").is_some());
            }
            HostSpec::PreHost { root: r } => {
                assert_eq!(r, root);
                assert!(!is_e2e_capable(&packaged_binary(&root)));
            }
        }
    }

    #[test]
    fn the_request_encoding_is_one_json_object_per_line() {
        let what = What::all();
        let line = serde_json::to_string(&Request::Snapshot { id: 7, what: &what }).unwrap();
        assert!(
            !line.contains('\n'),
            "a newline inside a frame ends the frame"
        );
        assert!(line.starts_with(r#"{"op":"snapshot""#), "{line}");
        assert!(line.contains(r#""id":7"#), "{line}");
    }
}
