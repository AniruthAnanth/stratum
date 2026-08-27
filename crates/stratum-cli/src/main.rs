//! `stratum` — the headless command-line entry point (spec §30).
//!
//! The statistical engine must be usable with no GUI: for testing, CI, servers,
//! batch execution and reproducibility. This binary is that entry point, and the
//! desktop app is a client of the same engine rather than the only way to reach
//! it.
//!
//! # Three things this file is responsible for, and nothing else
//!
//! 1. **stdout belongs to the stream.** CONTRACTS §7 guarantee 4 — "in `--json`
//!    mode stdout carries only the NDJSON stream; all logging, progress and
//!    human chatter goes to stderr" — is what makes `stratum run x.do --json |
//!    jq` work. It is enforced here by construction: the `tracing` subscriber is
//!    installed with `.with_writer(std::io::stderr)`, every command takes its
//!    two writers as arguments, and there is no `println!` anywhere in the
//!    crate.
//! 2. **Every path lands on design 08 §4.4's ladder.** `dispatch` returns
//!    `Result<ExitCode, CmdError>` and both arms are mapped in one place, so a
//!    verb cannot invent a code.
//! 3. **A panic is exit 9, not a signal.** The run is wrapped in
//!    `catch_unwind`, so an invariant failure is reported as "always a bug"
//!    rather than as 134/SIGABRT, which the shell reserves.
//!
//! # W07's module
//!
//! `mod serve` (W07) carried an `#[allow(dead_code)]` while nothing consumed it.
//! **It is gone**: `cmd/serve.rs` wires the backend and uses the four items
//! W07's manifest note named. See that file's header.

// W07 owns this module; W09 owns everything else in the binary.
mod serve;

mod cli;
mod cmd;
mod output;

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};

use clap::Parser;

use crate::cli::{Cli, Command, ExitCode};
use crate::cmd::CmdError;

/// Set by the SIGINT handler; read by the run pump between events.
///
/// A flag rather than an unwind: a signal handler may only touch async-signal-
/// safe state, and the pump has to reach a point where it can *close the stream*
/// (CONTRACTS §7 g1 — `RunFinished` is last, including on interrupt) before it
/// exits. Killing the process from the handler would leave a consumer waiting
/// for a `RunFinished` that never comes.
static INTERRUPTED: AtomicBool = AtomicBool::new(false);

/// Has the user pressed Ctrl-C?
pub(crate) fn interrupted() -> bool {
    INTERRUPTED.load(Ordering::Relaxed)
}

fn main() {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            // clap writes help and version to stdout and errors to stderr, and
            // exits 0 for the former. §4.4's exit 2 is for a real usage error.
            let _ = e.print();
            std::process::exit(usage_exit_code(&e).code());
        }
    };

    init_tracing(cli.log.as_deref());
    install_interrupt_handler();

    let mut out = std::io::stdout().lock();
    let mut err = std::io::stderr().lock();

    // A panic in a command is a bug, and design 08 §4.4 gives bugs their own
    // code. Without this it would be 134/SIGABRT, which the shell reserves for
    // signals and which no CI script can tell apart from a real crash.
    let code = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        dispatch(&cli.command, &mut out, &mut err)
    })) {
        Ok(Ok(code)) => code,
        Ok(Err(e)) => {
            let _ = writeln!(err, "stratum: {e}");
            e.exit_code()
        }
        Err(_) => {
            // The hook already printed the payload and the backtrace.
            let _ = writeln!(
                err,
                "stratum: internal error — this is a bug; please report it with \
                 the backtrace above"
            );
            ExitCode::Internal
        }
    };

    let _ = out.flush();
    let _ = err.flush();
    std::process::exit(code.code());
}

/// What clap's refusal to parse means for §4.4.
///
/// `--help` and `--version` are *successful* invocations that clap reports as
/// errors, and they go to stdout; everything else is a real usage error (exit 2)
/// on stderr. Extracted from `main` so the mapping is testable — a shell script
/// that treats `stratum --help` as a failure is a broken shell script, and this
/// is the only thing standing between it and one.
fn usage_exit_code(e: &clap::error::Error) -> ExitCode {
    match e.kind() {
        clap::error::ErrorKind::DisplayHelp
        | clap::error::ErrorKind::DisplayVersion
        | clap::error::ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => ExitCode::Success,
        _ => ExitCode::Usage,
    }
}

/// One place, so a verb cannot invent an exit code.
fn dispatch(
    command: &Command,
    out: &mut impl Write,
    err: &mut impl Write,
) -> Result<ExitCode, CmdError> {
    match command {
        Command::Run(a) => cmd::run::run(a, out, err),
        Command::Exec(a) => cmd::exec::exec(a, out, err),
        Command::Serve(a) => cmd::serve::serve(a, out, err),
        Command::Check(a) => cmd::check::check(a, out, err),
        Command::Fmt(a) => cmd::fmt::fmt(a, out, err),
        Command::Describe(a) => cmd::describe::describe(a, out, err),
        Command::Data(a) => cmd::data::data(a, out, err),
        Command::Init(a) => cmd::init::init(a, out, err),
        Command::Doctor(a) => cmd::doctor::doctor(a, out, err),
        Command::Version(a) => cmd::version::version(a, out, err),
        Command::Completions(a) => cmd::completions::completions(a, out, err),
    }
}

/// **stderr, always.** See CONTRACTS §7 guarantee 4.
fn init_tracing(filter: Option<&str>) {
    use tracing_subscriber::filter::EnvFilter;
    let env = filter.map(EnvFilter::new).unwrap_or_else(|| {
        EnvFilter::try_from_env("STRATUM_LOG").unwrap_or_else(|_| EnvFilter::new("warn"))
    });
    let _ = tracing_subscriber::fmt()
        .with_env_filter(env)
        .with_writer(std::io::stderr)
        .with_target(false)
        .try_init();
}

/// Ctrl-C sets a flag the pump polls; see [`INTERRUPTED`].
#[cfg(unix)]
fn install_interrupt_handler() {
    extern "C" fn on_sigint(_: libc::c_int) {
        // Async-signal-safe: a relaxed store on a `static AtomicBool` and
        // nothing else. No allocation, no locking, no formatting.
        INTERRUPTED.store(true, Ordering::Relaxed);
    }
    // SAFETY: `signal` with a plain `extern "C" fn` handler that touches only an
    // `AtomicBool`. This is the same shape `stratum-platform-macos` uses for its
    // process helpers, and the handler does nothing that is not
    // async-signal-safe.
    unsafe {
        libc::signal(libc::SIGINT, on_sigint as *const () as libc::sighandler_t);
    }
}

/// Windows has no `signal(2)`; the console handler is `stratum-platform-windows`'
/// (work unit W24). A run there is not interruptible from the CLI yet, which is
/// a gap and not a silent one: `doctor` reports it.
#[cfg(not(unix))]
fn install_interrupt_handler() {}

#[cfg(test)]
mod tests {
    //! **Design 08 §4.4's ladder, end to end, through `dispatch`.**
    //!
    //! The acceptance is "exit codes 0–10 asserted, with 10 distinct from 1".
    //! `cli.rs` asserts the *table* — that the eleven numbers are contiguous and
    //! that [`crate::cli::RunOutcome`] maps onto them — but a table is not a
    //! ladder: a code no command can produce is as much a defect as a command
    //! that produces an undocumented one. So every rung below is reached by
    //! calling the same `dispatch` the binary calls, with the same argv a user
    //! would type.
    //!
    //! Two rungs are driven from a synthesised `--replay` capture rather than
    //! from a real engine, because there is no engine in this build (see
    //! `cmd/mod.rs`). That is not a weaker test of the ladder: `run` computes
    //! its exit code from the *stream*, so a stream that says a block failed
    //! with `r(111)` exercises exactly the path a linked engine will take.

    use camino::Utf8PathBuf;
    use clap::Parser;
    use stratum_proto::engine::{EngineEvent, STREAM_SCHEMA};
    use stratum_proto::exec::ExecStatus;
    use stratum_proto::frame::Envelope;
    use stratum_proto::ids::{
        BlockId, CodeHash, DatasetStateId, DocumentId, ExecutionId, RunId, SessionId, Span,
    };

    use super::*;

    /// Run one argv through the real dispatcher and collapse both arms the way
    /// `main` does.
    fn code_of(argv: &[&str]) -> ExitCode {
        let cli = match Cli::try_parse_from(argv) {
            Ok(cli) => cli,
            Err(e) => return usage_exit_code(&e),
        };
        let mut out = Vec::new();
        let mut err = Vec::new();
        dispatch(&cli.command, &mut out, &mut err).unwrap_or_else(|e| e.exit_code())
    }

    fn run_started(seq: u64) -> EngineEvent {
        EngineEvent::RunStarted {
            seq,
            schema: STREAM_SCHEMA,
            run: RunId(1),
            session: SessionId(1),
            stratum_version: "test".to_owned(),
            source: None,
            clean_state: true,
            cwd: Utf8PathBuf::from("/tmp"),
            started_at_ms: 0,
            seed: None,
            plan_len: 1,
        }
    }

    fn block_started(seq: u64) -> EngineEvent {
        EngineEvent::BlockStarted {
            seq,
            run: RunId(1),
            exec: ExecutionId(1),
            block: BlockId(1),
            doc: Some(DocumentId(1)),
            span: Span { start: 0, end: 1 },
            code_hash: CodeHash([0; 16]),
            dataset_state_in: DatasetStateId(0),
            text: "regress price mpg".to_owned(),
        }
    }

    fn block_finished(seq: u64, status: ExecStatus, rc: u32) -> EngineEvent {
        EngineEvent::BlockFinished {
            seq,
            run: RunId(1),
            exec: ExecutionId(1),
            block: BlockId(1),
            result: None,
            status,
            rc,
            duration_us: 0,
            dataset_state_out: DatasetStateId(0),
        }
    }

    fn run_finished(seq: u64, rc: u32) -> EngineEvent {
        EngineEvent::RunFinished {
            seq,
            run: RunId(1),
            rc,
            blocks_run: 1,
            blocks_failed: u32::from(rc != 0),
            duration_us: 0,
            finished_at_ms: 0,
        }
    }

    /// A §7.1 NDJSON capture on disk. Written by hand rather than through
    /// `JsonSink`, because one of the cases below is a capture that BREAKS the
    /// framing guarantees and `JsonSink` would — correctly — refuse to write it.
    fn capture(dir: &std::path::Path, name: &str, events: &[EngineEvent]) -> String {
        let path = dir.join(name);
        let mut text = String::new();
        for ev in events {
            let body = serde_json::to_value(ev).expect("an EngineEvent serialises");
            text.push_str(&serde_json::to_string(&Envelope::event(body)).expect("an envelope"));
            text.push('\n');
        }
        std::fs::write(&path, text).expect("write the capture");
        path.to_str().expect("utf-8 tempdir").to_owned()
    }

    fn write(dir: &std::path::Path, name: &str, body: &str) -> String {
        let path = dir.join(name);
        std::fs::write(&path, body).expect("write");
        path.to_str().expect("utf-8 tempdir").to_owned()
    }

    fn smoke() -> String {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/smoke/hello.do")
            .to_str()
            .expect("utf-8 repo path")
            .to_owned()
    }

    /// **The acceptance bullet.** All eleven, each from a real invocation.
    #[test]
    fn every_rung_of_the_ladder_is_reachable_from_the_command_line() {
        let dir = tempfile::tempdir().expect("tempdir");
        let d = dir.path();

        // 0 — success.
        assert_eq!(code_of(&["stratum", "version"]), ExitCode::Success);

        // 1 — a Stata return code occurred. WE ARE WRONG.
        let wrong = capture(
            d,
            "wrong.jsonl",
            &[
                run_started(0),
                block_started(1),
                block_finished(
                    2,
                    ExecStatus::Failed {
                        rc: 111,
                        message: "variable income not found".to_owned(),
                        span: None,
                    },
                    111,
                ),
                run_finished(3, 111),
            ],
        );
        assert_eq!(
            code_of(&[
                "stratum",
                "run",
                &smoke(),
                "--format",
                "quiet",
                "--replay",
                &wrong
            ]),
            ExitCode::RuntimeError
        );

        // 2 — usage. Emitted by clap, mapped by `usage_exit_code`.
        assert_eq!(code_of(&["stratum", "nosuchverb"]), ExitCode::Usage);
        assert_eq!(
            code_of(&["stratum", "run", "--nosuchflag"]),
            ExitCode::Usage
        );

        // 3 — I/O.
        assert_eq!(
            code_of(&["stratum", "run", "/nonexistent/nope.do"]),
            ExitCode::Io
        );

        // 4 — parse. The file was never executed.
        let broken = write(d, "broken.do", "/* never closed\ndisplay 2+2\n");
        assert_eq!(code_of(&["stratum", "run", &broken]), ExitCode::Parse);

        // 5 — `check` found something at or above the deny level.
        let lint = write(d, "lint.do", "use \"/Users/ana/data/raw.dta\", clear\n");
        assert_eq!(
            code_of(&["stratum", "check", &lint, "--deny", "warning", "--format", "quiet"]),
            ExitCode::CheckFailed
        );

        // 6 — `fmt --check` would reformat.
        let messy = write(d, "messy.do", "sysuse auto, clear   \n");
        assert_eq!(
            code_of(&["stratum", "fmt", &messy, "--check"]),
            ExitCode::FormatChanged
        );

        // 7 — interrupted. The stream says a block was, and the exit code
        // follows the stream rather than the signal, which is what makes this
        // testable without racing a real SIGINT.
        let broken_off = capture(
            d,
            "interrupted.jsonl",
            &[
                run_started(0),
                block_started(1),
                block_finished(
                    2,
                    ExecStatus::Interrupted {
                        rolled_back: false,
                        at: None,
                    },
                    1,
                ),
                run_finished(3, 1),
            ],
        );
        assert_eq!(
            code_of(&[
                "stratum",
                "run",
                &smoke(),
                "--format",
                "quiet",
                "--replay",
                &broken_off
            ]),
            ExitCode::Interrupted
        );

        // 8 — timeout.
        let slow = capture(
            d,
            "slow.jsonl",
            &[
                run_started(0),
                block_started(1),
                block_finished(2, ExecStatus::Succeeded, 0),
                run_finished(3, 0),
            ],
        );
        assert_eq!(
            code_of(&[
                "stratum",
                "run",
                &smoke(),
                "--format",
                "quiet",
                "--replay",
                &slow,
                "--timeout",
                "0"
            ]),
            ExitCode::Timeout
        );

        // 9 — internal. A capture that breaks CONTRACTS §7 is always a bug in
        // whoever produced it, and the framing guard turns it into exit 9 at the
        // event that broke it rather than into a malformed pipe.
        let malformed = capture(d, "nested.jsonl", &[run_started(0), run_started(1)]);
        assert_eq!(
            code_of(&[
                "stratum",
                "run",
                &smoke(),
                "--format",
                "quiet",
                "--replay",
                &malformed
            ]),
            ExitCode::Internal
        );

        // 10 — unsupported. WE ARE INCOMPLETE, and it is not 1.
        assert_eq!(
            code_of(&["stratum", "run", &smoke(), "--format", "quiet"]),
            ExitCode::Unsupported
        );
        assert_ne!(ExitCode::Unsupported, ExitCode::RuntimeError);
    }

    /// `stratum --help` and `stratum --version` are successful invocations that
    /// clap reports as errors. A CI script that saw exit 2 for `--version` would
    /// be right to fail the build.
    #[test]
    fn help_and_version_are_not_usage_errors() {
        for argv in [
            vec!["stratum", "--help"],
            vec!["stratum", "--version"],
            vec!["stratum", "run", "--help"],
            vec!["stratum"],
        ] {
            let e = Cli::try_parse_from(&argv).expect_err("clap reports these as errors");
            assert_eq!(usage_exit_code(&e), ExitCode::Success, "{argv:?}");
        }
    }
}
