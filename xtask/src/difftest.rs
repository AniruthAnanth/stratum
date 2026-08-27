//! `cargo xtask difftest` — the Stata differential harness's entry point
//! (plan W23, spec §32, ADR-013).
//!
//! | invocation | needs Stata | outcome |
//! |---|---|---|
//! | `difftest --corpus` | no | compare regenerated output to the committed `tests/golden/stata18/` corpus; exit 0/1 |
//! | `difftest --selftest-perturb` | no | negative test: deliberately sabotage one case, exit 0 iff the harness caught it |
//! | `difftest --lint` | no | case hygiene: 256 KB ceiling, canonical `stata.jsonl` order |
//! | `difftest` (default) | opt-in | corpus, then live differential; **exit 77 (SKIP)** when no usable Stata exists |
//! | `difftest --live` | yes | live differential only; exit 77 without a usable Stata |
//!
//! Exit 77 is the automake SKIP convention: `stata-diff.yml` maps it to a
//! neutral job outcome, so "the oracle was absent" is visible as *did not
//! run* rather than laundered into green. Everything the normal build and CI
//! touch is the Stata-free rows; §8.6's machine checks (`xtask layering`,
//! `check-topology.sh stata-free-ci`) hold this file's crate out of every
//! default build and every other workflow.
//!
//! # This file contains no logic, on purpose
//!
//! It shells out to `cargo run -p stratum-difftest`, W25's `e2e.rs` pattern
//! and for the same two reasons: `cargo xtask difftest --corpus` must be the
//! *same execution* a contributor gets from the harness binary (not a second
//! code path that agrees by inspection), and `xtask/Cargo.toml` is W00's file
//! — this module may not add a dependency edge to `stratum-difftest` and
//! link it. Everything below uses what xtask already has: `clap`, `anyhow`,
//! `camino`, `std::process`.
//!
//! # Registration (three lines in W00's `main.rs`)
//!
//! Same anchors W25 documented: `mod difftest;` in the alphabetical module
//! list, the `Difftest(difftest::Cmd)` variant with its own doc line placed
//! BEFORE the next variant's `///` comment, and the dispatch arm. Applied
//! under the W25 precedent with the R0 crossing recorded in W23's return.

use std::process::Command;

use anyhow::{Context, Result};
use clap::Args;

use crate::Ctx;

#[derive(Args)]
pub struct Cmd {
    /// Corpus phase only: compare the regenerated runtime output against the
    /// committed Stata logs. Needs no Stata; this is the everyday loop.
    #[arg(long, conflicts_with_all = ["live", "lint", "selftest_perturb"])]
    corpus: bool,

    /// Live phase only: run `tests/difftest/cases/**` through a licensed
    /// Stata. Exits 77 (SKIP) when no usable Stata exists.
    #[arg(long, conflicts_with_all = ["lint", "selftest_perturb"])]
    live: bool,

    /// Restrict `--live` to these case names.
    #[arg(long, value_name = "NAME", requires = "live")]
    case: Vec<String>,

    /// The negative test: sabotage one case (one text byte, one scalar) and
    /// require the harness to report exactly those two mismatches.
    #[arg(long)]
    selftest_perturb: bool,

    /// Case hygiene for `tests/difftest/cases/**` (what `goldens --lint`
    /// wraps): the 256 KB ceiling and canonical `stata.jsonl` order.
    #[arg(long)]
    lint: bool,

    /// Print the harness invocation instead of running it.
    #[arg(long)]
    dry_run: bool,
}

/// Map the xtask flags onto the harness binary's subcommands.
fn harness_args(cmd: &Cmd) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "run".into(),
        "-q".into(),
        "-p".into(),
        "stratum-difftest".into(),
        "--".into(),
    ];
    if cmd.lint {
        args.push("lint".into());
    } else if cmd.selftest_perturb {
        args.push("corpus".into());
        args.push("--selftest-perturb".into());
    } else if cmd.corpus {
        args.push("corpus".into());
    } else if cmd.live {
        args.push("live".into());
        for c in &cmd.case {
            args.push("--case".into());
            args.push(c.clone());
        }
    } else {
        args.push("auto".into());
    }
    args
}

pub fn run(ctx: &Ctx, cmd: &Cmd) -> Result<()> {
    let args = harness_args(cmd);
    let mut c = Command::new("cargo");
    c.args(&args);
    c.current_dir(ctx.root.as_std_path());
    if cmd.dry_run {
        println!("cargo {}", args.join(" "));
        return Ok(());
    }
    let status = c.status().context("running the stratum-difftest harness")?;
    if status.success() {
        return Ok(());
    }
    // The harness's exit code IS its contract — 77 must reach CI unmangled so
    // stata-diff.yml can report SKIP as neutral, and 1 vs 2 distinguishes "the
    // numbers differ" from "the environment is broken". Re-raise it verbatim
    // rather than collapsing everything to xtask's generic failure.
    std::process::exit(status.code().unwrap_or(2));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd() -> Cmd {
        Cmd {
            corpus: false,
            live: false,
            case: vec![],
            selftest_perturb: false,
            lint: false,
            dry_run: true,
        }
    }

    #[test]
    fn the_default_is_the_full_differential() {
        assert_eq!(
            harness_args(&cmd()).last().map(String::as_str),
            Some("auto")
        );
    }

    #[test]
    fn corpus_and_selftest_map_to_the_corpus_subcommand() {
        let c = Cmd {
            corpus: true,
            ..cmd()
        };
        assert!(harness_args(&c).contains(&"corpus".to_owned()));
        let s = Cmd {
            selftest_perturb: true,
            ..cmd()
        };
        let args = harness_args(&s);
        assert!(args.contains(&"corpus".to_owned()));
        assert!(args.contains(&"--selftest-perturb".to_owned()));
    }

    #[test]
    fn live_cases_pass_through() {
        let c = Cmd {
            live: true,
            case: vec!["regress_ols".to_owned()],
            ..cmd()
        };
        let args = harness_args(&c);
        assert!(args.contains(&"live".to_owned()));
        assert!(args.contains(&"--case".to_owned()));
        assert!(args.contains(&"regress_ols".to_owned()));
    }

    #[test]
    fn every_mode_shells_to_the_one_harness_crate() {
        for c in [
            cmd(),
            Cmd {
                lint: true,
                ..cmd()
            },
            Cmd {
                corpus: true,
                ..cmd()
            },
        ] {
            let args = harness_args(&c);
            assert_eq!(args[0], "run");
            assert!(args.contains(&"stratum-difftest".to_owned()));
        }
    }
}
