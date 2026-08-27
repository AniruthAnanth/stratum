//! ARCHITECTURE §8.9 (amended, A8) and CONTRACTS §7.2 — drive the conformance
//! corpus and assert the properties §8.9 states about its output.
//!
//! §8.9 compares `stratum run <case> --json --deterministic`, never raw
//! `--json`, and asserts three things of it: the bytes are identical across
//! macOS, Windows and Linux; two consecutive clean runs on one machine are
//! identical *including tempnames*; and the output is unchanged at
//! `RAYON_NUM_THREADS ∈ {1, 2, 8}` (ADR-013). The first is inherently
//! cross-machine, so it is split into a producer (`--out`) and a comparator
//! (`--baseline`) that CI runs on two different runners; the other two are
//! local and run on every invocation.
//!
//! CONTRACTS §7.2 additionally declares `stratum run --json | xtask
//! normalize-ndjson` *equivalent* to `--deterministic`. Two implementations of
//! one substitution table drift apart silently unless something ties them
//! together, so every captured stream is also required to be a fixed point of
//! `normalize_ndjson::normalize_stream`: whatever the engine emitted, running
//! the normalizer over it must change nothing. That catches drift in either
//! direction with no second corpus to maintain.
//!
//! §8.9 names a SHA-256; this compares the bytes themselves, which is the
//! same assertion and can say *which line* moved.
//!
//! The corpus is W09's (`tests/conformance/**`) and `stratum-cli` is W09's too.
//! This module is only the driver, and says so when neither has landed.

use std::process::Command;

use anyhow::{Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use clap::Args;

use crate::{normalize_ndjson, Ctx};

#[derive(Args)]
pub struct Cmd {
    /// Corpus directory. Defaults to `tests/conformance`.
    #[arg(long, value_name = "DIR")]
    pub corpus: Option<Utf8PathBuf>,

    /// Write one `<case>.jsonl` per case here. This is what CI uploads and what
    /// `--baseline` reads back on another runner.
    #[arg(long, value_name = "DIR")]
    pub out: Option<Utf8PathBuf>,

    /// Compare every case against a directory written by an earlier `--out`,
    /// normally on a different OS. This is §8.9's cross-platform half.
    #[arg(long, value_name = "DIR")]
    pub baseline: Option<Utf8PathBuf>,

    /// Thread counts to cross-check. ADR-013 pins {1, 2, 8}; the first is also
    /// the count used for the repeat-run and baseline comparisons.
    #[arg(long, value_name = "N", value_delimiter = ',', default_value = "1,2,8")]
    pub threads: Vec<u32>,

    /// Run this executable instead of `cargo run -q -p stratum-cli --`.
    #[arg(long, value_name = "EXE")]
    pub stratum: Option<Utf8PathBuf>,

    /// Exit 0 when the corpus directory does not exist. CI gates the job on a
    /// preflight probe instead; this is for a local run before W09 lands.
    #[arg(long)]
    pub if_present: bool,
}

pub fn run(ctx: &Ctx, cmd: &Cmd) -> Result<()> {
    let corpus = cmd
        .corpus
        .clone()
        .unwrap_or_else(|| ctx.path("tests/conformance"));

    if !corpus.is_dir() {
        if cmd.if_present {
            println!("conformance: {corpus} does not exist yet (W09 owns it) — skipped");
            return Ok(());
        }
        anyhow::bail!(
            "corpus {corpus} does not exist. W09 owns `tests/conformance/**`; \
             pass --if-present to make its absence a skip rather than a failure"
        );
    }
    anyhow::ensure!(
        !cmd.threads.is_empty(),
        "--threads needs at least one value"
    );

    let cases = cases(&corpus)?;
    anyhow::ensure!(!cases.is_empty(), "no `*.do` case in {corpus}");

    if let Some(dir) = &cmd.out {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {dir}"))?;
    }

    let mut failures: Vec<String> = Vec::new();
    let pinned = cmd.threads[0];

    for case in &cases {
        let name = case
            .file_stem()
            .with_context(|| format!("case {case} has no file stem"))?;
        let first = capture(ctx, cmd, case, pinned)?;

        // CONTRACTS §7.2 — `--deterministic` and the normalizer are one rule.
        let renormalized = normalize_ndjson::normalize_stream(&first, None)
            .with_context(|| format!("normalizing {name}"))?;
        if renormalized != first {
            failures.push(format!(
                "{name}: `--deterministic` output is not a fixed point of \
                 `xtask normalize-ndjson`, so the two implementations of \
                 CONTRACTS §7.2 disagree; {}",
                first_difference(&first, &renormalized)
            ));
        }

        // §8.9 — two consecutive clean runs, including tempnames.
        let repeat = capture(ctx, cmd, case, pinned)?;
        if repeat != first {
            failures.push(format!(
                "{name}: two consecutive runs at RAYON_NUM_THREADS={pinned} differ; {}",
                first_difference(&first, &repeat)
            ));
        }

        // ADR-013 — the map/reduce split must not leak the thread count.
        for &n in &cmd.threads[1..] {
            let other = capture(ctx, cmd, case, n)?;
            if other != first {
                failures.push(format!(
                    "{name}: RAYON_NUM_THREADS={n} differs from {pinned}; {}",
                    first_difference(&first, &other)
                ));
            }
        }

        if let Some(dir) = &cmd.baseline {
            let path = dir.join(format!("{name}.jsonl"));
            match std::fs::read_to_string(path.as_std_path()) {
                Ok(want) if want != first => failures.push(format!(
                    "{name}: differs from the baseline at {path}; {}",
                    first_difference(&want, &first)
                )),
                Ok(_) => {}
                Err(e) => failures.push(format!("{name}: reading baseline {path}: {e}")),
            }
        }

        if let Some(dir) = &cmd.out {
            let path = dir.join(format!("{name}.jsonl"));
            std::fs::write(path.as_std_path(), &first)
                .with_context(|| format!("writing {path}"))?;
        }
    }

    if !failures.is_empty() {
        for f in &failures {
            eprintln!("conformance: {f}");
        }
        anyhow::bail!(
            "{} conformance mismatch(es) over {} case(s)",
            failures.len(),
            cases.len()
        );
    }

    println!(
        "conformance: OK — {} case(s), identical across {} thread count(s){}",
        cases.len(),
        cmd.threads.len(),
        if cmd.baseline.is_some() {
            " and against the baseline"
        } else {
            ""
        }
    );
    Ok(())
}

/// `*.do` directly under `dir`, sorted, so the report order does not depend on
/// the filesystem's readdir order and two runners agree on it.
fn cases(dir: &Utf8Path) -> Result<Vec<Utf8PathBuf>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir.as_std_path()).with_context(|| format!("reading {dir}"))? {
        let path = entry.with_context(|| format!("reading {dir}"))?.path();
        let Ok(path) = Utf8PathBuf::from_path_buf(path) else {
            continue;
        };
        if path.is_file() && path.extension() == Some("do") {
            out.push(path);
        }
    }
    out.sort();
    Ok(out)
}

fn capture(ctx: &Ctx, cmd: &Cmd, case: &Utf8Path, threads: u32) -> Result<String> {
    let mut child = match &cmd.stratum {
        Some(exe) => Command::new(exe),
        None => {
            // `CARGO` is set when this runs under `cargo xtask`, and is the
            // toolchain that built us — reusing it keeps the corpus on the
            // pinned rustc rather than whatever `cargo` resolves to on PATH.
            let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
            let mut c = Command::new(cargo);
            c.args(["run", "-q", "-p", "stratum-cli", "--"]);
            c
        }
    };
    child
        .current_dir(ctx.root.as_std_path())
        .args(["run", case.as_str(), "--json", "--deterministic"])
        .env("RAYON_NUM_THREADS", threads.to_string());

    let out = child
        .output()
        .with_context(|| format!("running the conformance case {case}"))?;
    anyhow::ensure!(
        out.status.success(),
        "{case} exited {}:\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr).trim()
    );
    String::from_utf8(out.stdout)
        .with_context(|| format!("{case} emitted non-UTF-8; CONTRACTS §7.1 requires UTF-8"))
}

/// Name the line that moved. A conformance stream is megabytes of NDJSON and a
/// full diff in CI logs is unreadable; the first divergent line is the whole
/// diagnosis in practice, because everything after it is downstream of it.
fn first_difference(a: &str, b: &str) -> String {
    let (mut la, mut lb) = (a.lines(), b.lines());
    let mut n = 0usize;
    loop {
        n += 1;
        match (la.next(), lb.next()) {
            (Some(x), Some(y)) if x == y => {}
            (Some(x), Some(y)) => {
                return format!(
                    "first difference at line {n}:\n    - {}\n    + {}",
                    cut(x),
                    cut(y)
                )
            }
            (None, None) => return "the streams are identical".to_owned(),
            (Some(x), None) => {
                return format!(
                    "the second stream ends at line {n}, the first has `{}`",
                    cut(x)
                )
            }
            (None, Some(y)) => {
                return format!(
                    "the first stream ends at line {n}, the second has `{}`",
                    cut(y)
                )
            }
        }
    }
}

const CUT: usize = 160;

fn cut(line: &str) -> String {
    if line.chars().count() <= CUT {
        return line.to_owned();
    }
    let head: String = line.chars().take(CUT).collect();
    format!("{head}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_divergent_line_is_named() {
        let a = "one\ntwo\nthree\n";
        let b = "one\nTWO\nthree\n";
        let d = first_difference(a, b);
        assert!(d.contains("line 2"), "{d}");
        assert!(d.contains("- two") && d.contains("+ TWO"), "{d}");
    }

    #[test]
    fn a_truncated_stream_is_reported_as_a_length_difference() {
        assert!(first_difference("a\nb\n", "a\n").contains("second stream ends at line 2"));
        assert!(first_difference("a\n", "a\nb\n").contains("first stream ends at line 2"));
        assert_eq!(first_difference("a\n", "a\n"), "the streams are identical");
    }

    #[test]
    fn a_long_line_is_cut_rather_than_dumped() {
        let long = "x".repeat(CUT * 2);
        let d = first_difference(&format!("{long}\n"), "y\n");
        assert!(d.contains('…'), "{d}");
        assert!(d.len() < long.len(), "{d}");
    }

    #[test]
    fn cases_are_the_do_files_in_sorted_order() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = Utf8Path::from_path(dir.path()).expect("utf-8 tempdir");
        for f in ["b.do", "a.do", "notes.txt", "c.dta"] {
            std::fs::write(root.join(f), "").expect("write");
        }
        std::fs::create_dir(root.join("sub.do")).expect("mkdir");

        let found: Vec<String> = cases(root)
            .expect("read")
            .iter()
            .map(|p| p.file_name().unwrap_or_default().to_owned())
            .collect();
        assert_eq!(found, vec!["a.do".to_owned(), "b.do".to_owned()]);
    }

    /// Before W09 lands there is no corpus and no `stratum-cli`. That must be a
    /// loud failure by default and a skip only when asked for, so a green CI
    /// run can never mean "nothing ran".
    #[test]
    fn an_absent_corpus_fails_unless_if_present() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = Utf8Path::from_path(dir.path()).expect("utf-8 tempdir");
        let ctx = Ctx {
            root: root.to_path_buf(),
        };
        let mut cmd = Cmd {
            corpus: Some(root.join("tests/conformance")),
            out: None,
            baseline: None,
            threads: vec![1],
            stratum: None,
            if_present: false,
        };
        assert!(run(&ctx, &cmd).is_err());
        cmd.if_present = true;
        run(&ctx, &cmd).expect("--if-present skips");
    }
}
