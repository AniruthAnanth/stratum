//! `stratum data <SUBCOMMAND>` — `.dta` utilities (design 08 §4.1).
//!
//! **Every subcommand here is exit 10, and that is the honest answer today.**
//! Reading a `.dta` is `stratum-dta`'s job (work unit W03, ARCHITECTURE §5), and
//! that crate does not exist in the tree yet. The alternatives were both worse:
//! omitting the verbs would make `stratum data` a usage error (exit 2) and hide
//! from CI that the feature is planned and missing, and implementing a second
//! `.dta` reader here would put a competing implementation of the storage-type
//! ladder, the `%fmt` rules and the strL heap in the CLI — the exact duplication
//! A10 was raised about.
//!
//! Exit 10 says "we are incomplete". Exit 1 would say "we are wrong". A
//! Stata-compatibility project has to be able to tell CI which one it is
//! looking at, and that is the entire reason the two codes are separate
//! (ADR-016, IMPLEMENTATION_PLAN W09).

use std::io::Write;

use crate::cli::{DataArgs, DataCommand, ExitCode};
use crate::cmd::CmdError;

/// Named once so the message a user reads and the message a test asserts on
/// cannot drift.
pub const DTA_READER_ABSENT: &str =
    "the .dta reader (crates/stratum-dta, work unit W03) is not linked into this build";

/// `stratum data`.
///
/// # Errors
/// Always [`CmdError::Unsupported`] — see this module's header.
pub fn data(
    args: &DataArgs,
    _out: &mut impl Write,
    _err: &mut impl Write,
) -> Result<ExitCode, CmdError> {
    let verb = match &args.what {
        DataCommand::Cat { file } => format!("data cat {file}"),
        DataCommand::Convert { input, output } => format!("data convert {input} {output}"),
        DataCommand::Diff { left, right } => format!("data diff {left} {right}"),
        DataCommand::Head { file, count } => format!("data head -n {count} {file}"),
    };
    Err(CmdError::Unsupported(format!(
        "`{verb}`: {DTA_READER_ABSENT}"
    )))
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, Parser};

    use super::*;
    use crate::cli::{Cli, Command};

    fn go(argv: &[&str]) -> CmdError {
        let args = match Cli::try_parse_from(argv).expect("argv parses").command {
            Command::Data(a) => a,
            other => panic!("expected `data`, got {other:?}"),
        };
        data(&args, &mut Vec::new(), &mut Vec::new()).expect_err("W03 has not landed")
    }

    #[test]
    fn every_subcommand_is_incomplete_rather_than_wrong() {
        for argv in [
            vec!["stratum", "data", "cat", "auto.dta"],
            vec!["stratum", "data", "convert", "a.dta", "b.csv"],
            vec!["stratum", "data", "diff", "a.dta", "b.dta"],
            vec!["stratum", "data", "head", "auto.dta", "-n", "5"],
        ] {
            let e = go(&argv);
            assert_eq!(e.exit_code(), ExitCode::Unsupported, "{argv:?}");
            assert_ne!(e.exit_code(), ExitCode::RuntimeError);
            assert!(e.to_string().contains("stratum-dta"), "{e}");
        }
    }

    /// The verbs must EXIST, or their absence is a usage error (exit 2) and CI
    /// cannot tell "planned and missing" from "never specified".
    #[test]
    fn the_verbs_are_on_the_surface() {
        let cmd = Cli::command_for_update();
        let data = cmd.find_subcommand("data").expect("data exists");
        for sub in ["cat", "convert", "diff", "head"] {
            assert!(data.find_subcommand(sub).is_some(), "missing `data {sub}`");
        }
    }
}
