//! `stratum-difftest` — the harness binary `cargo xtask difftest` wraps.
//!
//! Exit codes are the whole contract:
//!
//! | code | meaning |
//! |---|---|
//! | 0  | everything that ran compared clean |
//! | 1  | a comparison ran and found differences |
//! | 2  | environment/usage error (missing corpus, malformed rules) |
//! | 77 | the live differential could not run: no usable Stata (SKIP) |
//!
//! The default mode runs the corpus phase (no Stata needed) and then the
//! live phase; with no usable Stata the live phase becomes exit 77 so a
//! green-looking run can never quietly mean "nothing was compared against a
//! real Stata". `--corpus` alone is the everyday, Stata-free invocation and
//! exits 0 on a clean corpus.

use clap::{Parser, Subcommand};
use stratum_difftest::{capture, corpus, lint, stata, EXIT_DIFF, EXIT_ERR, EXIT_SKIP};

#[derive(Parser)]
#[command(
    name = "stratum-difftest",
    about = "Stata differential-test harness (spec §32): corpus comparison without Stata, live differential with one",
    version
)]
struct Cli {
    /// Repository root (defaults to the workspace this binary was built in).
    #[arg(long, global = true, value_name = "DIR")]
    root: Option<camino::Utf8PathBuf>,

    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Compare the regenerated runtime output against the committed Stata
    /// corpus (`tests/golden/stata18/`). Needs no Stata; exit 0/1.
    Corpus {
        /// Deliberately sabotage one case and require the harness to catch
        /// it: exit 0 iff BOTH perturbations (one text byte, one scalar)
        /// are reported. The negative test, runnable from the shell.
        #[arg(long)]
        selftest_perturb: bool,
    },
    /// Run `tests/difftest/cases/**` through a licensed Stata and compare
    /// fresh captures. Exit 77 (SKIP) when no usable Stata exists.
    Live {
        /// Run only these case names.
        #[arg(long, value_name = "NAME")]
        case: Vec<String>,
    },
    /// Corpus, then live: the full differential. Live degrades to exit 77.
    Auto,
    /// Hygiene for `tests/difftest/cases/**`: the 256 KB ceiling and
    /// canonical `stata.jsonl` ordering. What `xtask goldens --lint` calls.
    Lint,
    /// Rewrite a capture NDJSON file into canonical form (sorted, LF,
    /// deduplicated). For a capture machine to run after `live`.
    Canon {
        /// The capture file to canonicalize in place.
        file: camino::Utf8PathBuf,
    },
}

fn main() {
    let cli = Cli::parse();
    let root = cli.root.clone().unwrap_or_else(stratum_difftest::repo_root);
    let code = match cli.cmd.unwrap_or(Cmd::Auto) {
        Cmd::Corpus { selftest_perturb } => run_corpus(&root, selftest_perturb),
        Cmd::Live { case } => run_live(&root, &case),
        Cmd::Auto => match run_corpus(&root, false) {
            0 => run_live(&root, &[]),
            n => n,
        },
        Cmd::Lint => run_lint(&root),
        Cmd::Canon { file } => run_canon(&file),
    };
    std::process::exit(code);
}

fn run_corpus(root: &camino::Utf8Path, selftest: bool) -> i32 {
    let perturb = if selftest {
        corpus::Perturb::Deliberate
    } else {
        corpus::Perturb::None
    };
    let report = match corpus::run(root, perturb) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("difftest corpus: {e:#}");
            return EXIT_ERR;
        }
    };
    let c = report.counters;
    println!(
        "difftest corpus: {} case(s), {} text block(s) ({} bytes), {} listing(s), \
         {} scalar(s), {} macro(s), {} matrix shape(s), {} function name(s)",
        c.cases,
        c.text_blocks,
        c.text_bytes,
        c.listings,
        c.scalars,
        c.macros,
        c.matrices,
        c.functions
    );
    if selftest {
        // The negative test: the sabotage MUST be caught — one flipped byte
        // of classic text and one nudged scalar, two mismatches, exactly.
        return if report.mismatches.len() == 2 {
            println!("difftest corpus: selftest OK — both deliberate perturbations were caught");
            0
        } else {
            eprintln!(
                "difftest corpus: SELFTEST FAILED — expected exactly 2 caught perturbations, got {}:",
                report.mismatches.len()
            );
            for m in &report.mismatches {
                eprintln!("  {m}");
            }
            EXIT_DIFF
        };
    }
    if report.ok() {
        println!("difftest corpus: OK");
        0
    } else {
        eprintln!("difftest corpus: {} mismatch(es):", report.mismatches.len());
        for m in &report.mismatches {
            eprintln!("  {m}");
        }
        EXIT_DIFF
    }
}

fn run_live(root: &camino::Utf8Path, only: &[String]) -> i32 {
    let install = match stata::probe(root) {
        Ok(stata::Probe::Usable(s)) => s,
        Ok(stata::Probe::Absent(reason)) => {
            // SKIP is the honest outcome: nothing was compared against a live
            // Stata, and the exit code says so. CI maps 77 to neutral.
            println!("difftest live: SKIP — {reason}");
            return EXIT_SKIP;
        }
        Err(e) => {
            eprintln!("difftest live: {e:#}");
            return EXIT_ERR;
        }
    };
    println!("difftest live: using {}", install.bin);
    match stata::run_live(root, &install, only) {
        Ok(out) => {
            println!(
                "difftest live: {} case(s), {} record(s) compared",
                out.cases_run, out.report.counters.records
            );
            if out.report.ok() {
                println!("difftest live: OK");
                0
            } else {
                eprintln!(
                    "difftest live: {} mismatch(es):",
                    out.report.mismatches.len()
                );
                for m in &out.report.mismatches {
                    eprintln!("  {m}");
                }
                EXIT_DIFF
            }
        }
        Err(e) => {
            eprintln!("difftest live: {e:#}");
            EXIT_ERR
        }
    }
}

fn run_lint(root: &camino::Utf8Path) -> i32 {
    match lint::run(root) {
        Ok(r) if r.ok() => {
            println!(
                "difftest lint: OK — {} file(s), {} committed capture(s)",
                r.files, r.captures
            );
            0
        }
        Ok(r) => {
            eprintln!("difftest lint: {} problem(s):", r.problems.len());
            for p in &r.problems {
                eprintln!("  {p}");
            }
            EXIT_DIFF
        }
        Err(e) => {
            eprintln!("difftest lint: {e:#}");
            EXIT_ERR
        }
    }
}

fn run_canon(file: &camino::Utf8Path) -> i32 {
    let go = || -> anyhow::Result<bool> {
        let text = std::fs::read_to_string(file)?;
        let records = capture::read(&text).map_err(|e| anyhow::anyhow!("{file}: {e}"))?;
        let canon = capture::canonicalize(&records)?;
        let changed = canon != text;
        if changed {
            std::fs::write(file, canon)?;
        }
        Ok(changed)
    };
    match go() {
        Ok(true) => {
            println!("difftest canon: rewrote {file}");
            0
        }
        Ok(false) => {
            println!("difftest canon: {file} already canonical");
            0
        }
        Err(e) => {
            eprintln!("difftest canon: {e:#}");
            EXIT_ERR
        }
    }
}
