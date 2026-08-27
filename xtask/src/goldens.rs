//! IMPLEMENTATION_PLAN W22 / design 08 §11.3 — golden hygiene lint.
//!
//! The goldens under `tests/difftest/cases/**` are recorded Stata behaviour:
//! they cannot be regenerated without a license, so they are committed — and
//! committed oracle output needs two mechanical guards this lint provides:
//!
//! - **size**: any case directory over 256 KB fails. Bulk data does not belong
//!   in a golden; a case that needs it sets `capture.data = false` and asserts
//!   on summary statistics (08 §11.3).
//! - **canonical order**: `golden/stata.jsonl` must be sorted by its records'
//!   `name` field (ties broken by the full line). Capture order in Stata is
//!   incidental; a canonical order makes golden diffs reviewable and makes
//!   "the same capture twice" produce the same bytes.
//!
//! W23 owns producing goldens; this lint only polices their shape, which is
//! why it lives with the release pipeline that gates on it.

use anyhow::{ensure, Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use clap::Args;
use serde_json::Value;

use crate::Ctx;

#[derive(Args)]
pub struct Cmd {
    /// Run the lint (the only verb; spelled out so `xtask goldens --lint`
    /// reads as what it does in CI logs).
    #[arg(long)]
    pub lint: bool,
}

const CASES: &str = "tests/difftest/cases";
const MAX_CASE_BYTES: u64 = 256 * 1024;

pub fn run(ctx: &Ctx, cmd: &Cmd) -> Result<()> {
    ensure!(cmd.lint, "nothing to do: pass --lint");
    let cases_dir = ctx.path(CASES);
    if !cases_dir.is_dir() {
        println!("goldens: skipped, {cases_dir} does not exist yet (W23 creates it)");
        return Ok(());
    }

    let mut cases: Vec<Utf8PathBuf> = Vec::new();
    for entry in cases_dir
        .read_dir_utf8()
        .with_context(|| format!("reading {cases_dir}"))?
    {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            cases.push(entry.into_path());
        }
    }
    cases.sort();

    let mut failures = Vec::new();
    for case in &cases {
        if let Err(e) = lint_case(case) {
            failures.push(format!("{case}: {e:#}"));
        }
    }
    ensure!(
        failures.is_empty(),
        "goldens lint failed for {} case(s):\n  {}",
        failures.len(),
        failures.join("\n  ")
    );
    println!("goldens lint: OK ({} cases)", cases.len());
    Ok(())
}

fn lint_case(case: &Utf8Path) -> Result<()> {
    let total = dir_size(case)?;
    ensure!(
        total <= MAX_CASE_BYTES,
        "case is {total} bytes (limit {MAX_CASE_BYTES}); goldens are text, not data — \
         set capture.data = false and assert on summaries instead"
    );
    let jsonl = case.join("golden/stata.jsonl");
    if jsonl.is_file() {
        check_canonical_order(&jsonl)?;
    }
    Ok(())
}

fn dir_size(dir: &Utf8Path) -> Result<u64> {
    let mut total = 0u64;
    for entry in dir
        .read_dir_utf8()
        .with_context(|| format!("reading {dir}"))?
    {
        let entry = entry?;
        let ft = entry.file_type()?;
        if ft.is_dir() {
            total += dir_size(entry.path())?;
        } else if ft.is_file() {
            total += entry.metadata()?.len();
        }
    }
    Ok(total)
}

/// Sort key of one golden record: (`name`, full line). Records without a
/// `name` sort by their full text — stable, if degenerate; the difftest
/// capture always names records, so an unnamed one stands out in review.
fn sort_key(line: &str) -> Result<(String, String)> {
    let value: Value = serde_json::from_str(line).context("invalid JSON")?;
    let name = value
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    Ok((name, line.to_owned()))
}

fn check_canonical_order(path: &Utf8Path) -> Result<()> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?;
    let mut prev: Option<(String, String)> = None;
    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let key = sort_key(line).with_context(|| format!("{path}:{}", i + 1))?;
        if let Some(p) = &prev {
            ensure!(
                *p <= key,
                "{path}:{}: not in canonical sorted order (`{}` after `{}`); \
                 sort records by their `name` field",
                i + 1,
                key.0,
                p.0
            );
        }
        prev = Some(key);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write(dir: &std::path::Path, rel: &str, content: &[u8]) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::File::create(&p)
            .unwrap()
            .write_all(content)
            .unwrap();
    }

    fn utf8(p: &std::path::Path) -> Utf8PathBuf {
        Utf8PathBuf::from_path_buf(p.to_path_buf()).unwrap()
    }

    #[test]
    fn sorted_goldens_pass() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "golden/stata.jsonl",
            b"{\"kind\":\"scalar\",\"name\":\"e(N)\",\"value\":74}\n\
              {\"kind\":\"scalar\",\"name\":\"e(r2)\",\"value\":0.21}\n",
        );
        write(dir.path(), "case.do", b"summarize price\n");
        lint_case(&utf8(dir.path())).unwrap();
    }

    #[test]
    fn unsorted_goldens_fail() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "golden/stata.jsonl",
            b"{\"name\":\"e(r2)\"}\n{\"name\":\"e(N)\"}\n",
        );
        assert!(lint_case(&utf8(dir.path())).is_err());
    }

    #[test]
    fn oversized_case_fails() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "golden/stata.log", &vec![b'x'; 300 * 1024]);
        let err = lint_case(&utf8(dir.path())).unwrap_err();
        assert!(format!("{err:#}").contains("limit"));
    }

    /// The committed corpus must already satisfy its own lint.
    #[test]
    fn committed_cases_pass_the_lint() {
        let root = Utf8Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let cases = root.join(CASES);
        if !cases.is_dir() {
            return;
        }
        for entry in cases.read_dir_utf8().unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                lint_case(entry.path()).unwrap_or_else(|e| panic!("{}: {e:#}", entry.path()));
            }
        }
    }
}
