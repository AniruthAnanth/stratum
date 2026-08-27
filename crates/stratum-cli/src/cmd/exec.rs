//! `stratum exec` — commands against a scratch state discarded at exit.
//!
//! ```text
//! stratum exec -c 'sysuse auto, clear' -c 'summarize price'
//! cat cmds.txt | stratum exec --stdin --json
//! ```
//!
//! Design 08 §4.1: "same flags as `run`. State persists across `-c` occurrences
//! within one invocation, and is discarded at exit." So the `-c` occurrences are
//! joined into one buffer and segmented once — joining them is not a
//! convenience, it is the semantics: `#delimit ;` set by one `-c` is in force
//! for the next, and segmenting each argument independently would silently
//! mis-parse the second (design 02 §13.2, the mistake `stratum-parse`'s header
//! calls out by name).

use std::io::Write;

use crate::cli::{ExecArgs, ExitCode};
use crate::cmd::{executable_region_count, read_stdin, CmdError, Engine, RunShape};

/// `stratum exec`.
///
/// # Errors
/// [`CmdError`].
pub fn exec(
    args: &ExecArgs,
    out: &mut impl Write,
    err: &mut impl Write,
) -> Result<ExitCode, CmdError> {
    let mut source = args.code.join("\n");
    if args.stdin {
        let piped = read_stdin()?;
        if !source.is_empty() && !source.ends_with('\n') {
            source.push('\n');
        }
        source.push_str(&piped);
    }
    if source.trim().is_empty() {
        return Err(CmdError::Io {
            path: camino::Utf8PathBuf::from("<stdin>"),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "nothing to execute: pass -c CODE or --stdin",
            ),
        });
    }

    let seg = stratum_parse::segment(&source);
    let cwd = args.common.cd.clone().unwrap_or_else(|| {
        std::env::current_dir()
            .ok()
            .and_then(|p| camino::Utf8PathBuf::from_path_buf(p).ok())
            .unwrap_or_else(|| camino::Utf8PathBuf::from("."))
    });

    let shape = RunShape {
        // `exec` has no entry file, so §7.2 has no base directory to relativise
        // against and every absolute path in the stream becomes `<abs>`. That is
        // the rule, not a gap: a stream with no entry file has no anchor.
        source: None,
        cwd,
        plan_len: executable_region_count(&seg),
        seed: args.common.seed,
        // Scratch state, constructed fresh and discarded at exit — the same
        // clean-state promise `run` makes, over a different source.
        clean_state: true,
    };
    let engine = Engine::open(args.common.replay.as_deref(), shape)?;
    // `Echo::No`: the user typed these commands, so a log that repeated them
    // back would be noise. `run` echoes; see `cmd/run.rs`.
    crate::cmd::run::drive(
        engine,
        &args.common,
        None,
        crate::cmd::run::Echo::No,
        out,
        err,
    )
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;
    use crate::cli::{Cli, Command};

    fn parse(argv: &[&str]) -> ExecArgs {
        match Cli::try_parse_from(argv).expect("argv parses").command {
            Command::Exec(a) => a,
            other => panic!("expected `exec`, got {other:?}"),
        }
    }

    fn go(argv: &[&str]) -> (ExitCode, String, String) {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = exec(&parse(argv), &mut out, &mut err).unwrap_or_else(|e| e.exit_code());
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    #[test]
    fn several_dash_c_occurrences_are_one_buffer() {
        let (_, out, _) = go(&[
            "stratum",
            "exec",
            "-c",
            "sysuse auto, clear",
            "-c",
            "summarize price",
            "--json",
        ]);
        let first: serde_json::Value = serde_json::from_str(out.lines().next().unwrap()).unwrap();
        assert_eq!(first["body"]["plan_len"], 2, "two commands, one plan");
        assert!(first["body"]["source"].is_null(), "exec has no entry file");
    }

    /// `#delimit ;` set by one `-c` is in force for the next. Segmenting each
    /// argument on its own would report three regions instead of one.
    #[test]
    fn the_delimiter_mode_carries_between_occurrences() {
        let (_, out, _) = go(&[
            "stratum",
            "exec",
            "-c",
            "#delimit ;",
            "-c",
            "summarize price",
            "-c",
            "mpg weight;",
            "--json",
        ]);
        let first: serde_json::Value = serde_json::from_str(out.lines().next().unwrap()).unwrap();
        assert_eq!(
            first["body"]["plan_len"], 2,
            "the directive plus one `;`-terminated command"
        );
    }

    #[test]
    fn nothing_to_execute_is_an_input_error() {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let e =
            exec(&parse(&["stratum", "exec"]), &mut out, &mut err).expect_err("no code, no stdin");
        assert_eq!(e.exit_code(), ExitCode::Io);
    }

    #[test]
    fn exec_reports_incompleteness_the_same_way_run_does() {
        let (code, _, err) = go(&["stratum", "exec", "-c", "display 2+2", "--json"]);
        assert_eq!(code, ExitCode::Unsupported);
        assert!(err.contains("stratum:"));
    }
}
