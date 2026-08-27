//! The engine supervisor: spawn, health, cancel ladder, orphan prevention,
//! crash-and-respawn (ARCHITECTURE §3 and C21).
//!
//! It supervises either a real `stratum serve` child or the in-process
//! [`crate::mock_engine`], and the caller cannot tell which from the API — that
//! symmetry is the whole point of `--mock`, because a mock reached through a
//! different code path proves nothing about the path that ships.
//!
//! **What is deliberately NOT here.** `ProcessHost::spawn_supervised` (`08` §5.6,
//! W10) is the eventual home of the per-OS process mechanics below. W10 had not
//! landed when W07 was written, so the parent half lives in [`orphan`] with the
//! narrowest surface that works, and the Windows Job Object — which needs the
//! `windows` crate, banned outside `stratum-platform-*` — is the one piece this
//! file cannot supply. See [`orphan::WINDOWS_JOB_OBJECT_NOTE`].

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use stratum_proto::engine::{
    EngineEvent, EngineHealth, EngineRequest, EngineResponse, SessionMode,
};
use stratum_proto::exec::CancelLevel;
use stratum_proto::ids::{ExecutionId, RunId, SessionId};
use stratum_proto::session::SessionConfigWire;
use tokio::sync::{watch, RwLock};
use tokio::task::JoinHandle;

use crate::mock_engine::{self, MockOptions, MockStats};
use crate::transport::{BulkCopyLedger, BulkSegments, Transport, TransportError, TransportTasks};

/// C21, transcribed: `Interrupt` → 2000 ms with no ack → `Abort` → a further
/// 2000 ms → the supervisor kills the engine, respawns it, and offers
/// "Replay to Execution N".
pub const INTERRUPT_TO_ABORT: Duration = Duration::from_millis(2_000);
pub const ABORT_TO_KILL: Duration = Duration::from_millis(2_000);
/// What a *healthy* engine costs to acknowledge an `Interrupt`. Not a timeout —
/// a budget the tests assert, because the felt responsiveness of Stop is the
/// entire user-visible content of C21.
pub const ACK_BUDGET: Duration = Duration::from_millis(50);

/// How long the child gets to exit after its process group is signalled.
pub const REAP_GRACE: Duration = Duration::from_millis(1_000);

#[derive(Clone, Copy, Debug)]
pub struct CancelLadder {
    pub interrupt_to_abort: Duration,
    pub abort_to_kill: Duration,
}

impl Default for CancelLadder {
    fn default() -> Self {
        Self {
            interrupt_to_abort: INTERRUPT_TO_ABORT,
            abort_to_kill: ABORT_TO_KILL,
        }
    }
}

/// The end of one trip up the ladder.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CancelOutcome {
    /// The engine acknowledged at this level.
    Acked { level: CancelLevel },
    /// Nothing acknowledged; the process was killed and respawned. The UI
    /// offers "Replay to Execution N" from the ledger — that N.
    Killed { replay_from: Option<ExecutionId> },
}

#[derive(Debug, thiserror::Error)]
pub enum HostError {
    #[error(transparent)]
    Transport(#[from] TransportError),
    #[error("spawning the engine: {0}")]
    Spawn(#[source] std::io::Error),
    #[error("engine is not running")]
    NotRunning,
    /// The engine ANSWERED, with a well-formed protocol error. The transport,
    /// the framing and the handshake are all proven working — only the request
    /// was refused, and the engine said why.
    ///
    /// This variant exists because its absence cost a debugging session: the
    /// catch-all below used to fold this case into
    /// `TransportError::UnexpectedKind`, whose Display text is "engine answered
    /// a request with an event". So when `stratum serve` answered SessionOpen
    /// honestly — `EngineError::Internal("…stratum-exec…is not linked…")` — the
    /// desktop reported a routing failure that never happened, the setup hook
    /// bubbled it, Tauri panicked on the setup error, and the panic hit tao's
    /// `did_finish_launching`, which cannot unwind, so the user got a SIGABRT
    /// crash report where a one-line message belonged. Never fold a refusal
    /// into a transport failure again.
    #[error("engine refused the request: {0}")]
    Refused(stratum_proto::engine::EngineError),
    /// The engine answered with a response of the wrong TYPE — not an error,
    /// not the expected variant. This is a real protocol bug on one side.
    #[error("engine answered {got} where {expected} was expected")]
    WrongResponse {
        expected: &'static str,
        got: &'static str,
    },
}

/// The `resp` tag a response would carry on the wire — for error messages that
/// name what actually arrived instead of guessing.
fn resp_tag(resp: &EngineResponse) -> &'static str {
    match resp {
        EngineResponse::Hello { .. } => "hello",
        EngineResponse::SessionOpened { .. } => "session_opened",
        EngineResponse::Error(_) => "error",
        _ => "another response variant",
    }
}

/// How this host's engine is embodied.
pub enum EngineSource {
    /// A real `stratum serve --stdio` child.
    Child {
        program: std::path::PathBuf,
        args: Vec<String>,
        cwd: Option<std::path::PathBuf>,
    },
    /// The in-process mock, over a `tokio::io::duplex` pipe.
    Mock(MockOptions),
}

enum Process {
    Child {
        // Boxed because `tokio::process::Child` is 276 bytes on Windows (a
        // HANDLE plus the reaper's state) against 8 for the mock arm, which
        // trips `clippy::large_enum_variant` on that target and nowhere else.
        // Found by cross-checking clippy for x86_64-pc-windows-msvc; the
        // `test` job's windows-2022 leg would have gone red on it.
        child: Box<tokio::process::Child>,
        pid: u32,
    },
    Mock {
        task: JoinHandle<Result<(), TransportError>>,
    },
    Dead,
}

impl Process {
    fn pid(&self) -> Option<u32> {
        match self {
            Self::Child { pid, .. } => Some(*pid),
            _ => None,
        }
    }

    /// Hard kill of the engine **and everything it spawned** — a do-file may
    /// `shell`. On Unix that means the process group, not the pid.
    async fn kill(&mut self) {
        match std::mem::replace(self, Self::Dead) {
            Self::Child { mut child, pid } => {
                // The grace period is only worth spending when something was
                // actually signalled. On Windows nothing is, until W24's Job
                // Object lands, so waiting it out would push C21's kill rung
                // from 4000 ms to 5000 ms on that platform alone.
                if orphan::terminate_tree(pid) {
                    let _ = tokio::time::timeout(REAP_GRACE, child.wait()).await;
                }
                let _ = child.start_kill();
                let _ = child.wait().await;
            }
            Self::Mock { task } => task.abort(),
            Self::Dead => {}
        }
    }
}

/// One supervised engine.
pub struct EngineHost {
    source: EngineSource,
    ladder: CancelLadder,
    transport: RwLock<Transport>,
    process: tokio::sync::Mutex<Process>,
    tasks: tokio::sync::Mutex<TransportTasks>,
    health_tx: watch::Sender<EngineHealth>,
    health_rx: watch::Receiver<EngineHealth>,
    /// Highest `ExecutionId` observed, for C21's "Replay to Execution N".
    last_exec: Arc<AtomicU64>,
    pub bulk: Arc<BulkSegments>,
    pub ledger: Arc<BulkCopyLedger>,
    pub mock_stats: Arc<MockStats>,
}

impl EngineHost {
    /// Spawn the engine described by `source` and bring the transport up.
    pub async fn spawn(source: EngineSource, ladder: CancelLadder) -> Result<Arc<Self>, HostError> {
        let ledger = Arc::new(BulkCopyLedger::default());
        let bulk = Arc::new(BulkSegments::new(Arc::clone(&ledger)));
        let mock_stats = Arc::new(MockStats::default());
        let (health_tx, health_rx) = watch::channel(EngineHealth::Starting);

        let (transport, tasks, process) = Self::connect(&source, &ledger, &mock_stats).await?;

        let host = Arc::new(Self {
            source,
            ladder,
            transport: RwLock::new(transport),
            process: tokio::sync::Mutex::new(process),
            tasks: tokio::sync::Mutex::new(tasks),
            health_tx,
            health_rx,
            last_exec: Arc::new(AtomicU64::new(0)),
            bulk,
            ledger,
            mock_stats,
        });
        host.watch_events();
        Ok(host)
    }

    /// `--mock`: replay `tests/fixtures/mock/scenario_a.msgpack` over the real
    /// transport. The fixture is decoded through the same `FrameReader` the
    /// live path uses, and falls back to the compiled-in script only when the
    /// repo is not on disk (a packaged demo build).
    pub async fn spawn_mock(mut opts: MockOptions) -> Result<Arc<Self>, HostError> {
        if let Some(root) = mock_engine::repo_root() {
            let path = root.join(mock_engine::SCENARIO_A_FIXTURE);
            if let Ok(bytes) = std::fs::read(&path) {
                opts.script = mock_engine::decode_stream(&bytes)?;
            }
        }
        Self::spawn(EngineSource::Mock(opts), CancelLadder::default()).await
    }

    async fn connect(
        source: &EngineSource,
        ledger: &Arc<BulkCopyLedger>,
        mock_stats: &Arc<MockStats>,
    ) -> Result<(Transport, TransportTasks, Process), HostError> {
        match source {
            EngineSource::Child { program, args, cwd } => {
                let mut cmd = tokio::process::Command::new(program);
                cmd.args(args)
                    .stdin(std::process::Stdio::piped())
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::inherit())
                    .kill_on_drop(true);
                if let Some(dir) = cwd {
                    cmd.current_dir(dir);
                }
                orphan::configure(&mut cmd);
                let mut child = cmd.spawn().map_err(HostError::Spawn)?;
                let pid = child.id().unwrap_or_default();
                let stdout = child.stdout.take().expect("piped above");
                let stdin = child.stdin.take().expect("piped above");
                let (transport, tasks) = Transport::spawn(stdout, stdin);
                Ok((
                    transport,
                    tasks,
                    Process::Child {
                        child: Box::new(child),
                        pid,
                    },
                ))
            }
            EngineSource::Mock(opts) => {
                // A real duplex pipe, so the mock's bytes go through the same
                // framing, the same buffering and the same partial reads a pipe
                // to a child would produce.
                let (desktop_side, engine_side) = tokio::io::duplex(64 * 1024);
                let (engine_rx, engine_tx) = tokio::io::split(engine_side);
                let (desktop_rx, desktop_tx) = tokio::io::split(desktop_side);
                let task = tokio::spawn(mock_engine::serve(
                    engine_rx,
                    engine_tx,
                    opts.clone(),
                    Arc::clone(mock_stats),
                    Arc::clone(ledger),
                ));
                let (transport, tasks) = Transport::spawn(desktop_rx, desktop_tx);
                Ok((transport, tasks, Process::Mock { task }))
            }
        }
    }

    pub async fn transport(&self) -> Transport {
        self.transport.read().await.clone()
    }

    #[must_use]
    pub fn health(&self) -> watch::Receiver<EngineHealth> {
        self.health_rx.clone()
    }

    #[must_use]
    pub fn last_exec(&self) -> Option<ExecutionId> {
        match self.last_exec.load(Ordering::Relaxed) {
            0 => None,
            n => Some(ExecutionId(n)),
        }
    }

    /// The §7 `Hello` handshake plus `SessionOpen`, in the order a real client
    /// must use them.
    pub async fn open_session(
        &self,
        project_root: camino::Utf8PathBuf,
        mode: SessionMode,
    ) -> Result<SessionId, HostError> {
        let tx = self.transport().await;
        crate::transport::handshake(&tx, "stratum-desktop").await?;
        let resp = tx
            .request(EngineRequest::SessionOpen {
                project_root,
                mode,
                config: SessionConfigWire {
                    cwd: None,
                    seed: None,
                    linesize: 80,
                    level: 95.0,
                    varabbrev: true,
                    more: false,
                    max_memory_bytes: None,
                    ado_personal: mode == SessionMode::Interactive,
                    write_sandbox: None,
                },
            })
            .await?;
        let _ = self.health_tx.send(EngineHealth::Ready);
        match resp {
            EngineResponse::SessionOpened { session, .. } => Ok(session),
            EngineResponse::Error(e) => Err(HostError::Refused(e)),
            other => Err(HostError::WrongResponse {
                expected: "session_opened",
                got: resp_tag(&other),
            }),
        }
    }

    /// C21's ladder. Returns as soon as a level is acknowledged; escalates on
    /// the clock, never on a reply it may never get.
    pub async fn cancel(self: &Arc<Self>, session: SessionId, run: RunId) -> CancelOutcome {
        for (level, budget) in [
            (CancelLevel::Interrupt, self.ladder.interrupt_to_abort),
            (CancelLevel::Abort, self.ladder.abort_to_kill),
        ] {
            let tx = self.transport().await;
            let req = EngineRequest::ExecCancel {
                session,
                run,
                level,
            };
            if let Ok(Ok(_)) = tokio::time::timeout(budget, tx.request(req)).await {
                return CancelOutcome::Acked { level };
            }
        }
        // Nothing answered in 4 s. Kill, respawn, offer replay — an engine out
        // of process is exactly what makes this recoverable instead of a
        // poisoned session.
        let replay_from = self.last_exec();
        let _ = self.health_tx.send(EngineHealth::Crashed {
            signal: None,
            last_statement: None,
            log_tail: "engine did not acknowledge Abort within 4000 ms".to_owned(),
        });
        let _ = self.restart().await;
        CancelOutcome::Killed { replay_from }
    }

    /// Kill and respawn. Result cards are host state and survive this — that is
    /// the invariant `cargo xtask layering` protects by keeping the desktop off
    /// `stratum-runtime`.
    pub async fn restart(self: &Arc<Self>) -> Result<(), HostError> {
        self.process.lock().await.kill().await;
        {
            let tasks = self.tasks.lock().await;
            tasks.reader.abort();
            tasks.writer.abort();
        }
        let _ = self.health_tx.send(EngineHealth::Starting);
        let (transport, tasks, process) =
            Self::connect(&self.source, &self.ledger, &self.mock_stats).await?;
        *self.transport.write().await = transport;
        *self.tasks.lock().await = tasks;
        *self.process.lock().await = process;
        self.watch_events();
        let _ = self.health_tx.send(EngineHealth::Ready);
        Ok(())
    }

    pub async fn shutdown(&self) {
        let tx = self.transport().await;
        let _ = tokio::time::timeout(REAP_GRACE, tx.request(EngineRequest::Shutdown)).await;
        tx.close();
        self.process.lock().await.kill().await;
        let _ = self.health_tx.send(EngineHealth::Stopped);
    }

    /// Track the highest `ExecutionId` and the engine's own health events.
    fn watch_events(self: &Arc<Self>) {
        let this = Arc::clone(self);
        tokio::spawn(async move {
            let mut rx = this.transport().await.subscribe();
            while let Ok(ev) = rx.recv().await {
                match ev.as_ref() {
                    EngineEvent::BlockStarted { exec, .. }
                    | EngineEvent::BlockFinished { exec, .. } => {
                        this.last_exec.fetch_max(exec.0, Ordering::Relaxed);
                    }
                    EngineEvent::EngineHealth { health, .. } => {
                        let _ = this.health_tx.send(health.clone());
                    }
                    _ => {}
                }
            }
        });
    }

    /// The pid of the supervised child, when there is one. Tests kill it.
    pub async fn child_pid(&self) -> Option<u32> {
        self.process.lock().await.pid()
    }
}

/// Orphan prevention — the PARENT half.
///
/// The child half (macOS's `getppid()` watchdog) cannot live here: it runs
/// inside `stratum serve`, which is `stratum-cli`, and the CLI must not link the
/// desktop. W07 owns `crates/stratum-cli/src/serve/**` and implements it there.
pub mod orphan {
    /// Windows needs a Job Object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`
    /// (`08` §5.6). Creating one needs the `windows` crate, which
    /// ARCHITECTURE §5 allows only inside `stratum-platform-*`. This file
    /// therefore sets `CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW` — which it
    /// can do through `std` alone, and which `CTRL_BREAK_EVENT` requires — and
    /// leaves the Job Object to `ProcessHost::spawn_supervised` (W10/W24).
    /// **Until that lands, a hard-killed Stratum leaks its engine on Windows.**
    ///
    /// This is also why W07's per-OS acceptance ships two of its three
    /// kill-the-parent tests and not three. The macOS one is
    /// `stratum-cli`'s `serve::orphan::tests`; the Linux one is
    /// `linux_pdeathsig_kills_the_engine_within_one_second_of_the_parent_dying`
    /// below. The Windows one has no mechanism to test yet, and writing a
    /// green test against a mechanism that does not exist would be worse than
    /// its absence. **W24 owes it**, together with the Job Object itself.
    pub const WINDOWS_JOB_OBJECT_NOTE: &str =
        "Job Object assignment belongs to stratum-platform-windows (W24)";

    #[cfg(unix)]
    pub fn configure(cmd: &mut tokio::process::Command) {
        use std::os::unix::process::CommandExt;
        // Own process group: a do-file's `shell` children join it, so one
        // `killpg` takes the whole tree (`08` §5.6).
        cmd.as_std_mut().process_group(0);
        #[cfg(target_os = "linux")]
        // SAFETY: `prctl` is async-signal-safe and touches only this (freshly
        // forked, single-threaded) child.
        unsafe {
            cmd.as_std_mut().pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
                Ok(())
            });
        }
    }

    #[cfg(windows)]
    pub fn configure(cmd: &mut tokio::process::Command) {
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    }

    /// Cooperative cancel: `SIGINT` to the group on Unix. Windows needs
    /// `GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT)`, which is W10's.
    #[cfg(unix)]
    pub fn interrupt(pid: u32) {
        // SAFETY: `killpg` with a pid we spawned; a stale pgid returns ESRCH.
        unsafe {
            libc::killpg(pid as libc::pid_t, libc::SIGINT);
        }
    }

    #[cfg(not(unix))]
    pub fn interrupt(_pid: u32) {}

    /// Kill the child and everything it spawned.
    ///
    /// Returns whether a tree kill was actually signalled. The caller uses that
    /// to decide whether waiting out [`super::REAP_GRACE`] can accomplish
    /// anything: where the answer is `false` nothing has been asked to exit, so
    /// the grace period is pure latency on C21's kill rung.
    #[cfg(unix)]
    pub fn terminate_tree(pid: u32) -> bool {
        // SAFETY: as above. The process group id equals the child's pid because
        // `configure` put it in its own group.
        unsafe {
            libc::killpg(pid as libc::pid_t, libc::SIGKILL);
        }
        true
    }

    /// Windows has no tree kill from this crate: see [`WINDOWS_JOB_OBJECT_NOTE`].
    /// The supervisor falls back to `TerminateProcess` on the direct child, so
    /// the engine still dies on the ladder's fourth second — a do-file's
    /// `shell` grandchildren are what leak until W24 lands.
    #[cfg(not(unix))]
    pub fn terminate_tree(_pid: u32) -> bool {
        false
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;
    use std::time::Instant;

    use stratum_proto::data::{PageRequest, RenderMode};
    use stratum_proto::engine::InlineResultsMode;
    use stratum_proto::exec::RunIntent;
    use stratum_proto::ids::{DatasetStateId, VarIdx};

    use super::*;
    use crate::mock_engine::{MockBehaviour, MOCK_DOC, MOCK_SESSION};

    /// C21's numbers are a product decision, not an implementation detail.
    #[test]
    fn the_default_ladder_is_2000_then_4000_milliseconds() {
        let l = CancelLadder::default();
        assert_eq!(l.interrupt_to_abort, Duration::from_millis(2_000));
        assert_eq!(
            l.interrupt_to_abort + l.abort_to_kill,
            Duration::from_millis(4_000),
            "process kill is at 4000 ms measured from the first Interrupt"
        );
    }

    async fn mock_host(behaviour: MockBehaviour, ladder: CancelLadder) -> Arc<EngineHost> {
        let opts = MockOptions {
            behaviour,
            ..MockOptions::default()
        };
        EngineHost::spawn(EngineSource::Mock(opts), ladder)
            .await
            .expect("the mock always spawns")
    }

    /// ACCEPTANCE: `--mock` replays the committed fixture over the real
    /// transport. Nothing here is a stub — the bytes go through
    /// `rmp_serde::to_vec_named`, a §10 frame, a `tokio::io::duplex` pipe and
    /// `FrameReader`, which is exactly the path a real engine's bytes take.
    #[tokio::test]
    async fn mock_replays_scenario_a_over_the_real_transport() {
        let host = EngineHost::spawn_mock(MockOptions::default())
            .await
            .expect("spawn");
        let mut events = host.transport().await.subscribe();
        let session = host
            .open_session(
                camino::Utf8PathBuf::from("/tmp/proj"),
                SessionMode::Interactive,
            )
            .await
            .expect("session opens");
        assert_eq!(session, MOCK_SESSION);

        let tx = host.transport().await;
        let submitted = tx
            .request(EngineRequest::ExecSubmit {
                session,
                intent: RunIntent::CurrentBlock {
                    doc: MOCK_DOC,
                    cursor: 21,
                },
                inline_mode: InlineResultsMode::Always,
            })
            .await
            .expect("submit");
        assert!(matches!(submitted, EngineResponse::Submitted { .. }));

        let want = crate::mock_engine::scenario_a();
        let mut got = Vec::new();
        while got.len() < want.len() {
            let ev = tokio::time::timeout(Duration::from_secs(5), events.recv())
                .await
                .expect("the replay must not stall")
                .expect("the broadcast must not lag at 256 deep");
            got.push(ev.as_ref().clone());
        }
        assert_eq!(got, want, "the replay is the fixture, event for event");

        // And the desktop's own seq bookkeeping saw a clean stream.
        let stats = tx.stats();
        assert_eq!(stats.seq_gaps.load(Ordering::Relaxed), 0);
        assert_eq!(stats.events.load(Ordering::Relaxed) as usize, want.len());
        host.shutdown().await;
    }

    /// ACCEPTANCE: `Interrupt` acks within 50 ms.
    #[tokio::test]
    async fn interrupt_is_acknowledged_inside_the_50ms_budget() {
        let host = mock_host(MockBehaviour::Responsive, CancelLadder::default()).await;
        host.open_session(
            camino::Utf8PathBuf::from("/tmp/proj"),
            SessionMode::Interactive,
        )
        .await
        .unwrap();
        let started = Instant::now();
        let outcome = host.cancel(MOCK_SESSION, RunId(1)).await;
        let elapsed = started.elapsed();
        assert_eq!(
            outcome,
            CancelOutcome::Acked {
                level: CancelLevel::Interrupt
            }
        );
        assert!(
            elapsed < ACK_BUDGET,
            "Interrupt acked in {elapsed:?}, budget {ACK_BUDGET:?}"
        );
        assert_eq!(host.mock_stats.cancels.load(Ordering::Relaxed), 1);
        host.shutdown().await;
    }

    /// ACCEPTANCE: a deliberately-uninterruptible engine escalates to `Abort`
    /// and then to a process kill, and the supervisor respawns and can offer a
    /// replay.
    ///
    /// The ladder is scaled down 40× so the test costs 100 ms rather than 4 s;
    /// the shipped numbers are asserted separately, above.
    #[tokio::test]
    async fn an_uninterruptible_engine_escalates_to_abort_then_kill_and_respawns() {
        let ladder = CancelLadder {
            interrupt_to_abort: Duration::from_millis(50),
            abort_to_kill: Duration::from_millis(50),
        };
        let host = mock_host(MockBehaviour::Uninterruptible, ladder).await;
        host.open_session(
            camino::Utf8PathBuf::from("/tmp/proj"),
            SessionMode::Interactive,
        )
        .await
        .unwrap();
        let tx = host.transport().await;
        let mut events = tx.subscribe();
        tx.request(EngineRequest::ExecSubmit {
            session: MOCK_SESSION,
            intent: RunIntent::CurrentBlock {
                doc: MOCK_DOC,
                cursor: 21,
            },
            inline_mode: InlineResultsMode::Always,
        })
        .await
        .unwrap();
        // Let a couple of executions be observed so there is something to
        // replay to.
        for _ in 0..8 {
            let _ = tokio::time::timeout(Duration::from_secs(2), events.recv()).await;
        }

        let started = Instant::now();
        let outcome = host.cancel(MOCK_SESSION, RunId(1)).await;
        let elapsed = started.elapsed();
        match outcome {
            CancelOutcome::Killed { replay_from } => {
                assert!(replay_from.is_some(), "nothing to replay to");
            }
            other => panic!("an engine that never acks must be killed, got {other:?}"),
        }
        assert!(
            elapsed >= Duration::from_millis(100),
            "the ladder must wait out both rungs, waited {elapsed:?}"
        );
        assert_eq!(
            host.mock_stats.cancels.load(Ordering::Relaxed),
            2,
            "both Interrupt and Abort must have been sent"
        );

        // Respawned: the fresh engine answers a handshake on a NEW transport.
        let fresh = host.transport().await;
        crate::transport::handshake(&fresh, "stratum-desktop")
            .await
            .expect("the supervisor respawned a working engine");
        host.shutdown().await;
    }

    /// ACCEPTANCE: a 64 MB `DataPage` moves engine → mmap → webview with
    /// exactly two copies, asserted by instrumentation.
    #[tokio::test]
    async fn a_64mb_datapage_costs_exactly_two_copies() {
        let dir = tempfile::tempdir().unwrap();
        let opts = MockOptions {
            bulk_dir: Some(dir.path().to_path_buf()),
            ..MockOptions::default()
        };
        let host = EngineHost::spawn(EngineSource::Mock(opts), CancelLadder::default())
            .await
            .unwrap();

        // 9 bytes per row per column (f64 + missing tag, §8.1), one column.
        const TARGET: u64 = 64 * 1024 * 1024;
        let nrows = u32::try_from(TARGET / 9).unwrap();
        let resp = host
            .transport()
            .await
            .request(EngineRequest::DataPage {
                session: MOCK_SESSION,
                request: PageRequest {
                    frame: "default".to_owned(),
                    state: DatasetStateId(17),
                    row0: 0,
                    nrows,
                    cols: vec![VarIdx(0)],
                    order: None,
                    render: RenderMode::Edit,
                    seq: 1,
                },
            })
            .await
            .unwrap();
        let EngineResponse::Bulk { bulk } = resp else {
            panic!("a DataPage is answered with Bulk, never inline bytes");
        };
        assert!(bulk.len >= TARGET, "{} bytes is not 64 MB", bulk.len);

        // The desktop maps the segment read-only and serves the bytes.
        let path = BulkSegments::segment_path(dir.path(), MOCK_SESSION.0, bulk.segment);
        host.bulk.attach(bulk.segment, &path, bulk.epoch).unwrap();
        let slice = host.bulk.resolve(&bulk).unwrap();
        assert_eq!(slice.as_bytes().len() as u64, bulk.len);
        assert_eq!(&slice.as_bytes()[..4], b"SDP1");
        let body = slice.into_response_body();
        assert_eq!(body.len() as u64, bulk.len);

        assert_eq!(
            host.ledger.engine_to_mmap.load(Ordering::Relaxed),
            1,
            "the engine built the page straight into the mapping"
        );
        assert_eq!(
            host.ledger.mmap_to_response.load(Ordering::Relaxed),
            1,
            "the webview response body is the only other copy"
        );
        assert_eq!(host.ledger.total_copies(), 2, "§10's whole budget");
        host.shutdown().await;
    }

    /// A stale `epoch` must not resolve: a retired segment's bytes belong to a
    /// different page, and serving them is the worst class of bug this
    /// transport can have.
    #[tokio::test]
    async fn a_stale_bulk_ref_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Arc::new(BulkCopyLedger::default());
        let segs = BulkSegments::new(Arc::clone(&ledger));
        let path = BulkSegments::segment_path(dir.path(), 1, 0);
        std::fs::write(&path, vec![7_u8; 4096]).unwrap();
        segs.attach(0, &path, 1).unwrap();

        assert!(segs
            .resolve(&stratum_proto::engine::BulkRef {
                segment: 0,
                offset: 0,
                len: 16,
                epoch: 2,
            })
            .is_err());
        assert!(segs
            .resolve(&stratum_proto::engine::BulkRef {
                segment: 0,
                offset: 4_090,
                len: 16,
                epoch: 1,
            })
            .is_err());
        assert_eq!(ledger.total_copies(), 0);
    }

    // -----------------------------------------------------------------------
    // Orphan prevention, the PARENT half, as a real kill-the-parent test.
    //
    // W07's acceptance names three: macOS `getppid()`, Linux `PR_SET_PDEATHSIG`,
    // Windows Job Object. The macOS one is in `stratum-cli`'s `serve::orphan`
    // tests, because on macOS the mechanism lives in the CHILD. The Linux one is
    // here, because `PR_SET_PDEATHSIG` is set by whoever spawns the engine. The
    // Windows one cannot be written from this crate at all — see
    // `orphan::WINDOWS_JOB_OBJECT_NOTE`.
    //
    // Both need a parent that is not this process, because the parent gets
    // SIGKILLed. The fixture therefore re-execs the test binary as a MID
    // process that spawns one engine-shaped grandchild through the same
    // `orphan::configure` the supervisor uses.
    // -----------------------------------------------------------------------

    /// When this names a path, the test binary is the fixture's MID process.
    #[cfg(unix)]
    const ORPHAN_MID_ENV: &str = "STRATUM_W07_ORPHAN_MID";

    /// Signal 0 runs the existence check and delivers nothing. A zombie answers
    /// yes, which is why every use below either kills the mid first (so the
    /// grandchild is reparented to init and reaped there) or lets the mid reap.
    #[cfg(unix)]
    fn pid_alive(pid: u32) -> bool {
        // SAFETY: signal 0 has no effect; `pid` came from a process we spawned.
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }

    #[cfg(unix)]
    async fn poll_until<F: FnMut() -> bool>(budget: Duration, mut done: F) -> bool {
        let deadline = Instant::now() + budget;
        while Instant::now() < deadline {
            if done() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        done()
    }

    /// The MID process. Spawns one grandchild exactly the way `EngineHost`
    /// spawns an engine, publishes its pid, then blocks on it — blocking on
    /// `wait` is what reaps the grandchild promptly, so `pid_alive` observes a
    /// death rather than a zombie.
    #[cfg(unix)]
    #[tokio::test]
    async fn orphan_fixture_mid_process() {
        let Ok(pidfile) = std::env::var(ORPHAN_MID_ENV) else {
            // The ordinary case: we are a test runner, not the fixture.
            return;
        };
        let mut cmd = tokio::process::Command::new("/bin/sh");
        // `exec` so the pid we publish is the sleeper itself, not a shell that
        // owns it — the per-OS mechanisms act on the process they were set on.
        cmd.arg("-c")
            .arg("exec sleep 30")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        orphan::configure(&mut cmd);
        let mut child = cmd.spawn().expect("/bin/sh exists");
        std::fs::write(&pidfile, child.id().expect("just spawned").to_string())
            .expect("publish the grandchild pid");
        let _ = tokio::time::timeout(Duration::from_secs(30), child.wait()).await;
        // Unreachable if the outer test killed us. 97 is not a libtest exit
        // code, so a stray survivor is identifiable in a process listing.
        std::process::exit(97);
    }

    /// Start the fixture and return the mid process together with the pid of
    /// the engine-shaped grandchild it spawned.
    #[cfg(unix)]
    async fn spawn_orphan_fixture(pidfile: &std::path::Path) -> (std::process::Child, u32) {
        // `--exact` matches libtest's fully-qualified name, which is the module
        // path minus the crate segment. Derived, not hard-coded: this file moves
        // when W17 creates the crate around it.
        let module = module_path!()
            .split_once("::")
            .map_or(String::new(), |(_, rest)| format!("{rest}::"));
        let exe = std::env::current_exe().expect("the test binary knows its own path");
        let mid = std::process::Command::new(exe)
            .arg(format!("{module}orphan_fixture_mid_process"))
            .arg("--exact")
            .arg("--nocapture")
            // Run the fixture on the process's main thread: on Linux
            // PR_SET_PDEATHSIG is armed against the *thread* that forked, and a
            // libtest worker thread can outlive nothing useful.
            .arg("--test-threads=1")
            .env(ORPHAN_MID_ENV, pidfile)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("re-exec the test binary");
        assert!(
            poll_until(Duration::from_secs(20), || pidfile.exists()).await,
            "the fixture mid process never published a grandchild pid"
        );
        let pid: u32 = std::fs::read_to_string(pidfile)
            .expect("read pid")
            .trim()
            .parse()
            .expect("pid is a number");
        (mid, pid)
    }

    /// ACCEPTANCE (Linux third): kill the parent, the engine dies within 1 s.
    /// Nothing but `PR_SET_PDEATHSIG` can do it here — `orphan::configure` put
    /// the grandchild in its own process group, so it is not collateral of any
    /// signal aimed at the mid process.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn linux_pdeathsig_kills_the_engine_within_one_second_of_the_parent_dying() {
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("engine.pid");
        let (mut mid, engine) = spawn_orphan_fixture(&pidfile).await;
        assert!(pid_alive(engine), "the fixture engine died before we began");

        mid.kill().expect("kill the stand-in supervisor");
        mid.wait().expect("reap the stand-in supervisor");

        let died = poll_until(Duration::from_secs(1), || !pid_alive(engine)).await;
        if !died {
            let _ = orphan::terminate_tree(engine);
        }
        assert!(
            died,
            "PR_SET_PDEATHSIG must take the engine down within 1 s of the \
             supervisor dying"
        );
    }

    /// The same fixture on macOS, where the parent side has NO death mechanism:
    /// the engine outlives its supervisor, and `killpg` from a live supervisor
    /// is the only parent-side remedy. This is the whole reason
    /// `stratum-cli`'s `serve::orphan` getppid watchdog exists; if this test
    /// ever starts failing, a parent-side mechanism appeared on macOS and that
    /// story needs rewriting rather than the test relaxing.
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn macos_has_no_parent_side_death_mechanism_so_killpg_is_the_remedy() {
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("engine.pid");
        let (mut mid, engine) = spawn_orphan_fixture(&pidfile).await;
        assert!(pid_alive(engine), "the fixture engine died before we began");

        mid.kill().expect("kill the stand-in supervisor");
        mid.wait().expect("reap the stand-in supervisor");

        assert!(
            !poll_until(Duration::from_secs(1), || !pid_alive(engine)).await,
            "macOS grew a parent-side death mechanism; see serve::orphan"
        );
        // And the supervisor's own remedy still takes the whole group.
        assert!(
            orphan::terminate_tree(engine),
            "unix always signals the group"
        );
        assert!(
            poll_until(Duration::from_secs(1), || !pid_alive(engine)).await,
            "killpg must reap a reparented engine"
        );
    }

    /// A TRIPWIRE, not the Windows third of the per-OS acceptance.
    ///
    /// It asserts the one Windows-side property this crate can honestly assert:
    /// that it reports having no tree kill, which is what keeps C21's kill rung
    /// at 4000 ms there instead of 5000 ms. It deliberately does not claim that
    /// killing the supervisor kills the engine — on Windows it does not, and
    /// nothing in this crate can make it, because the Job Object needs the
    /// `windows` crate that `deny.toml` confines to `stratum-platform-windows`.
    /// When W24 lands that crate this assertion flips and must be replaced by
    /// the real kill-the-parent test.
    #[cfg(windows)]
    #[test]
    fn windows_reports_no_tree_kill_so_the_ladder_does_not_burn_the_reap_grace() {
        // Any pid: the `#[cfg(not(unix))]` arm signals nothing either way, and
        // the report is the whole subject.
        assert!(
            !orphan::terminate_tree(u32::MAX),
            "{}",
            orphan::WINDOWS_JOB_OBJECT_NOTE
        );
    }

    /// Orphan prevention, parent half: the engine gets its own process group,
    /// so one `killpg` takes the engine *and* anything a do-file `shell`ed.
    #[cfg(unix)]
    #[tokio::test]
    async fn terminate_tree_kills_the_engines_children_too() {
        let marker = format!("stratum-w07-orphan-{}", std::process::id());
        let mut cmd = tokio::process::Command::new("/bin/sh");
        cmd.arg("-c")
            .arg(format!("sleep 30 & sleep 30 # {marker}"))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .kill_on_drop(true);
        orphan::configure(&mut cmd);
        let mut child = cmd.spawn().expect("/bin/sh exists");
        let pid = child.id().expect("just spawned");
        tokio::time::sleep(Duration::from_millis(200)).await;

        let alive = |m: &str| {
            std::process::Command::new("/usr/bin/pgrep")
                .arg("-f")
                .arg(m)
                .output()
                .map(|o| !o.stdout.is_empty())
                .unwrap_or(false)
        };
        assert!(alive(&marker), "the fixture process did not start");

        assert!(orphan::terminate_tree(pid), "unix always signals the group");
        let deadline = Instant::now() + Duration::from_secs(1);
        while alive(&marker) && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            !alive(&marker),
            "killpg must take the whole tree within 1 s"
        );
        let _ = child.start_kill();
    }
}
