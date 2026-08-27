//! `stratum run <FILE.do>` — the primary verb (design 08 §4.1).
//!
//! **Always clean-state.** Fresh dataset, macros, estimates and frames, every
//! time; there is no `--dirty` flag and there must never be one. That is the CI
//! and reproducibility contract of design 08 §15/§16, and interactive execution
//! against live state is `serve`'s job. `cli::tests::run_has_no_dirty_flag`
//! asserts the absence, because the cheapest way for this to regress is somebody
//! adding a convenience flag.
//!
//! The pump below is the same for `run` and `exec` (design 08 §4.1: "same flags
//! as `run`"), and it is deliberately *streaming*: one event is read, checked,
//! written and dropped before the next is read. Nothing accumulates a run's
//! events in memory, which is what makes `stratum run` over a script that
//! produces a million `Output` chunks a constant-memory operation.

use std::io::Write;
use std::time::Instant;

use camino::{Utf8Path, Utf8PathBuf};
use stratum_proto::diagnostic::Severity;
use stratum_proto::engine::EngineEvent;
use stratum_proto::exec::ExecStatus;
use stratum_proto::ids::DatasetStateId;

use crate::cli::{ExecCommon, ExitCode, Format, RunArgs};
use crate::cmd::{
    executable_region_count, now_ms, read_to_string, CmdError, Engine, RunEngine, RunShape, CLI_RUN,
};
use crate::output::json::{JsonSink, OutputError};
use crate::output::text::TextSink;
use crate::output::{human, FramingGuard, Tally};

/// `stratum run`.
///
/// # Errors
/// [`CmdError`] for anything the exit ladder names; the caller maps it.
pub fn run(
    args: &RunArgs,
    out: &mut impl Write,
    err: &mut impl Write,
) -> Result<ExitCode, CmdError> {
    let src = read_to_string(&args.file)?;
    let entry = absolutize(&args.file);

    // A file that does not parse is never executed (exit 4). Segmentation
    // diagnostics are the scanner's own — an unterminated brace, an unclosed
    // block comment — and they are structural: no engine could run this text.
    let seg = stratum_parse::segment(&src);
    let fatal: Vec<_> = seg
        .diags
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    if !fatal.is_empty() {
        for d in &fatal {
            let mut d = (*d).clone();
            d.file = Some(entry.clone());
            human::diagnostic(err, &d).map_err(|source| CmdError::Io {
                path: Utf8PathBuf::from("<stderr>"),
                source,
            })?;
        }
        return Err(CmdError::Parse { path: entry });
    }

    let cwd = args
        .common
        .cd
        .clone()
        .unwrap_or_else(|| entry.parent().map_or_else(cwd_or_dot, Utf8Path::to_owned));

    let shape = RunShape {
        source: Some(entry.clone()),
        cwd,
        plan_len: executable_region_count(&seg),
        seed: args.common.seed,
        // Design 08 §4.1. Not a variable, not a flag, not configurable.
        clean_state: true,
    };
    let engine = Engine::open(args.common.replay.as_deref(), shape)?;
    let base = entry.parent().map(|p| p.to_string());
    // `run` echoes: a classic log of a do-file shows `. command` before its
    // output, the way `stata -b` writes one. `exec` passes `Echo::No` — the user
    // typed the command, so repeating it back is noise.
    drive(engine, &args.common, base, Echo::Yes, out, err)
}

/// Whether the classic log repeats each block's source as `. command`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Echo {
    /// `stratum run` — a do-file log looks like `stata -b`'s.
    Yes,
    /// `stratum exec` — the user just typed it.
    No,
}

/// The shared pump. `exec` calls it with a source of `None`.
///
/// # Errors
/// [`CmdError`].
pub fn drive(
    mut engine: impl RunEngine,
    common: &ExecCommon,
    base: Option<String>,
    echo: Echo,
    out: &mut impl Write,
    err: &mut impl Write,
) -> Result<ExitCode, CmdError> {
    let mut sink = Sink::new(common, base, echo, out);
    let mut log = open_log(common, echo)?;
    let mut profile: Vec<(u64, u64)> = Vec::new();
    let started = Instant::now();
    let mut stop = None;

    while let Some(ev) = engine.next_event() {
        if common.profile {
            if let EngineEvent::BlockFinished {
                exec, duration_us, ..
            } = &ev
            {
                profile.push((exec.0, *duration_us));
            }
        }
        sink.event(&ev)?;
        if let Some(log) = log.as_mut() {
            log.event(&ev)?;
        }
        if crate::interrupted() {
            stop = Some(Stop::Interrupt);
            break;
        }
        if let Some(ms) = common.timeout {
            if started.elapsed().as_millis() as u64 >= ms {
                stop = Some(Stop::Timeout);
                break;
            }
        }
    }

    // CONTRACTS §7 guarantee 1: `RunFinished` is last, "including on error,
    // interrupt, and timeout". If we truncated the stream ourselves, we close
    // it ourselves — a consumer that never sees `RunFinished` waits forever, and
    // that is a worse failure than the one that caused the truncation. `rc = 1`
    // is Stata's own return code for a user break.
    if let Some(stop) = stop {
        for ev in sink.closing_events() {
            sink.event(&ev)?;
            if let Some(log) = log.as_mut() {
                log.event(&ev)?;
            }
        }
        match stop {
            Stop::Interrupt => sink.tally_mut().outcome.interrupted = true,
            Stop::Timeout => sink.tally_mut().outcome.timed_out = true,
        }
    }

    sink.finish()?;
    if let Some(log) = log.as_mut() {
        log.finish()?;
    }
    sink.log_counters();

    let tally = *sink.tally();
    let exit = tally.outcome.exit_code();
    write_rc_file(common, tally.outcome.rc)?;

    // Everything below is chatter and goes to stderr — CONTRACTS §7 g4, which
    // is what makes `stratum run x.do --json | jq` work.
    if common.profile {
        writeln!(err, "  execution        duration").ok();
        for (exec, us) in &profile {
            writeln!(err, "  E{exec:<14} {:>9.3} ms", *us as f64 / 1000.0).ok();
        }
    }
    if common.resolved_format() != Format::Quiet {
        human::summary(err, &tally, exit).map_err(|source| CmdError::Io {
            path: Utf8PathBuf::from("<stderr>"),
            source,
        })?;
    }
    Ok(exit)
}

#[derive(Clone, Copy)]
enum Stop {
    Interrupt,
    Timeout,
}

/// The three renderings, behind one interface, so the pump is written once.
enum Sink<'w, W: Write> {
    Json(JsonSink<&'w mut W>),
    Text(Box<TextSink<&'w mut W>>),
    /// `--format quiet`: the exit code is the answer. The framing guard and the
    /// tally still run, because "quiet" means "no bytes on stdout", not "do not
    /// check the stream".
    Quiet(FramingGuard, Tally),
}

impl<'w, W: Write> Sink<'w, W> {
    fn new(common: &ExecCommon, base: Option<String>, echo: Echo, out: &'w mut W) -> Self {
        match common.resolved_format() {
            Format::Json if common.deterministic => Sink::Json(JsonSink::deterministic(out, base)),
            Format::Json => Sink::Json(JsonSink::new(out)),
            Format::Text => Sink::Text(Box::new(text_sink(out, echo))),
            Format::Quiet => Sink::Quiet(FramingGuard::new(), Tally::default()),
        }
    }

    /// ADR-017's counters, on **stderr** through `tracing` (CONTRACTS §7 g4).
    ///
    /// They are recorded rather than asserted here, and every one of them is a
    /// count of work rather than a duration: lines written against events
    /// admitted catches a sink that drops or duplicates a record, `round_trips`
    /// catches a normalizer that has become a second pass over an
    /// already-serialised stream, and `flattens` catches a log that re-walks its
    /// own buffer. The in-crate tests assert the same numbers; this is what makes
    /// them visible on a real run.
    fn log_counters(&self) {
        let guard = self.guard();
        // Guarantee 1 as a pair of counters: `admit`/`finish` already refuse a
        // stream where these two disagree, so seeing them here is how a reader
        // of the log confirms the guard actually ran rather than inferring it.
        tracing::debug!(
            runs_started = guard.runs_started(),
            runs_finished = guard.runs_finished(),
            "stream framing"
        );
        match self {
            Sink::Json(s) => tracing::debug!(
                events = s.tally().events,
                lines = s.lines_written(),
                round_trips = s.round_trips(),
                "ndjson stream written"
            ),
            Sink::Text(s) => tracing::debug!(
                events = s.tally().events,
                flattens = s.flattens(),
                bytes = s.bytes(),
                "classic log written"
            ),
            Sink::Quiet(_, t) => {
                tracing::debug!(events = t.events, "stream checked, nothing written")
            }
        }
    }

    fn event(&mut self, ev: &EngineEvent) -> Result<(), OutputError> {
        match self {
            Sink::Json(s) => s.event(ev),
            Sink::Text(s) => s.event(ev),
            Sink::Quiet(g, t) => {
                g.admit(ev)?;
                t.observe(ev);
                Ok(())
            }
        }
    }

    fn finish(&mut self) -> Result<(), OutputError> {
        match self {
            Sink::Json(s) => s.finish(),
            Sink::Text(s) => s.finish(),
            Sink::Quiet(g, _) => g.finish().map_err(Into::into),
        }
    }

    fn tally(&self) -> &Tally {
        match self {
            Sink::Json(s) => s.tally(),
            Sink::Text(s) => s.tally(),
            Sink::Quiet(_, t) => t,
        }
    }

    fn tally_mut(&mut self) -> &mut Tally {
        match self {
            Sink::Json(s) => s.tally_mut(),
            Sink::Text(s) => s.tally_mut(),
            Sink::Quiet(_, t) => t,
        }
    }

    fn guard(&self) -> &FramingGuard {
        match self {
            Sink::Json(s) => s.guard(),
            Sink::Text(s) => s.guard(),
            Sink::Quiet(g, _) => g,
        }
    }

    /// The events needed to close a stream we truncated. Empty when nothing is
    /// open, which is the normal path.
    fn closing_events(&self) -> Vec<EngineEvent> {
        let g = self.guard();
        let mut seq = g.last_seq().map_or(0, |s| s + 1);
        let mut out = Vec::new();
        if let Some((exec, block)) = g.open_block() {
            out.push(EngineEvent::BlockFinished {
                seq,
                run: g.open_run().unwrap_or(CLI_RUN),
                exec,
                block,
                result: None,
                status: ExecStatus::Interrupted {
                    rolled_back: false,
                    at: None,
                },
                // Stata's own return code for a user break.
                rc: 1,
                duration_us: 0,
                dataset_state_out: DatasetStateId(0),
            });
            seq += 1;
        }
        if let Some(run) = g.open_run() {
            out.push(EngineEvent::RunFinished {
                seq,
                run,
                rc: 1,
                blocks_run: self.tally().blocks_run,
                blocks_failed: self.tally().blocks_failed,
                duration_us: 0,
                finished_at_ms: now_ms(),
            });
        }
        out
    }
}

/// A classic-log sink that echoes, or does not. One place, so the log file and
/// stdout can never disagree about it.
fn text_sink<W: Write>(out: W, echo: Echo) -> TextSink<W> {
    match echo {
        Echo::Yes => TextSink::new(out),
        Echo::No => TextSink::new(out).without_echo(),
    }
}

/// `--log-file <PATH>`: a Stata-compatible text log beside whatever stdout
/// carries.
fn open_log(common: &ExecCommon, echo: Echo) -> Result<Option<TextSink<std::fs::File>>, CmdError> {
    let Some(path) = &common.log_file else {
        return Ok(None);
    };
    let file = std::fs::File::create(path).map_err(|source| CmdError::Io {
        path: path.clone(),
        source,
    })?;
    Ok(Some(text_sink(file, echo)))
}

/// `--rc-file <PATH>`: the final Stata return code as decimal text. Design 08
/// §4.4 offers it to anyone scripting around Stata's "always exit 0" behaviour.
fn write_rc_file(common: &ExecCommon, rc: u32) -> Result<(), CmdError> {
    let Some(path) = &common.rc_file else {
        return Ok(());
    };
    std::fs::write(path, format!("{rc}\n")).map_err(|source| CmdError::Io {
        path: path.clone(),
        source,
    })
}

/// Absolute, `/`-separated, without touching the filesystem for a path that is
/// already absolute. `--deterministic` relativises it again, so the only thing
/// that matters here is that two runs from different working directories agree.
fn absolutize(p: &Utf8Path) -> Utf8PathBuf {
    if p.is_absolute() {
        return p.to_owned();
    }
    cwd_or_dot().join(p)
}

fn cwd_or_dot() -> Utf8PathBuf {
    std::env::current_dir()
        .ok()
        .and_then(|p| Utf8PathBuf::from_path_buf(p).ok())
        .unwrap_or_else(|| Utf8PathBuf::from("."))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::Cli;
    use crate::output::fixture;
    use clap::Parser;

    fn parse(argv: &[&str]) -> RunArgs {
        let cli = Cli::try_parse_from(argv).expect("argv parses");
        match cli.command {
            crate::cli::Command::Run(a) => a,
            other => panic!("expected `run`, got {other:?}"),
        }
    }

    fn go(argv: &[&str]) -> (ExitCode, String, String) {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run(&parse(argv), &mut out, &mut err).unwrap_or_else(|e| e.exit_code());
        (
            code,
            String::from_utf8(out).expect("stdout is UTF-8"),
            String::from_utf8(err).expect("stderr is UTF-8"),
        )
    }

    fn smoke() -> String {
        fixture::repo_root()
            .join("tests/smoke/hello.do")
            .to_string_lossy()
            .into_owned()
    }

    /// **The acceptance bullet.** `stratum run tests/smoke/hello.do --json`
    /// emits a well-framed stream: exactly one `RunStarted` first and one
    /// `RunFinished` last, never interleaved `BlockStarted`/`BlockFinished`,
    /// **stdout NDJSON only** with all chatter on stderr.
    #[test]
    fn run_json_emits_ndjson_on_stdout_and_chatter_on_stderr() {
        let (_, out, err) = go(&["stratum", "run", &smoke(), "--json"]);

        let lines: Vec<serde_json::Value> = out
            .lines()
            .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("`{l}`: {e}")))
            .collect();
        assert!(!lines.is_empty(), "the stream is not empty");
        assert_eq!(lines.first().unwrap()["body"]["event"], "run_started");
        assert_eq!(lines.last().unwrap()["body"]["event"], "run_finished");
        assert_eq!(
            lines
                .iter()
                .filter(|l| l["body"]["event"] == "run_started")
                .count(),
            1,
            "exactly one RunStarted"
        );
        assert_eq!(
            lines
                .iter()
                .filter(|l| l["body"]["event"] == "run_finished")
                .count(),
            1,
            "exactly one RunFinished"
        );
        // The property `| jq` depends on: stdout is NOTHING but the stream.
        for l in out.lines() {
            assert!(
                l.starts_with('{') && l.ends_with('}'),
                "chatter on stdout: {l}"
            );
        }
        assert!(
            err.contains("stratum:"),
            "the summary goes to stderr: {err:?}"
        );
        assert!(!out.contains("stratum:"));
    }

    /// The other half of guarantee 4: a `--format text` run puts the classic log
    /// on stdout and still keeps the summary on stderr.
    #[test]
    fn run_text_puts_the_log_on_stdout_and_the_summary_on_stderr() {
        let (_, out, err) = go(&["stratum", "run", &smoke()]);
        assert!(!out.contains("stratum:"));
        assert!(err.contains("stratum:"));
    }

    /// **Exit 10, distinct from 1.** No engine is linked, so the run is
    /// *incomplete*, not *wrong* — and CI for a Stata-compatibility project has
    /// to be able to tell those apart.
    #[test]
    fn a_run_with_no_engine_is_unsupported_not_a_runtime_error() {
        let (code, _, err) = go(&["stratum", "run", &smoke(), "--json"]);
        assert_eq!(code, ExitCode::Unsupported);
        assert_ne!(code, ExitCode::RuntimeError);
        assert!(err.contains("exit 10"), "{err}");
    }

    /// Exit 3: the input is not there.
    #[test]
    fn a_missing_file_is_an_io_error() {
        let (code, out, _) = go(&["stratum", "run", "/nonexistent/nope.do", "--json"]);
        assert_eq!(code, ExitCode::Io);
        assert!(
            out.is_empty(),
            "nothing was executed, so nothing was streamed"
        );
    }

    /// Exit 4: the file was never executed.
    #[test]
    fn an_unparseable_file_is_a_parse_error_and_runs_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("broken.do");
        // An unterminated block comment: the scanner cannot close it at EOF.
        std::fs::write(&path, "/* never closed\ndisplay 2+2\n").unwrap();
        let (code, out, err) = go(&["stratum", "run", path.to_str().unwrap(), "--json"]);
        assert_eq!(code, ExitCode::Parse);
        assert!(out.is_empty(), "nothing was executed");
        assert!(err.contains("error["), "{err}");
    }

    /// `--replay` drives the real writer over W07's committed capture: three
    /// runs, real StataMP numbers, and the framing guarantees hold.
    #[test]
    fn replay_drives_a_real_engine_capture_through_the_real_writer() {
        let capture = fixture::repo_root().join("tests/fixtures/mock/scenario_a.msgpack");
        let (code, out, _) = go(&[
            "stratum",
            "run",
            &smoke(),
            "--json",
            "--replay",
            capture.to_str().unwrap(),
        ]);
        assert_eq!(code, ExitCode::Success, "the capture is a clean run");
        let starts = out.matches(r#""event":"run_started""#).count();
        let finishes = out.matches(r#""event":"run_finished""#).count();
        assert_eq!((starts, finishes), (3, 3));
    }

    /// The classic log replayed from the capture is the StataMP 18.5 text.
    #[test]
    fn replay_in_text_mode_writes_the_classic_log() {
        let capture = fixture::repo_root().join("tests/fixtures/mock/scenario_a.msgpack");
        let (_, out, _) = go(&[
            "stratum",
            "run",
            &smoke(),
            "--replay",
            capture.to_str().unwrap(),
        ]);
        assert!(
            out.contains("       price |         74    6165.257    2949.496       3291      15906")
        );
    }

    /// `--rc-file` carries the true Stata return code even when the exit code
    /// compresses it, which is the whole reason design 08 §4.4 offers it.
    #[test]
    fn the_rc_file_carries_the_stata_return_code() {
        let dir = tempfile::tempdir().unwrap();
        let rc = dir.path().join("rc.txt");
        let (code, _, _) = go(&[
            "stratum",
            "run",
            &smoke(),
            "--json",
            "--rc-file",
            rc.to_str().unwrap(),
        ]);
        assert_eq!(code, ExitCode::Unsupported);
        assert_eq!(std::fs::read_to_string(&rc).unwrap(), "10\n");
    }

    /// `--format quiet` writes nothing to stdout and still checks the stream.
    #[test]
    fn quiet_writes_nothing_to_stdout() {
        let (code, out, _) = go(&["stratum", "run", &smoke(), "--format", "quiet"]);
        assert_eq!(code, ExitCode::Unsupported);
        assert!(out.is_empty());
    }

    /// A truncated replay must still end in `RunFinished` — guarantee 1 is a
    /// promise to the consumer, and a consumer that never sees it hangs.
    #[test]
    fn a_timeout_closes_the_stream_it_truncated() {
        let dir = tempfile::tempdir().unwrap();
        let cap = dir.path().join("slow.jsonl");
        // From the first `RunStarted`, so `--timeout 0` truncates with a run
        // OPEN. The capture opens with session-scoped events (health, block
        // map) that legitimately precede any run, and breaking there would
        // leave nothing to close — a correct outcome, but not the one this test
        // is about.
        let all = fixture::scenario_a();
        let at = all
            .iter()
            .position(|e| matches!(e, EngineEvent::RunStarted { .. }))
            .expect("the capture has a run");
        let events = all[at..].to_vec();
        {
            let f = std::fs::File::create(&cap).unwrap();
            let mut w = JsonSink::new(f);
            for ev in &events {
                w.event(ev).unwrap();
            }
            w.finish().unwrap();
        }
        let (code, out, _) = go(&[
            "stratum",
            "run",
            &smoke(),
            "--json",
            "--replay",
            cap.to_str().unwrap(),
            "--timeout",
            "0",
        ]);
        assert_eq!(code, ExitCode::Timeout);
        let last: serde_json::Value =
            serde_json::from_str(out.lines().last().expect("a stream")).unwrap();
        assert_eq!(last["body"]["event"], "run_finished");
        assert_eq!(last["body"]["rc"], 1, "Stata's return code for a break");
    }

    /// `--deterministic` output is stable across runs and independent of the
    /// clock, the version string and the working directory — the three fields
    /// §7.2 names.
    #[test]
    fn deterministic_runs_are_byte_identical() {
        let argv = ["stratum", "run", &smoke(), "--json", "--deterministic"];
        let (_, a, _) = go(&argv);
        let (_, b, _) = go(&argv);
        assert_eq!(a, b);
        assert!(a.contains(r#""stratum_version":"<version>""#));
        assert!(a.contains(r#""cwd":"<cwd>""#));
        assert!(a.contains(r#""started_at_ms":0"#));
        assert!(a.contains(r#""source":"hello.do""#), "{a}");
    }

    /// **`tests/smoke/expected.jsonl`.** Design 08 §4.4 says `tests/smoke`
    /// asserts the exit codes; this is that corpus, and it also pins the
    /// framing and the §7.2 normalisation of a real invocation.
    #[test]
    fn the_smoke_stream_matches_its_committed_golden() {
        let (code, out, _) = go(&["stratum", "run", &smoke(), "--json", "--deterministic"]);
        let want = std::fs::read_to_string(fixture::repo_root().join("tests/smoke/expected.jsonl"))
            .expect("the committed golden");
        assert_eq!(out, want, "regenerate with tests/smoke/README.md's recipe");
        assert_eq!(code, ExitCode::Unsupported);
    }
}
