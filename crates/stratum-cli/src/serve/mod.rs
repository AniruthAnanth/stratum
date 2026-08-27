//! `stratum serve` — the engine end of both transports (`08` §4.1, CONTRACTS §7).
//!
//! **Not JSON-RPC** (A9): `08` §4.1's `session/open` method namespace is retired
//! and the one envelope is §7.1's. Two encodings, one dispatch:
//!
//! * default — framed MessagePack over stdin/stdout ([`codec`]), the desktop
//!   transport;
//! * `--protocol json` — NDJSON ([`ndjson`]), which is also what `stratum run
//!   --json` writes.
//!
//! This module owns the **plumbing only**. Engine semantics live behind
//! [`EngineBackend`], which `stratum-exec` implements (W08) and the CLI wires
//! up (W09); W07 must not decide what `ExecSubmit` means.

pub mod codec;
pub mod ndjson;

use std::io::{Read, Write};
use std::sync::mpsc;

use stratum_proto::engine::{EngineEvent, EngineRequest, EngineResponse, STREAM_SCHEMA};
use stratum_proto::frame::{Envelope, WireTag};

/// Where a backend puts events. Cloneable and `Send`, because the session worker
/// emits from its own thread while the control thread answers `Status` and
/// `ExecCancel` (ARCHITECTURE §4).
#[derive(Clone)]
pub struct EventSink(mpsc::Sender<EngineEvent>);

impl EventSink {
    pub fn emit(&self, ev: EngineEvent) {
        // A closed channel means the writer thread is gone, i.e. the desktop
        // went away. Dropping the event is right; the process is exiting.
        let _ = self.0.send(ev);
    }
}

/// The engine, as `serve` needs to see it.
pub trait EngineBackend: Send {
    /// Answer one request. Long-running work belongs on the session worker; a
    /// backend that blocks here also blocks `Status` and `ExecCancel`, which is
    /// exactly the C21 failure the cancel ladder exists to survive.
    fn handle(&mut self, req: EngineRequest, events: &EventSink) -> EngineResponse;

    /// Engine identity for the §7 `Hello` handshake.
    fn hello(&self) -> (String, String) {
        (
            format!("stratum {}", env!("CARGO_PKG_VERSION")),
            std::env::consts::ARCH.to_owned(),
        )
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Protocol {
    /// Framed MessagePack (§10). The desktop transport.
    #[default]
    MessagePack,
    /// NDJSON (§7.1). `--protocol json`.
    Json,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ServeOptions {
    pub protocol: Protocol,
    /// Install the parent-death watchdog. On by default: an engine that
    /// outlives the IDE holds a redb lock and a dataset in RAM forever.
    pub watch_parent: bool,
}

/// Run one connection to completion.
///
/// # Errors
/// Framing or i/o failure. A clean EOF, or `EngineRequest::Shutdown`, is `Ok`.
pub fn serve<R, W, B>(
    input: R,
    output: W,
    opts: ServeOptions,
    mut backend: B,
) -> Result<(), codec::CodecError>
where
    R: Read,
    W: Write,
    B: EngineBackend,
{
    if opts.watch_parent {
        orphan::install_parent_death_watchdog();
    }
    let (tx, rx) = mpsc::channel::<EngineEvent>();
    let sink = EventSink(tx);

    match opts.protocol {
        Protocol::MessagePack => {
            let mut sink_out = codec::FrameSink::new(output);
            let mut source = codec::FrameSource::new(input);
            // The writer half runs on this thread between requests; events
            // queued by the backend are drained after each one. W08's session
            // worker gets its own writer thread — this loop is the control
            // thread, and it must never block on user code.
            while let Some(msg) = source.next_message()? {
                match msg {
                    codec::Incoming::Ping(p) if !p.pong => sink_out.pong(0, p.nonce)?,
                    codec::Incoming::Ping(_) => {}
                    codec::Incoming::Request { corr, req } => {
                        let shutdown = matches!(*req, EngineRequest::Shutdown);
                        let resp = dispatch(&mut backend, *req, &sink);
                        sink_out.response(corr, &resp)?;
                        while let Ok(ev) = rx.try_recv() {
                            sink_out.event(&ev)?;
                        }
                        if shutdown {
                            break;
                        }
                    }
                }
            }
        }
        Protocol::Json => {
            let mut out = ndjson::NdjsonWriter::new(output);
            let mut reader = ndjson::NdjsonReader::new(WireTag::Req);
            let mut input = input;
            let mut chunk = vec![0_u8; 64 * 1024];
            'json: loop {
                let n = input.read(&mut chunk)?;
                if n == 0 {
                    reader.end_of_stream()?;
                    break;
                }
                reader.feed(&chunk[..n]);
                while let Some(line) = reader.next_line::<EngineRequest>()? {
                    let ndjson::Line::Ok { corr, body } = line else {
                        // §7.1: skip and continue.
                        continue;
                    };
                    let shutdown = matches!(body, EngineRequest::Shutdown);
                    let resp = dispatch(&mut backend, body, &sink);
                    out.write(&Envelope {
                        v: STREAM_SCHEMA,
                        t: WireTag::Resp,
                        corr,
                        body: resp,
                    })?;
                    while let Ok(ev) = rx.try_recv() {
                        out.event(ev)?;
                    }
                    if shutdown {
                        break 'json;
                    }
                }
            }
        }
    }
    Ok(())
}

/// `Hello` is answered here, not by the backend: the schema check is a property
/// of the protocol, and an engine that let a backend get it wrong would fail
/// version-skew detection.
fn dispatch<B: EngineBackend>(
    backend: &mut B,
    req: EngineRequest,
    events: &EventSink,
) -> EngineResponse {
    match req {
        EngineRequest::Hello { schema, .. } => {
            if schema == STREAM_SCHEMA {
                let (engine, target) = backend.hello();
                EngineResponse::Hello {
                    engine,
                    schema: STREAM_SCHEMA,
                    target,
                }
            } else {
                EngineResponse::Error(stratum_proto::engine::EngineError::SchemaMismatch {
                    engine: STREAM_SCHEMA,
                    client: schema,
                })
            }
        }
        EngineRequest::Shutdown => EngineResponse::Ok,
        other => backend.handle(other, events),
    }
}

/// Orphan prevention — the CHILD half (`08` §5.6, ARCHITECTURE §3).
///
/// The parent half is in the desktop's `engine_host::orphan`: Linux gets
/// `PR_SET_PDEATHSIG` and Windows a Job Object, both set by whoever spawns us.
/// **macOS has neither**, so the only mechanism left is this: poll `getppid()`
/// and exit when the parent we were spawned by is gone.
///
/// `POLL` and `parent_is_gone` are the watchdog's two pieces, so they are gated
/// the way it is — plus `test`, because the arithmetic is platform-independent
/// and its unit test runs on every leg of the matrix. Ungated, they are dead
/// code in the Linux and Windows binaries, which `-D warnings` rejects (and a
/// `#[allow]` would only hide that the non-macOS engine has no watchdog at all).
pub mod orphan {
    #[cfg(any(target_os = "macos", test))]
    use std::time::Duration;

    /// `08` §5.6's cadence. Worst-case orphan lifetime after the IDE dies, and
    /// the number W07's per-OS acceptance test ("the child dies within 1 s")
    /// is budgeted against.
    #[cfg(any(target_os = "macos", test))]
    pub const POLL: Duration = Duration::from_millis(500);

    /// Should a child whose parent was `original` and is now `current` exit?
    ///
    /// Two conditions, not one: on Linux and macOS an orphan is reparented to
    /// pid 1 (or to a reaper such as launchd), but a pid can also be *reused*,
    /// so "my parent id changed at all" is the safe test.
    #[cfg(any(target_os = "macos", test))]
    #[must_use]
    pub fn parent_is_gone(original: u32, current: u32) -> bool {
        current != original || current <= 1
    }

    /// Spawn the watchdog thread. No-op where the parent already guaranteed our
    /// death (Linux `PR_SET_PDEATHSIG`, Windows Job Object): a second mechanism
    /// there would only add a way to exit for the wrong reason.
    #[cfg(target_os = "macos")]
    pub fn install_parent_death_watchdog() {
        // SAFETY: `getppid` takes no arguments and cannot fail.
        let original = unsafe { libc::getppid() } as u32;
        std::thread::Builder::new()
            .name("stratum-parent-watchdog".to_owned())
            .spawn(move || loop {
                std::thread::sleep(POLL);
                // SAFETY: as above.
                let current = unsafe { libc::getppid() } as u32;
                if parent_is_gone(original, current) {
                    // Not a panic and not an unwind: the IDE is gone, there is
                    // nothing left to report a clean shutdown to, and any
                    // in-flight command's output has nowhere to go.
                    std::process::exit(0);
                }
            })
            .expect("the watchdog thread must start; without it macOS leaks engines");
    }

    #[cfg(not(target_os = "macos"))]
    pub fn install_parent_death_watchdog() {}
}

#[cfg(test)]
mod tests {
    use stratum_proto::frame::{FrameKind, FrameReader};
    use stratum_proto::ids::SessionId;

    use super::*;

    struct CountingBackend {
        seen: u32,
    }

    impl EngineBackend for CountingBackend {
        fn handle(&mut self, req: EngineRequest, events: &EventSink) -> EngineResponse {
            self.seen += 1;
            events.emit(EngineEvent::EngineHealth {
                seq: u64::from(self.seen),
                health: stratum_proto::engine::EngineHealth::Ready,
            });
            match req {
                EngineRequest::Status { .. } => EngineResponse::Ok,
                _ => EngineResponse::Ok,
            }
        }

        fn hello(&self) -> (String, String) {
            ("stratum-test".to_owned(), "test".to_owned())
        }
    }

    fn request_frames(reqs: &[EngineRequest]) -> Vec<u8> {
        let mut wire = Vec::new();
        for (i, r) in reqs.iter().enumerate() {
            stratum_proto::frame::encode_frame(
                FrameKind::Request,
                i as u32 + 1,
                &rmp_serde::to_vec_named(r).unwrap(),
                &mut wire,
            )
            .unwrap();
        }
        wire
    }

    #[test]
    fn msgpack_serve_answers_hello_dispatches_and_stops_at_shutdown() {
        let wire = request_frames(&[
            EngineRequest::Hello {
                client: "test".to_owned(),
                schema: STREAM_SCHEMA,
            },
            EngineRequest::Status {
                session: SessionId(1),
            },
            EngineRequest::Shutdown,
            // Never read: `serve` stops at Shutdown.
            EngineRequest::Status {
                session: SessionId(2),
            },
        ]);
        let mut out = Vec::new();
        serve(
            std::io::Cursor::new(wire),
            &mut out,
            ServeOptions::default(),
            CountingBackend { seen: 0 },
        )
        .unwrap();

        let mut rd = FrameReader::new();
        rd.feed(&out);
        let frames: Vec<_> = std::iter::from_fn(|| rd.next_frame().unwrap()).collect();
        rd.end_of_stream().unwrap();
        // Hello response, Status response, the event the backend emitted,
        // Shutdown response.
        assert_eq!(frames[0].kind, FrameKind::Response);
        assert_eq!(frames[0].corr, 1);
        let hello: EngineResponse = rmp_serde::from_slice(&frames[0].payload).unwrap();
        assert!(matches!(hello, EngineResponse::Hello { schema, .. } if schema == STREAM_SCHEMA));
        assert!(frames.iter().any(|f| f.kind == FrameKind::Event));
        assert_eq!(frames.last().unwrap().corr, 3, "Shutdown was answered");
    }

    #[test]
    fn a_client_on_another_schema_is_told_so_rather_than_ignored() {
        let wire = request_frames(&[EngineRequest::Hello {
            client: "future".to_owned(),
            schema: STREAM_SCHEMA + 7,
        }]);
        let mut out = Vec::new();
        serve(
            std::io::Cursor::new(wire),
            &mut out,
            ServeOptions::default(),
            CountingBackend { seen: 0 },
        )
        .unwrap();
        let mut rd = FrameReader::new();
        rd.feed(&out);
        let f = rd.next_frame().unwrap().unwrap();
        let resp: EngineResponse = rmp_serde::from_slice(&f.payload).unwrap();
        assert!(matches!(
            resp,
            EngineResponse::Error(stratum_proto::engine::EngineError::SchemaMismatch { .. })
        ));
    }

    #[test]
    fn json_serve_speaks_exactly_section_7_1() {
        let mut input = Vec::new();
        {
            let mut w = ndjson::NdjsonWriter::new(&mut input);
            w.write(&Envelope::req(
                1,
                EngineRequest::Status {
                    session: SessionId(1),
                },
            ))
            .unwrap();
        }
        // A line from a future schema, between two good ones (§7.1: skip).
        input.extend_from_slice(br#"{"v":1,"t":"req","corr":2,"body":{"req":"telepathy"}}"#);
        input.push(b'\n');
        {
            let mut w = ndjson::NdjsonWriter::new(&mut input);
            w.write(&Envelope::req(3, EngineRequest::Shutdown)).unwrap();
        }

        let mut out = Vec::new();
        serve(
            std::io::Cursor::new(input),
            &mut out,
            ServeOptions {
                protocol: Protocol::Json,
                watch_parent: false,
            },
            CountingBackend { seen: 0 },
        )
        .unwrap();

        let text = String::from_utf8(out).unwrap();
        let lines: Vec<_> = text.lines().collect();
        assert!(
            lines[0].starts_with(r#"{"v":1,"t":"resp","corr":1,"#),
            "{}",
            lines[0]
        );
        assert!(
            lines[1].starts_with(r#"{"v":1,"t":"event","#),
            "{}",
            lines[1]
        );
        assert!(lines[2].contains(r#""corr":3"#), "{}", lines[2]);
        assert_eq!(lines.len(), 3, "the unknown request produced no response");
    }

    #[test]
    fn the_watchdog_fires_on_any_change_of_parent() {
        assert!(!orphan::parent_is_gone(4242, 4242));
        assert!(orphan::parent_is_gone(4242, 1), "reparented to init");
        assert!(
            orphan::parent_is_gone(4242, 99),
            "pid reuse must also count"
        );
        assert_eq!(orphan::POLL, std::time::Duration::from_millis(500));
    }

    // -----------------------------------------------------------------------
    // Orphan prevention, macOS: the kill-the-parent integration test.
    //
    // `parent_is_gone` above is arithmetic; it proves nothing about whether a
    // real engine process actually dies when the IDE does. This does, and it is
    // the macOS third of W07's per-OS acceptance ("kill the parent, assert the
    // child dies within 1 s"). The Linux third is in the desktop's
    // `engine_host::tests`, because `PR_SET_PDEATHSIG` is set by the PARENT.
    // -----------------------------------------------------------------------

    /// When this names a path, the test binary is not a test runner: it is the
    /// engine-shaped CHILD of the fixture below. Re-execing ourselves is the
    /// only way to get the real watchdog running in a real second process
    /// without depending on a `stratum` binary that W09 has not built yet.
    const WATCHDOG_CHILD_ENV: &str = "STRATUM_W07_WATCHDOG_CHILD";

    /// Does a pid exist? Signal 0 runs the permission and existence checks and
    /// delivers nothing. A *zombie* still answers yes, which is why the fixture
    /// gives the child away to `launchd` rather than keeping it reapable here.
    ///
    /// Gated to macOS, not to `unix`, because the watchdog it supports is: on
    /// Linux the parent arms `PR_SET_PDEATHSIG` instead, so `#[cfg(unix)]` here
    /// is dead code under `-D warnings` on the ubuntu leg of the `test` matrix.
    #[cfg(target_os = "macos")]
    fn pid_alive(pid: u32) -> bool {
        // SAFETY: `kill` with signal 0 has no effect beyond returning ESRCH for
        // an unknown pid; `pid` came from a process this test spawned.
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }

    /// Gated with `pid_alive`, its only caller.
    #[cfg(target_os = "macos")]
    fn poll_until<F: FnMut() -> bool>(budget: std::time::Duration, mut done: F) -> bool {
        let deadline = std::time::Instant::now() + budget;
        while std::time::Instant::now() < deadline {
            if done() {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        done()
    }

    /// The child half, running for real. Installs the watchdog, publishes its
    /// pid, then blocks far longer than the test's budget — so if this process
    /// is still alive when the deadline passes, the watchdog did not work.
    #[test]
    fn watchdog_fixture_child_process() {
        let Ok(pidfile) = std::env::var(WATCHDOG_CHILD_ENV) else {
            // The ordinary case: we are a test runner, not the fixture.
            return;
        };
        orphan::install_parent_death_watchdog();
        std::fs::write(&pidfile, std::process::id().to_string()).expect("publish our pid");
        std::thread::sleep(std::time::Duration::from_secs(30));
        // Unreachable if the watchdog fired. 97 is not a libtest exit code, so
        // a stray survivor is identifiable in a process listing.
        std::process::exit(97);
    }

    /// ACCEPTANCE (macOS third): kill the parent, the engine dies within 1 s.
    ///
    /// `/bin/sh` stands in for the IDE. It has to be there: the parent whose
    /// death we are testing gets SIGKILLed, and that cannot be this process.
    /// `cmd & wait` leaves the child running in its own right, so SIGKILLing
    /// the shell reparents it instead of taking it down as collateral — the
    /// child's own `getppid()` poll is then the only thing that can save us,
    /// which is exactly the property under test.
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_getppid_watchdog_kills_the_child_within_one_second_of_the_parent_dying() {
        use std::process::{Command, Stdio};
        use std::time::Duration;

        let dir = std::env::temp_dir().join(format!("stratum-w07-watchdog-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("fixture dir");
        let pidfile = dir.join("child.pid");

        // `--exact` matches libtest's fully-qualified name, which is the module
        // path minus the crate segment. Deriving it beats hard-coding it: these
        // three files move into `crates/stratum-cli/src/serve/` when W09 lands.
        let exe = std::env::current_exe().expect("the test binary knows its own path");
        let module = module_path!()
            .split_once("::")
            .map_or(String::new(), |(_, rest)| format!("{rest}::"));
        let mut parent = Command::new("/bin/sh")
            .arg("-c")
            .arg(r#""$0" "$1" --exact --nocapture & wait"#)
            .arg(&exe)
            .arg(format!("{module}watchdog_fixture_child_process"))
            .env(WATCHDOG_CHILD_ENV, &pidfile)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("/bin/sh exists");

        assert!(
            poll_until(Duration::from_secs(20), || pidfile.exists()),
            "the fixture child never started; is the test binary re-execable?"
        );
        let child_pid: u32 = std::fs::read_to_string(&pidfile)
            .expect("read pid")
            .trim()
            .parse()
            .expect("pid is a number");
        assert!(
            pid_alive(child_pid),
            "the fixture child died before we began"
        );

        // The IDE dies. SIGKILL, not SIGTERM: the whole point is that the
        // parent gets no chance to clean up after itself.
        parent.kill().expect("kill the stand-in parent");
        parent.wait().expect("reap the stand-in parent");

        let died = poll_until(Duration::from_secs(1), || !pid_alive(child_pid));
        if !died {
            // Do not leak a 30 s sleeper into the rest of the suite.
            // SAFETY: `child_pid` is the pid this fixture published.
            unsafe { libc::kill(child_pid as libc::pid_t, libc::SIGKILL) };
        }
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            died,
            "the getppid() watchdog must take the engine down within 1 s of the \
             IDE dying; POLL is {:?}",
            orphan::POLL
        );
    }
}
