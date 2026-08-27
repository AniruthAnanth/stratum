//! `stratum-desktop` — the Tauri v2 host process (plan W17).
//!
//! One process: window creation (spec §26), the CONTRACTS §11 command surface
//! (`ipc.rs`), native menus through W10's platform traits (`menu.rs`), the
//! `stratum-asset://` protocol (`asset.rs` grammar + `ipc.rs` glue, CONTRACTS
//! §10 / ADR-007), the seq-ordered event fan-out (`windows.rs`), the 5 s
//! heartbeat (`heartbeat.rs`) — and engine supervision through W07's
//! `engine_host`: a real `stratum serve --stdio` child when one is built,
//! the in-process mock otherwise, and the frontend cannot tell which.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex};

use tauri::Manager;

mod asset;
mod engine_host;
mod file_open;
mod heartbeat;
mod ipc;
mod menu;
mod mock_engine;
mod transport;
mod windows;

// ADR-011: declared UNCONDITIONALLY — the ~450 non-Tauri lines and their tests
// must be compiled by every ordinary build. The Tauri surface inside it is
// gated on `--features e2e`; `xtask e2e --check-fence` asserts a build without
// the feature contains neither fenced command name. The allow is W25's own
// prescription (see the header of e2e_cmds.rs): in a build without the
// feature, the module's constants and `Control` are reachable from nothing,
// and that is the fence working, not dead weight to delete.
#[allow(dead_code)]
mod e2e_cmds;

use engine_host::{CancelLadder, EngineHost, EngineSource};
use ipc::HostState;
use windows::SessionRegistry;

// 08 §10.2: mimalloc in exactly two binaries, never a library. This is one.
#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// `--selftest` / `STRATUM_E2E`: whether windows are created hidden. A host
/// that opens a real window during a headless CI run is a host that hangs.
static HEADLESS: AtomicBool = AtomicBool::new(false);

pub(crate) fn headless() -> bool {
    HEADLESS.load(Ordering::Relaxed)
}

/// sysexits(3) `EX_SOFTWARE`: the status this host leaves with when it refuses
/// to start — the engine would not spawn, the main window would not build, or
/// (`--selftest`) the engine would not answer. See [`refuse_to_start`].
const EX_SOFTWARE: i32 = 70;

/// `--selftest`, engine half: how long the engine gets to answer the §7
/// handshake and `SessionOpen`. This is the only startup step that WAITS on
/// the engine — spawning it is a synchronous fork/exec that either succeeds or
/// fails at once — and so the only one that needs a clock. Without it a
/// sidecar that was alive but silent (hung, or not a Stratum engine at all)
/// held the launch open forever; the packaging verifier measured exactly that.
const ENGINE_HANDSHAKE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(15);

/// `--selftest`, webview half: how long the bundled frontend gets to boot and
/// invoke `app_ready`. `xtask smoke selftest`'s own hang guard (120 s) is
/// deliberately twice this, so the binary's verdict always arrives first.
const SELFTEST_WEBVIEW_DEADLINE: std::time::Duration = std::time::Duration::from_secs(60);

/// The process status of a `--selftest` whose webview never called
/// `app_ready` — "the bundle is fine but the frontend cannot load", the one
/// packaging failure nothing else in the repository catches. Distinct from
/// [`EX_SOFTWARE`], which the engine half uses, so a log line is not needed
/// to tell the two halves apart.
const SELFTEST_WEBVIEW_FAILED: i32 = 1;

/// The setup hook's one way out when the host cannot start.
///
/// Tauri turns an `Err` from the setup hook into a panic, and on macOS that
/// panic is raised inside tao's `did_finish_launching` — an Objective-C
/// callback that cannot unwind — so it becomes `abort()`: a SIGABRT and a
/// native crash report for what is an ordinary runtime condition. Measured
/// twice: first when `stratum serve` honestly refused `SessionOpen` (now
/// `HostError::Refused`, and non-fatal), then by the packaging verifier with a
/// deliberately broken sidecar — one that would not execute, one that exited
/// at once. So the hook never returns `Err`: every failure comes here, says
/// exactly what happened on stderr, and leaves through `process::exit`, which
/// does not unwind. A user with a broken engine gets a message and a clean
/// exit; a harness gets [`EX_SOFTWARE`] to assert on; nobody gets a crash
/// report.
///
/// `process::exit` runs no destructors — `kill_on_drop` on the engine child
/// included — so callers that have an engine up take it down first.
fn refuse_to_start(why: &str) -> ! {
    eprintln!("stratum-desktop: cannot start: {why}");
    std::process::exit(EX_SOFTWARE)
}

/// What a failed spawn means, in words a user can act on.
fn spawn_failure(e: &engine_host::HostError) -> String {
    let hint = match e {
        engine_host::HostError::NotRunning => "the engine exited during startup",
        engine_host::HostError::Spawn(_) => "the engine binary would not start",
        engine_host::HostError::Transport(_) => "the engine transport failed during startup",
        // Cannot occur from a spawn — no request has been made yet — but the
        // compiler is right that the host can now say it, and a wrong hint is
        // worse than a spare arm.
        engine_host::HostError::Refused(_) | engine_host::HostError::WrongResponse { .. } => {
            "the engine refused the startup handshake"
        }
    };
    format!(
        "{hint}: {e}\n\
         stratum-desktop: the `stratum` engine next to this executable (or on PATH, or named \
         by STRATUM_ENGINE) could not be used. Reinstall, or run with --mock to use the \
         built-in replay engine."
    )
}

/// A built `stratum` engine binary, when one exists (the plan's bullet: the
/// REAL `stratum serve` when built, `--mock` otherwise; the frontend cannot
/// tell). `None` means the mock.
fn engine_binary(force_mock: bool) -> Option<std::path::PathBuf> {
    if force_mock {
        return None;
    }
    if let Ok(path) = std::env::var("STRATUM_ENGINE") {
        return Some(path.into());
    }
    // A `stratum` binary next to this executable (the packaged layout), then
    // PATH.
    let name = if cfg!(windows) {
        "stratum.exe"
    } else {
        "stratum"
    };
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join(name));
        }
    }
    if let Some(paths) = std::env::var_os("PATH") {
        candidates.extend(std::env::split_paths(&paths).map(|p| p.join(name)));
    }
    let found = candidates.into_iter().find(|c| c.is_file());
    if found.is_none() {
        eprintln!("stratum-desktop: no `stratum` engine binary found; using the mock engine");
    }
    found
}

/// The engine resolves `sysuse` under `$STRATUM_ADO_BASE`, falling back to an
/// `ado/base` tree next to its own executable. In a bundle the tree rides as a
/// Tauri resource (`bundle.resources`, staged by `cargo xtask dist stage`) —
/// which is NOT executable-adjacent on macOS (Contents/Resources vs
/// Contents/MacOS) — so the host names the real location in the child's
/// environment. Set on this process (inherited by the child, and by every
/// crash-respawn of it) because `EngineSource::Child` carries no env of its
/// own. An explicit `STRATUM_ADO_BASE` from the user wins and is never
/// overwritten; a missing tree (a dev run outside a bundle) exports nothing
/// and leaves the engine's own fallbacks in charge.
fn export_ado_base(resource_dir: Option<std::path::PathBuf>) {
    if std::env::var_os("STRATUM_ADO_BASE").is_some() {
        return;
    }
    let Some(base) = resource_dir.map(|d| d.join("ado").join("base")) else {
        return;
    };
    if base.is_dir() {
        std::env::set_var("STRATUM_ADO_BASE", &base);
    }
}

/// Spawn the supervised engine: a real child or W07's mock, one API.
/// `resource_dir` is the bundle's resolved resource directory, for the child's
/// `STRATUM_ADO_BASE` (see [`export_ado_base`]).
async fn spawn_engine(
    force_mock: bool,
    resource_dir: Option<std::path::PathBuf>,
) -> Result<Arc<EngineHost>, engine_host::HostError> {
    match engine_binary(force_mock) {
        Some(program) => {
            export_ado_base(resource_dir);
            EngineHost::spawn(
                EngineSource::Child {
                    program,
                    args: vec!["serve".into(), "--stdio".into()],
                    cwd: None,
                },
                CancelLadder::default(),
            )
            .await
        }
        // `spawn_mock` replays the committed fixture through the real framing
        // (falling back to the compiled-in script outside the repo).
        None => EngineHost::spawn_mock(mock_engine::MockOptions::default()).await,
    }
}

fn config_dir() -> std::path::PathBuf {
    // The platform layer resolves the real per-OS directories; `ui` is this
    // host's slice of them.
    stratum_platform_host::host()
        .paths()
        .config_dir()
        .join("ui")
        .into_std_path_buf()
}

/// Everything the builder registers, with room for the fenced e2e commands.
/// One list, so the two feature configurations cannot drift apart.
macro_rules! stratum_handler {
    ($($extra:path),* $(,)?) => {
        tauri::generate_handler![
            ipc::session_open,
            ipc::session_subscribe,
            ipc::session_close,
            ipc::session_status,
            ipc::doc_open,
            ipc::doc_change,
            ipc::doc_save,
            ipc::doc_close,
            ipc::doc_claim,
            ipc::exec_submit,
            ipc::exec_cancel,
            ipc::blocks_get,
            ipc::statuses_get,
            ipc::ledger_get,
            ipc::result_get,
            ipc::variables_list,
            ipc::variable_stats,
            ipc::frames_list,
            ipc::data_order_set,
            ipc::data_order_drop,
            ipc::graph_render,
            ipc::log_range,
            ipc::log_copy,
            ipc::log_search,
            ipc::repro_report,
            ipc::defuse_index,
            ipc::completion_env,
            ipc::workspace_load,
            ipc::workspace_save,
            ipc::layout_load,
            ipc::layout_save,
            ipc::layout_reset,
            ipc::keymap_load,
            ipc::keymap_save,
            ipc::sidecar_get,
            ipc::sidecar_patch,
            ipc::menu_accelerator,
            ipc::window_open_pane,
            ipc::window_close,
            ipc::platform_open_external,
            ipc::platform_reveal,
            ipc::credentials_backend,
            ipc::bench_report,
            ipc::app_ready,
            $($extra),*
        ]
    };
}

/// Everything the setup hook does once the engine is up: the e2e control
/// surface, the selftest's engine half, the event pump and heartbeat, the
/// host state, menus, the main window, the selftest's webview watchdog.
///
/// Returns `Err` with the reason instead of exiting, so the hook — the one
/// place that knows a returned error is a crash report (see
/// [`refuse_to_start`]) — can take the engine down and leave cleanly. Every
/// `?` in here lands in that `String`; none of them can reach Tauri.
fn bring_up(
    app: &mut tauri::App,
    engine: Arc<EngineHost>,
    registry: Arc<SessionRegistry>,
    ready: (std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>),
    selftest: bool,
) -> Result<(), String> {
    let (ready_tx, ready_rx) = ready;

    // The e2e control surface (ADR-011): the Control that correlates
    // harness requests with webview replies, managed HERE because this
    // is the builder's one setup hook (see the note in `main`). The socket
    // thread itself starts in `ipc::app_ready`, once the webview's
    // listener exists.
    #[cfg(feature = "e2e")]
    {
        use tauri::Emitter;

        struct HostWebview {
            app: tauri::AppHandle,
        }
        impl e2e_cmds::Webview for HostWebview {
            fn ask(
                &self,
                id: u32,
                op: &str,
                payload: &serde_json::Value,
            ) -> Result<(), e2e_cmds::E2eError> {
                self.app
                    .emit(
                        e2e_cmds::REQUEST_EVENT,
                        serde_json::json!({ "id": id, "op": op, "payload": payload }),
                    )
                    .map_err(|e| e2e_cmds::E2eError::Transport(e.to_string()))
            }
        }

        let control = Arc::new(e2e_cmds::Control::new(Arc::new(HostWebview {
            app: app.handle().clone(),
        })));
        app.handle().manage(control.pending());
        app.handle().manage(control);
    }

    if selftest {
        // Half one of the selftest: the engine path itself — spawn, §7
        // handshake, SessionOpen — before the webview half below. This is
        // the one startup step that waits on the engine, so it is the one
        // under a clock: a sidecar that exited at once ("engine closed the
        // connection") and one that was alive but silent both used to end
        // here, the first as a crash report, the second as a launch that
        // never returned.
        //
        // A SessionOpen the engine REFUSES is reported, not fatal: at HEAD
        // `stratum serve` answers it with "the execution engine
        // (crates/stratum-exec, work unit W08) is not linked into this
        // build" (W09's declared ordering blocker, crates/stratum-cli/
        // src/cmd/mod.rs), and a selftest that exits 1 on another unit's
        // known gap teaches people to skip the selftest. The spawn and the
        // §7 handshake DID run — a broken pipe or a schema mismatch still
        // fails here.
        //
        // The timeout is built INSIDE the async block: `tokio::time::timeout`
        // arms its timer at construction and panics when that happens outside
        // a runtime context, which is exactly where the argument to `block_on`
        // is evaluated.
        let opened = tauri::async_runtime::block_on(async {
            tokio::time::timeout(
                ENGINE_HANDSHAKE_DEADLINE,
                engine.open_session(
                    camino::Utf8PathBuf::from("."),
                    stratum_proto::engine::SessionMode::Interactive,
                ),
            )
            .await
        });
        match opened {
            Ok(Ok(_)) => {}
            // The engine ANSWERED, refusing the open and saying why. The
            // transport and the handshake are proven; only the backend is
            // missing. Print the engine's own reason — the previous shape of
            // this guard matched a mislabeled transport error and had to
            // explain itself in a comment instead of a message.
            Ok(Err(engine_host::HostError::Refused(e))) => {
                eprintln!(
                    "stratum-desktop: selftest: engine refused SessionOpen \
                     ({e}); continuing with the webview half"
                );
            }
            Ok(Err(e)) => {
                return Err(format!("selftest FAILED — engine handshake: {e}"));
            }
            Err(_elapsed) => {
                return Err(format!(
                    "selftest FAILED — the engine did not answer the §7 handshake within \
                     {} s\n\
                     stratum-desktop: the `stratum` engine process started but never spoke. \
                     It may be hung, or may not be a Stratum engine at all.",
                    ENGINE_HANDSHAKE_DEADLINE.as_secs()
                ));
            }
        }
    }

    ipc::spawn_event_pump(Arc::clone(&engine), Arc::clone(&registry));
    let hb = Arc::new(heartbeat::Heartbeat::new(
        Arc::clone(&engine),
        Arc::clone(&registry),
    ));
    hb.spawn();

    let mut state = HostState::new(engine, registry, config_dir());
    state.ready_tx = Mutex::new(Some(ready_tx));
    app.handle().manage(state);

    // Menus are policy from W10 + toolkit from Tauri. Non-fatal: a menu that
    // failed to build must not take the window down with it.
    if let Err(e) = menu::install(app.handle(), Default::default()) {
        eprintln!("stratum-desktop: menu install failed: {e}");
    }

    // The main window. `label=main` matches the bridge's default. A webview
    // that cannot be created at all (no WebKit, a broken WebView2) is the
    // same class of failure as a broken engine: a message and a clean exit,
    // not a crash report.
    tauri::WebviewWindowBuilder::new(app, "main", tauri::WebviewUrl::App("index.html".into()))
        .title("Stratum")
        .inner_size(1280.0, 800.0)
        .visible(!headless())
        .build()
        .map_err(|e| format!("the main window could not be created: {e}"))?;

    if selftest {
        // The only honest way to catch "the bundle is fine but the
        // webview cannot load": wait for the webview to call app_ready,
        // then exit 0. A blocking recv with a deadline — no polling.
        //
        // The two arms leave differently, on purpose.
        //
        // OK goes through `AppHandle::exit(0)`: the event loop drains
        // and `RunEvent::Exit` below says goodbye to the engine. (Any
        // non-zero code sent this way reaches the process status only
        // through `requested_exit_code` in the run loop below —
        // `AppHandle::exit(1)` on its own does NOT, and a failed
        // selftest once exited 0 because of it.)
        //
        // FAILED is a hard `process::exit`: a webview that never
        // loaded may have wedged the main thread itself, and an exit
        // request that has to travel through that event loop could be
        // the one thing that never arrives — the harness would then
        // see a hang, not the verdict already printed. Nothing is
        // orphaned by it: `stratum serve` exits on stdin EOF, and on
        // macOS also on its getppid() watchdog (08 §5.6).
        let handle = app.handle().clone();
        std::thread::spawn(
            move || match ready_rx.recv_timeout(SELFTEST_WEBVIEW_DEADLINE) {
                Ok(()) => {
                    eprintln!("stratum-desktop: selftest OK (app_ready received)");
                    handle.exit(0);
                }
                Err(_) => {
                    eprintln!(
                        "stratum-desktop: selftest FAILED — the webview never \
                         invoked app_ready within {} s",
                        SELFTEST_WEBVIEW_DEADLINE.as_secs()
                    );
                    std::process::exit(SELFTEST_WEBVIEW_FAILED);
                }
            },
        );
    }

    Ok(())
}

fn main() {
    // NON-OPTIONAL (W17 acceptance): before any GTK init on Linux. WebKitGTK's
    // DMA-BUF renderer wedges on common driver stacks; the env var must be in
    // the environment before the first GTK call, which means first in main().
    #[cfg(target_os = "linux")]
    std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");

    let args: Vec<String> = std::env::args().collect();
    let force_mock = args.iter().any(|a| a == "--mock");
    let selftest = args.iter().any(|a| a == "--selftest");
    let e2e_run = std::env::var_os("STRATUM_E2E").is_some();
    HEADLESS.store(selftest || e2e_run, Ordering::Relaxed);

    // Fixture regenerator: `--write-mock-fixture <path>` re-encodes the canned
    // Scenario A stream through the real framing and exits. This is how
    // `tests/fixtures/mock/scenario_a.msgpack` (W07's committed fixture) is
    // reproduced when the script changes.
    if let Some(pos) = args.iter().position(|a| a == "--write-mock-fixture") {
        let path = args.get(pos + 1).map(String::as_str).unwrap_or_else(|| {
            eprintln!("--write-mock-fixture needs a path");
            std::process::exit(2);
        });
        let bytes = mock_engine::encode_stream(&mock_engine::scenario_a())
            .expect("the canned stream encodes");
        std::fs::write(path, bytes).expect("writing the fixture");
        eprintln!("stratum-desktop: wrote {path}");
        return;
    }

    // File-association opens on Windows/Linux arrive as plain argv paths —
    // Tauri v2 exposes no runtime event for them without the single-instance
    // plugin, which this host does not carry — and a macOS Launch Services
    // open never uses argv, so scanning costs nothing there. No session can
    // exist yet; the requests wait in `file_open`'s queue until the first
    // `session_open` drains it. Unrecognised positional args are ignored, not
    // refused: argv is a shared namespace, not an open request per se.
    for arg in args.iter().skip(1).filter(|a| !a.starts_with('-')) {
        if let Ok(action) = file_open::classify(arg) {
            file_open::enqueue(action);
        }
    }

    // Build the platform singleton before anything asks for it.
    let _ = stratum_platform_host::host();

    // Stash the harness callback address and silence the env var BEFORE the
    // builder exists: `e2e_cmds::tauri_surface::attach` would otherwise dial
    // the harness at setup time, when the webview cannot yet answer, and every
    // handshake would burn its 20 s webview deadline. `ipc::app_ready` — the
    // webview's "my listener is installed" — is what dials out instead.
    #[cfg(feature = "e2e")]
    if let Some(port) = e2e_cmds::harness_port() {
        let _ = ipc::E2E_CALLBACK.set((port, e2e_cmds::harness_host()));
        std::env::remove_var(e2e_cmds::PORT_ENV);
    }

    let registry = Arc::new(SessionRegistry::new());
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<()>();

    let builder = tauri::Builder::default();
    // NOTE deliberately NOT `e2e_cmds::tauri_surface::attach(builder)`:
    // `tauri::Builder::setup` REPLACES the stored hook rather than chaining,
    // so attach's setup (which `manage`s the `Control`) would be silently
    // discarded by this file's own `.setup(...)` below — measured as a
    // "state() called before manage()" panic on the first `e2e_reply`. The
    // Control is managed inside the one real setup instead, and the three
    // fenced commands are registered through `stratum_handler!` below.
    //
    // The handler is bound inside `invoke_handler` in both branches so the
    // closure's `R = Wry` is inferable; a free-standing binding is not.
    #[cfg(feature = "e2e")]
    let builder = builder.invoke_handler(stratum_handler![
        e2e_cmds::tauri_surface::e2e_dispatch,
        e2e_cmds::tauri_surface::e2e_snapshot,
        e2e_cmds::tauri_surface::e2e_reply,
    ]);
    #[cfg(not(feature = "e2e"))]
    let builder = builder.invoke_handler(stratum_handler![]);

    let registry_for_setup = Arc::clone(&registry);
    let app = builder
        .register_asynchronous_uri_scheme_protocol("stratum-asset", ipc::handle_asset)
        .setup(move |app| {
            // THIS HOOK NEVER RETURNS `Err`. A returned error is a panic inside
            // an Objective-C callback on macOS, which is a SIGABRT and a crash
            // report — see `refuse_to_start`, the one exit for every failure
            // below. The two fallible halves are matched right here, and
            // `bring_up` reports its reason as a `String`: no `?` inside it
            // can reach Tauri.
            //
            // The engine, supervised (W07), comes first: spawned before the
            // first window so `session_open` from a fast-booting webview finds
            // it running. A spawn is a synchronous fork/exec — it cannot hang,
            // only fail — so the only clock in the bring-up is on the
            // selftest's handshake, inside `bring_up`.
            let resource_dir = app.path().resource_dir().ok();
            let engine =
                match tauri::async_runtime::block_on(spawn_engine(force_mock, resource_dir)) {
                    Ok(engine) => engine,
                    Err(e) => refuse_to_start(&spawn_failure(&e)),
                };
            if let Err(why) = bring_up(
                app,
                Arc::clone(&engine),
                registry_for_setup,
                (ready_tx, ready_rx),
                selftest,
            ) {
                // The engine is up and the host is not going to be. A bounded
                // goodbye (Shutdown request, close, reap) before the exit,
                // because `process::exit` runs no destructors and the child's
                // `kill_on_drop` is one.
                tauri::async_runtime::block_on(engine.shutdown());
                refuse_to_start(&why);
            }
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("stratum-desktop failed to start");

    // The exit code the host itself asked for through `AppHandle::exit(code)`.
    //
    // tauri-runtime-wry 2.11 turns `Message::RequestExit(code)` into a bare
    // `ControlFlow::Exit` and drops the code on the floor (tao would honour
    // `ExitWithCode`, wry never sets it), so `App::run` ends in
    // `process::exit(0)` no matter what was requested. The smoke gate caught
    // the consequence: a selftest whose webview never loaded printed FAILED
    // and exited 0 — a false green on the one assertion that exists to catch
    // exactly that. So the code is taken from `RunEvent::ExitRequested`, the
    // loop is run with `run_return` (tao's `run` is `run_return` plus
    // `process::exit`, nothing else), and the process leaves with the code it
    // was asked for once the loop has drained — engine goodbye included.
    //
    // `ExitRequested { code: None }` is the user closing the last window; that
    // stays 0. A restart request never reaches the exit below: Tauri
    // re-executes the process from inside `RunEvent::Exit`.
    let requested_exit_code = Arc::new(AtomicI32::new(0));
    let requested_exit_code_sink = Arc::clone(&requested_exit_code);
    let loop_exit_code = app.run_return(move |app, event| match event {
        // Files the OS asked this app to open (double-click, drag onto the
        // Dock icon) — the Apple Event behind the Info.plist's Alternate-rank
        // registration. Delivered as file:// URLs; anything else (the
        // stratum:// scheme from CFBundleURLTypes) has no open path yet and
        // is refused with its URL, not silently dropped.
        #[cfg(target_os = "macos")]
        tauri::RunEvent::Opened { urls } => {
            for url in &urls {
                match url.to_file_path() {
                    Ok(p) => file_open::open_request(app, &p.to_string_lossy()),
                    Err(()) => {
                        eprintln!("stratum-desktop: cannot open {url}: not a local file path");
                    }
                }
            }
        }
        tauri::RunEvent::ExitRequested {
            code: Some(code), ..
        } => {
            requested_exit_code_sink.store(code, Ordering::Relaxed);
        }
        tauri::RunEvent::Exit => {
            // A clean engine goodbye: Shutdown request, close, reap. This
            // is what keeps `stratum serve` from outliving its window on
            // the one platform with no PDEATHSIG (macOS).
            //
            // `try_state`, not `state`: this callback runs inside the event
            // loop, where a panic is the same abort-without-unwinding the
            // setup hook guards against. The state is always managed by the
            // time a normal exit gets here; a guard that costs one branch is
            // cheaper than being wrong about that once.
            if let Some(state) = app.try_state::<HostState>() {
                let engine = Arc::clone(&state.engine);
                tauri::async_runtime::block_on(engine.shutdown());
            }
        }
        _ => {}
    });
    let requested = requested_exit_code.load(Ordering::Relaxed);
    std::process::exit(if requested != 0 {
        requested
    } else {
        loop_exit_code
    });
}
