//! Case hygiene: the `goldens --lint` rules for `tests/difftest/cases/**`.
//!
//! Two rules, both from W23's acceptance:
//!
//! * **no case file over 256 KB** — a case is a probe, not a dataset; a case
//!   that big is smuggling its fixture instead of deriving it, and it will
//!   bloat every future re-capture diff;
//! * **every committed `stata.jsonl` is canonical** — sorted, deduplicated,
//!   LF, valid `CaptureRecord` lines ([`crate::capture::canonical_problem`]).
//!   The comparator is order-insensitive, so canonical order is free and
//!   makes a re-capture diff exactly the values that moved.
//!
//! Exposed as a library function (and as `stratum-difftest lint`) so W22's
//! `cargo xtask goldens --lint` can call the same check without owning a
//! second implementation.

use anyhow::{Context, Result};
use camino::{Utf8Path, Utf8PathBuf};

/// The 256 KB ceiling, verbatim from the acceptance bullet.
pub const MAX_CASE_FILE_BYTES: u64 = 256 * 1024;

/// What the lint saw — counters (ADR-017) plus every problem.
#[derive(Debug, Default)]
pub struct LintReport {
    /// Files examined.
    pub files: u32,
    /// Committed `stata.jsonl` captures checked for canonical form.
    pub captures: u32,
    /// Problems found, each `path: problem`.
    pub problems: Vec<String>,
}

impl LintReport {
    /// Clean?
    #[must_use]
    pub fn ok(&self) -> bool {
        self.problems.is_empty()
    }
}

/// Lint `tests/difftest/cases/**` under `root`. A missing cases directory is
/// an empty (green) report: the lint gets teeth as cases land.
///
/// # Errors
/// I/O failures only; findings go in the report.
pub fn run(root: &Utf8Path) -> Result<LintReport> {
    let mut report = LintReport::default();
    let cases = root.join("tests/difftest/cases");
    if !cases.exists() {
        return Ok(report);
    }
    let mut stack = vec![cases];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir).with_context(|| format!("read_dir {dir}"))?;
        for e in entries {
            let e = e?;
            let path = Utf8PathBuf::from_path_buf(e.path())
                .map_err(|p| anyhow::anyhow!("non-UTF-8 path {}", p.display()))?;
            if e.file_type()?.is_dir() {
                stack.push(path);
                continue;
            }
            report.files += 1;
            let len = e.metadata()?.len();
            if len > MAX_CASE_FILE_BYTES {
                report.problems.push(format!(
                    "{path}: {len} bytes exceeds the {MAX_CASE_FILE_BYTES}-byte case ceiling"
                ));
            }
            if path.file_name() == Some("stata.jsonl") {
                report.captures += 1;
                let text =
                    std::fs::read_to_string(&path).with_context(|| format!("read {path}"))?;
                if let Some(problem) = crate::capture::canonical_problem(&text) {
                    report.problems.push(format!("{path}: {problem}"));
                }
            }
        }
    }
    report.problems.sort();
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Utf8Path, rel: &str, content: &[u8]) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
        std::fs::write(p, content).expect("write");
    }

    #[test]
    fn oversized_and_uncanonical_files_are_both_reported() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = Utf8Path::from_path(tmp.path()).expect("utf8 tempdir");
        write(root, "tests/difftest/cases/ok/case.do", b"summarize mpg\n");
        write(
            root,
            "tests/difftest/cases/ok/golden/stata.jsonl",
            b"{\"kind\":\"scalar\",\"name\":\"r(N)\",\"value\":\"74\"}\n",
        );
        // Out of order: `r(mean)` sorts before `r(N)`? No — bytes: 'N' < 'm',
        // so putting mean first violates ascending byte order.
        write(
            root,
            "tests/difftest/cases/bad/golden/stata.jsonl",
            b"{\"kind\":\"scalar\",\"name\":\"r(mean)\",\"value\":\"1\"}\n{\"kind\":\"scalar\",\"name\":\"r(N)\",\"value\":\"74\"}\n",
        );
        write(
            root,
            "tests/difftest/cases/huge/case.do",
            &vec![b'*'; (MAX_CASE_FILE_BYTES + 1) as usize],
        );

        let r = run(root).expect("lint runs");
        assert_eq!(r.files, 4);
        assert_eq!(r.captures, 2);
        assert_eq!(r.problems.len(), 2, "{:?}", r.problems);
        assert!(r.problems.iter().any(|p| p.contains("ascending")));
        assert!(r.problems.iter().any(|p| p.contains("ceiling")));
    }

    #[test]
    fn an_absent_cases_tree_is_green_not_an_error() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = Utf8Path::from_path(tmp.path()).expect("utf8");
        let r = run(root).expect("lint runs");
        assert!(r.ok());
        assert_eq!(r.files, 0);
    }
}
