//! The command surface (design 08 §4.1) and the exit-code ladder (§4.4).
//!
//! Both live in one file because they are one contract: every subcommand below
//! has to land on one of the eleven codes in [`ExitCode`], and a code that no
//! command can produce is as much a defect as a command that produces an
//! undocumented one. `tests/smoke/` is what §4.4 says asserts them; the
//! `#[cfg(test)]` block at the foot of this file is what asserts the mapping
//! itself, because a table that is only exercised end-to-end is a table whose
//! rarest rows are never exercised at all.

use camino::Utf8PathBuf;
use clap::{Args, Parser, Subcommand, ValueEnum};

/// Design 08 §4.4, transcribed. `#[repr(u8)]` and the explicit discriminants are
/// the contract: these numbers are in CI scripts, so they are pinned here rather
/// than left to declaration order.
///
/// 126/127 and ≥128 are avoided — the shell and the signal ranges own them.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
#[repr(u8)]
pub enum ExitCode {
    /// Every block ran and returned `r(0)`.
    Success = 0,
    /// A Stata return code `r(N)`, `N > 0`, occurred. **We are wrong.**
    RuntimeError = 1,
    /// Bad flag, bad subcommand. Emitted by clap.
    Usage = 2,
    /// Input missing/unreadable, output path unwritable.
    Io = 3,
    /// The file was never executed.
    Parse = 4,
    /// `check` found diagnostics at or above the deny level.
    CheckFailed = 5,
    /// `fmt --check` found files that would be reformatted.
    FormatChanged = 6,
    /// SIGINT / Ctrl-C. The partial stream is still well-framed.
    Interrupted = 7,
    /// `--timeout` exceeded.
    Timeout = 8,
    /// A panic was caught, or an invariant failed. Always a bug.
    Internal = 9,
    /// A syntactically valid Stata construct we have not implemented.
    /// **We are incomplete.** Distinct from [`ExitCode::RuntimeError`] on
    /// purpose: CI for a Stata-compatibility project must be able to tell the
    /// two apart (ADR-016, IMPLEMENTATION_PLAN W09).
    Unsupported = 10,
}

impl ExitCode {
    /// The number the process exits with.
    #[must_use]
    pub fn code(self) -> i32 {
        self as u8 as i32
    }

    /// Every code, in order.
    ///
    /// `#[cfg(test)]` because it has exactly one job — letting
    /// `tests::the_exit_ladder_is_contiguous_zero_to_ten_and_unique` walk the
    /// whole table — and a table that the shipped binary never reads should say
    /// so rather than sit behind an `allow(dead_code)`.
    #[cfg(test)]
    pub const ALL: [ExitCode; 11] = [
        ExitCode::Success,
        ExitCode::RuntimeError,
        ExitCode::Usage,
        ExitCode::Io,
        ExitCode::Parse,
        ExitCode::CheckFailed,
        ExitCode::FormatChanged,
        ExitCode::Interrupted,
        ExitCode::Timeout,
        ExitCode::Internal,
        ExitCode::Unsupported,
    ];
}

/// The Stata return code an *unimplemented construct* raises.
///
/// ADR-016 pins it: `set linesize` other than 80 "fails with **`rc = 10`** and
/// diagnostic `STRATUM0010`". Everything in the CLI that has to decide between
/// "we are wrong" (exit 1) and "we are incomplete" (exit 10) reads this one
/// constant, so the two cannot drift apart.
pub const RC_UNSUPPORTED: u32 = 10;

/// What a finished run did, reduced to what the exit code depends on.
///
/// Built by [`crate::output::Tally`] while the stream is written, so the
/// decision below never re-walks the event stream.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct RunOutcome {
    /// `RunFinished.rc` of the last run in the stream.
    pub rc: u32,
    /// A block failed with a return code other than [`RC_UNSUPPORTED`].
    pub had_real_error: bool,
    /// A block failed with [`RC_UNSUPPORTED`].
    pub had_unsupported: bool,
    /// A block reported `ExecStatus::Interrupted`.
    pub interrupted: bool,
    /// The wall-clock budget ran out.
    pub timed_out: bool,
}

impl RunOutcome {
    /// §4.4's ladder, as one total function.
    ///
    /// The ordering is deliberate and is the whole reason this is not a `match`
    /// on `rc` alone. Under `--continue-on-error` a run can contain both an
    /// unimplemented construct and a genuine wrong answer, and `RunFinished.rc`
    /// carries only the last one. **"We are wrong" must win**: reporting exit 10
    /// for a run that also produced an incorrect number would let a fidelity
    /// regression be triaged as a missing feature, which is the exact confusion
    /// the two codes exist to prevent.
    #[must_use]
    pub fn exit_code(self) -> ExitCode {
        if self.timed_out {
            return ExitCode::Timeout;
        }
        if self.interrupted {
            return ExitCode::Interrupted;
        }
        if self.had_real_error {
            return ExitCode::RuntimeError;
        }
        if self.had_unsupported {
            return ExitCode::Unsupported;
        }
        match self.rc {
            0 => ExitCode::Success,
            RC_UNSUPPORTED => ExitCode::Unsupported,
            _ => ExitCode::RuntimeError,
        }
    }
}

/// `stratum <COMMAND> [OPTIONS]` — design 08 §4.1.
#[derive(Parser, Debug)]
#[command(
    name = "stratum",
    version,
    about = "Stratum — a Stata-compatible statistical engine, headless.",
    long_about = "The statistical engine runs without the GUI: for testing, CI, \
                  servers, automation, reproducibility and batch execution. The \
                  desktop app is a client of this same engine (spec §30).",
    disable_help_subcommand = true,
    propagate_version = true
)]
pub struct Cli {
    /// Log filter for the `tracing` subscriber. Always writes to **stderr**, so
    /// `--json` output on stdout stays machine-readable (CONTRACTS §7 g4).
    #[arg(long, global = true, value_name = "FILTER", env = "STRATUM_LOG")]
    pub log: Option<String>,

    #[command(subcommand)]
    pub command: Command,
}

/// The verbs. `difftest` is **deliberately absent**: it lives in
/// `cargo xtask difftest` only, so the shipped binary has no code path that can
/// invoke Stata (design 08 §9.6, spec §32).
#[derive(Subcommand, Debug)]
pub enum Command {
    /// Execute a do-file from a CLEAN state.
    Run(RunArgs),
    /// Execute commands from `-c`/stdin against a scratch state.
    Exec(ExecArgs),
    /// Serve the engine protocol over stdio (the desktop transport).
    Serve(ServeArgs),
    /// Static + reproducibility audit. No execution.
    Check(CheckArgs),
    /// Format do-files.
    Fmt(FmtArgs),
    /// Structural description of a `.do` or `.dta`.
    Describe(DescribeArgs),
    /// `.dta` utilities.
    Data(DataArgs),
    /// Scaffold a project.
    Init(InitArgs),
    /// Environment diagnostics; exit 0 iff healthy.
    Doctor(DoctorArgs),
    /// Version, build hash, target triple, features, allocator.
    Version(VersionArgs),
    /// Shell completion script.
    Completions(CompletionsArgs),
}

/// How a command that produces a stream renders it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, ValueEnum)]
pub enum Format {
    /// A faithful classic Stata log on stdout (spec §17).
    #[default]
    Text,
    /// CONTRACTS §7.1 NDJSON on stdout, nothing else.
    Json,
    /// Nothing on stdout. The exit code is the answer.
    Quiet,
}

/// Flags shared by `run` and `exec`. `exec` takes "the same flags as `run`"
/// (design 08 §4.1), so they are one type rather than two lists that drift.
#[derive(Args, Debug, Clone)]
pub struct ExecCommon {
    /// CONTRACTS §7.1 NDJSON on stdout. Shorthand for `--format json`.
    #[arg(long)]
    pub json: bool,

    /// Output rendering.
    #[arg(long, value_name = "F", value_enum)]
    pub format: Option<Format>,

    /// Apply the CONTRACTS §7.2 substitution table to the stream. Implies
    /// `--json`: there is nothing to normalise in a classic log.
    #[arg(long)]
    pub deterministic: bool,

    /// Also write a Stata-compatible text log here.
    #[arg(long, value_name = "PATH")]
    pub log_file: Option<Utf8PathBuf>,

    /// chdir before running. Defaults to the do-file's directory.
    #[arg(long, value_name = "DIR")]
    pub cd: Option<Utf8PathBuf>,

    /// Positional macros `1'`, `2', … (Stata `do file args`).
    #[arg(long, value_name = "ARGS", num_args = 1.., allow_hyphen_values = true)]
    pub args: Vec<String>,

    /// `set seed N` before the first block; recorded in `RunStarted.seed`.
    #[arg(long, value_name = "N")]
    pub seed: Option<u64>,

    /// Numeric-kernel threads. Default: physical cores.
    #[arg(long, value_name = "N")]
    pub threads: Option<u32>,

    /// Wall-clock budget in milliseconds; on expiry, SIGINT semantics, exit 8.
    #[arg(long, value_name = "MS")]
    pub timeout: Option<u64>,

    /// Refuse to run against more than N observations. A CI guard.
    #[arg(long, value_name = "N")]
    pub max_obs: Option<u64>,

    /// Do not stop at the first `r()`. Default is to stop, like `stata -b`.
    #[arg(long)]
    pub continue_on_error: bool,

    /// Write the final Stata return code here as decimal text.
    #[arg(long, value_name = "PATH")]
    pub rc_file: Option<Utf8PathBuf>,

    /// Skip `profile.do` / `sysprofile.do`.
    #[arg(long)]
    pub no_init: bool,

    /// Prepend to the ado search path.
    #[arg(long, value_name = "PATH", num_args = 1..)]
    pub ado: Vec<Utf8PathBuf>,

    /// Write a CaptureFile of terminal `r()`/`e()` state.
    #[arg(long, value_name = "PATH")]
    pub capture: Option<Utf8PathBuf>,

    /// Per-block timings to stderr as a table.
    #[arg(long)]
    pub profile: bool,

    /// Like `set trace on`.
    #[arg(long)]
    pub trace: bool,

    /// Drive the stream from a recorded engine capture instead of executing.
    ///
    /// The file is either CONTRACTS §10 event frames (what
    /// `tests/fixtures/mock/scenario_a.msgpack` holds) or §7.1 NDJSON; the
    /// reader sniffs which. This is the CLI half of R2 "mock-first, not
    /// integration-last": a renderer, a CI pipeline or a bug report can drive
    /// the real writer over a real stream with no engine present, and W07's
    /// desktop `--mock` is the same idea on the other side of the pipe.
    #[arg(long, value_name = "PATH")]
    pub replay: Option<Utf8PathBuf>,
}

impl ExecCommon {
    /// `--json` and `--format` resolved to one value. `--deterministic` implies
    /// JSON because §7.2's substitution table is defined over the NDJSON stream
    /// and over nothing else.
    #[must_use]
    pub fn resolved_format(&self) -> Format {
        if self.json || self.deterministic {
            return Format::Json;
        }
        self.format.unwrap_or_default()
    }
}

/// `stratum run <FILE.do>` — the primary verb.
///
/// **There is no `--dirty` flag, and there must never be one.** `run` is always
/// clean-state (fresh dataset, macros, estimates, frames); that is the CI and
/// reproducibility contract of design 08 §15/§16, and interactive execution
/// against live state is `serve`'s job. A `#[cfg(test)]` assertion below greps
/// the rendered help for the word, because the cheapest way for this to regress
/// is somebody adding a convenience flag.
#[derive(Args, Debug, Clone)]
pub struct RunArgs {
    /// The do-file to execute.
    #[arg(value_name = "FILE.do")]
    pub file: Utf8PathBuf,

    #[command(flatten)]
    pub common: ExecCommon,
}

/// `stratum exec -c '…'` — commands against a scratch state discarded at exit.
#[derive(Args, Debug, Clone)]
pub struct ExecArgs {
    /// A command to execute. Repeatable; state persists across occurrences
    /// within one invocation.
    #[arg(short = 'c', value_name = "CODE")]
    pub code: Vec<String>,

    /// Read commands from stdin.
    #[arg(long)]
    pub stdin: bool,

    #[command(flatten)]
    pub common: ExecCommon,
}

/// Which encoding `serve` speaks.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, ValueEnum)]
pub enum Protocol {
    /// Framed MessagePack (CONTRACTS §10). The desktop transport.
    #[default]
    Msgpack,
    /// CONTRACTS §7.1 NDJSON.
    Json,
}

/// `stratum serve` — the engine end of both transports.
#[derive(Args, Debug, Clone)]
pub struct ServeArgs {
    /// Wire encoding.
    #[arg(long, value_enum, default_value_t = Protocol::Msgpack)]
    pub protocol: Protocol,

    /// Explicitly select stdio. Accepted for symmetry; stdio is the only
    /// transport in v1.
    #[arg(long)]
    pub stdio: bool,

    /// Print the §7.1 method registry and exit.
    ///
    /// CONTRACTS §7.1 names this by name: "A client that wants the list of
    /// methods reads the generated `apps/desktop/src/ipc/types.ts` or `stratum
    /// serve --print-schema`."
    #[arg(long)]
    pub print_schema: bool,

    /// Do not install the parent-death watchdog. Only for running `serve` by
    /// hand from a shell that is not the engine's supervisor.
    #[arg(long)]
    pub no_watch_parent: bool,
}

/// Severity at or above which `check` fails with exit 5.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default, ValueEnum)]
pub enum DenyLevel {
    /// Any finding fails.
    Note,
    /// Warnings and errors fail.
    Warning,
    /// Only errors fail.
    #[default]
    Error,
    /// Nothing fails; report only.
    Never,
}

/// `stratum check <PATH>...` — deterministic checks only (design 08 §16).
#[derive(Args, Debug, Clone)]
pub struct CheckArgs {
    /// Files to audit.
    #[arg(value_name = "PATH", required = true)]
    pub paths: Vec<Utf8PathBuf>,

    /// Output rendering.
    #[arg(long, value_name = "F", value_enum, default_value_t = Format::Text)]
    pub format: Format,

    /// Shorthand for `--format json`.
    #[arg(long)]
    pub json: bool,

    /// Fail (exit 5) at or above this severity.
    #[arg(long, value_enum, default_value_t = DenyLevel::Error)]
    pub deny: DenyLevel,

    /// Treat warnings as errors.
    #[arg(long)]
    pub warn_as_error: bool,
}

/// `stratum fmt <PATH>...`
#[derive(Args, Debug, Clone)]
pub struct FmtArgs {
    /// Files to format.
    #[arg(value_name = "PATH", required = true)]
    pub paths: Vec<Utf8PathBuf>,

    /// Do not write; exit 6 if any file would change.
    #[arg(long)]
    pub check: bool,

    /// Write the formatted text to stdout instead of to the file.
    #[arg(long)]
    pub stdout: bool,
}

/// `stratum describe <PATH>...`
#[derive(Args, Debug, Clone)]
pub struct DescribeArgs {
    /// `.do` or `.dta` files.
    #[arg(value_name = "PATH", required = true)]
    pub paths: Vec<Utf8PathBuf>,

    /// Output rendering.
    #[arg(long, value_name = "F", value_enum, default_value_t = Format::Text)]
    pub format: Format,

    /// Shorthand for `--format json`.
    #[arg(long)]
    pub json: bool,
}

/// `stratum data <SUBCOMMAND>`
#[derive(Args, Debug, Clone)]
pub struct DataArgs {
    #[command(subcommand)]
    pub what: DataCommand,
}

/// `.dta` utilities (design 08 §4.1).
#[derive(Subcommand, Debug, Clone)]
pub enum DataCommand {
    /// Dump a dataset as delimited text.
    Cat {
        /// The `.dta` file.
        #[arg(value_name = "FILE.dta")]
        file: Utf8PathBuf,
    },
    /// Convert between `.dta` and other formats.
    Convert {
        /// Input file.
        #[arg(value_name = "IN")]
        input: Utf8PathBuf,
        /// Output file.
        #[arg(value_name = "OUT")]
        output: Utf8PathBuf,
    },
    /// Structural and value diff of two datasets.
    Diff {
        /// Left file.
        #[arg(value_name = "A")]
        left: Utf8PathBuf,
        /// Right file.
        #[arg(value_name = "B")]
        right: Utf8PathBuf,
    },
    /// First N observations.
    Head {
        /// The `.dta` file.
        #[arg(value_name = "FILE.dta")]
        file: Utf8PathBuf,
        /// How many observations.
        #[arg(short = 'n', long, default_value_t = 10)]
        count: u32,
    },
}

/// `stratum init [DIR]`
#[derive(Args, Debug, Clone)]
pub struct InitArgs {
    /// Project directory. Defaults to the current directory.
    #[arg(value_name = "DIR")]
    pub dir: Option<Utf8PathBuf>,

    /// Overwrite files that already exist.
    #[arg(long)]
    pub force: bool,
}

/// `stratum doctor`
#[derive(Args, Debug, Clone)]
pub struct DoctorArgs {
    /// Output rendering.
    #[arg(long, value_name = "F", value_enum, default_value_t = Format::Text)]
    pub format: Format,

    /// Shorthand for `--format json`.
    #[arg(long)]
    pub json: bool,
}

/// `stratum version`
#[derive(Args, Debug, Clone)]
pub struct VersionArgs {
    /// Output rendering.
    #[arg(long, value_name = "F", value_enum, default_value_t = Format::Text)]
    pub format: Format,

    /// Shorthand for `--format json`.
    #[arg(long)]
    pub json: bool,
}

/// `stratum completions <SHELL>`
#[derive(Args, Debug, Clone)]
pub struct CompletionsArgs {
    /// Which shell.
    #[arg(value_name = "SHELL", value_enum)]
    pub shell: clap_complete::Shell,
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    #[test]
    fn the_exit_ladder_is_contiguous_zero_to_ten_and_unique() {
        let codes: Vec<i32> = ExitCode::ALL.iter().map(|c| c.code()).collect();
        assert_eq!(codes, (0..=10).collect::<Vec<i32>>());
        // 126/127 and >=128 belong to the shell and to signals.
        assert!(codes.iter().all(|c| *c < 126));
    }

    #[test]
    fn ten_is_distinct_from_one_and_says_incomplete_rather_than_wrong() {
        assert_eq!(ExitCode::Unsupported.code(), 10);
        assert_eq!(ExitCode::RuntimeError.code(), 1);
        assert_ne!(ExitCode::Unsupported, ExitCode::RuntimeError);

        let incomplete = RunOutcome {
            rc: RC_UNSUPPORTED,
            had_unsupported: true,
            ..RunOutcome::default()
        };
        assert_eq!(incomplete.exit_code(), ExitCode::Unsupported);

        let wrong = RunOutcome {
            rc: 111,
            had_real_error: true,
            ..RunOutcome::default()
        };
        assert_eq!(wrong.exit_code(), ExitCode::RuntimeError);
    }

    /// The `--continue-on-error` case the ordering exists for: a run that hit
    /// both an unimplemented construct and a genuine `r(111)` is exit 1, no
    /// matter which one `RunFinished.rc` happens to carry.
    #[test]
    fn we_are_wrong_beats_we_are_incomplete() {
        for rc in [0, RC_UNSUPPORTED, 111] {
            let both = RunOutcome {
                rc,
                had_real_error: true,
                had_unsupported: true,
                ..RunOutcome::default()
            };
            assert_eq!(
                both.exit_code(),
                ExitCode::RuntimeError,
                "rc={rc}: a wrong answer must not be triaged as a missing feature"
            );
        }
    }

    #[test]
    fn interrupt_and_timeout_outrank_the_return_code() {
        let t = RunOutcome {
            rc: 111,
            had_real_error: true,
            timed_out: true,
            ..RunOutcome::default()
        };
        assert_eq!(t.exit_code(), ExitCode::Timeout);
        let i = RunOutcome {
            rc: 1,
            had_real_error: true,
            interrupted: true,
            ..RunOutcome::default()
        };
        assert_eq!(i.exit_code(), ExitCode::Interrupted);
    }

    #[test]
    fn a_clean_run_is_zero() {
        assert_eq!(RunOutcome::default().exit_code(), ExitCode::Success);
    }

    /// `run` is always clean-state (design 08 §4.1). The flag that would break
    /// that is the one somebody adds for convenience, so its absence is a test.
    #[test]
    fn run_has_no_dirty_flag() {
        let help = Cli::command()
            .find_subcommand_mut("run")
            .expect("run exists")
            .render_long_help()
            .to_string();
        assert!(
            !help.contains("--dirty"),
            "`stratum run` is ALWAYS clean-state; interactive execution against \
             live state is `serve`'s job. Help was:\n{help}"
        );
        for arg in Cli::command()
            .find_subcommand("run")
            .expect("run exists")
            .get_arguments()
        {
            assert_ne!(arg.get_id().as_str(), "dirty");
        }
    }

    #[test]
    fn deterministic_implies_json_because_7_2_is_defined_over_the_stream() {
        let cli = Cli::try_parse_from(["stratum", "run", "a.do", "--deterministic"]).unwrap();
        let Command::Run(args) = cli.command else {
            panic!("parsed the wrong subcommand")
        };
        assert_eq!(args.common.resolved_format(), Format::Json);
        assert!(args.common.deterministic);
    }

    #[test]
    fn text_is_the_default_and_json_is_opt_in() {
        let cli = Cli::try_parse_from(["stratum", "run", "a.do"]).unwrap();
        let Command::Run(args) = cli.command else {
            panic!("parsed the wrong subcommand")
        };
        assert_eq!(args.common.resolved_format(), Format::Text);
    }

    /// spec §32 / design 08 §9.6: the shipped binary must have no code path that
    /// can invoke Stata.
    #[test]
    fn there_is_no_difftest_subcommand() {
        assert!(Cli::command().find_subcommand("difftest").is_none());
        assert!(Cli::try_parse_from(["stratum", "difftest"]).is_err());
    }

    /// Design 08 §4.1's list, as a test. A verb that silently disappears from
    /// the surface is a §30 regression.
    #[test]
    fn every_verb_in_design_08_section_4_1_exists() {
        let cmd = Cli::command();
        for verb in [
            "run",
            "exec",
            "serve",
            "check",
            "fmt",
            "describe",
            "data",
            "init",
            "completions",
            "doctor",
            "version",
        ] {
            assert!(cmd.find_subcommand(verb).is_some(), "missing `{verb}`");
        }
    }

    #[test]
    fn clap_debug_asserts_hold_for_the_whole_surface() {
        Cli::command().debug_assert();
    }
}
