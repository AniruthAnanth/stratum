//! The live half: run `tests/difftest/cases/**` through a real, licensed
//! Stata and compare the fresh capture against the regenerated side.
//!
//! Everything here degrades to **SKIP, never to green**: no binary, an
//! unlicensed binary, a hung binary — each produces [`Probe::Absent`] with
//! its reason, the caller exits [`crate::EXIT_SKIP`] (77), and CI reports the
//! job as neutral.
//!
//! # The four rules this module exists to enforce
//!
//! * **Never trust Stata's exit code.** `stata-mp -b` returns 0
//!   unconditionally — on `r(111)`, and even on an explicit `exit _rc`. The
//!   return code is parsed from the log (`crate::log::final_rc`), and the
//!   process status is consulted only for "did it launch".
//! * **Each case gets its own temp cwd.** Batch mode writes the log into
//!   `$PWD` named after the do-file's basename — not beside the do-file — so
//!   two cases sharing a directory would race on `driver.log`.
//! * **Only Stata's output is committed.** This module writes captures into
//!   temp directories and compares them in memory; nothing here ever writes
//!   under `tests/`.
//! * **No raw log line leaves this module.** The banner names the licensee
//!   (serial number, name, institution), and an expired licence writes
//!   *nothing but* the banner — exactly the log a SKIP reason quotes. Every
//!   excerpt that reaches a message ([`Probe::Absent`], a failed case's
//!   detail) is cut by `redacted_tail`, which runs
//!   [`crate::log::redact_licence`] over the whole log before choosing
//!   lines. `stata-diff.yml` prints those messages into CI logs.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use camino::{Utf8Path, Utf8PathBuf};

use crate::capture;
use crate::compare::Report;
use crate::corpus;
use crate::fixture::auto;
use crate::log;

/// A usable Stata installation.
#[derive(Clone, Debug)]
pub struct Stata {
    /// The batch-capable binary.
    pub bin: Utf8PathBuf,
}

/// What probing found.
#[derive(Debug)]
pub enum Probe {
    /// A Stata that actually ran a do-file to completion.
    Usable(Stata),
    /// No usable Stata, with the reason the skip message will carry.
    Absent(String),
}

/// Environment variable naming the Stata binary. `STATA_BIN` is what
/// `scripts/run-stata.sh` and `scripts/capture-golden.sh` honour already, so
/// the harness reads the same one.
pub const STATA_BIN_ENV: &str = "STATA_BIN";

/// How long one batch invocation may run before it is killed and treated as
/// unusable. Generous: the whole corpus took seconds on the capture machine,
/// but a licence dialog waiting forever must not hang CI.
const BATCH_TIMEOUT: Duration = Duration::from_secs(300);

/// Find and verify a Stata. Existence is not usability — an expired licence
/// launches and does nothing — so the probe runs a one-line do-file in a temp
/// cwd and requires the marker file it writes.
///
/// # Errors
/// I/O failures preparing the probe directory. "No Stata" is not an error;
/// it is [`Probe::Absent`].
pub fn probe(root: &Utf8Path) -> Result<Probe> {
    if !cfg!(unix) {
        return Ok(Probe::Absent(
            "live differential runs on unix hosts only in this version".to_owned(),
        ));
    }
    let Some(bin) = locate() else {
        let reason = match std::env::var(STATA_BIN_ENV) {
            Ok(v) => format!("{STATA_BIN_ENV}={v} is not an existing file"),
            Err(_) => format!(
                "no Stata binary found ({STATA_BIN_ENV} unset, no known install path present)"
            ),
        };
        return Ok(Probe::Absent(reason));
    };
    verify(root, bin)
}

/// The usability half of [`probe`]: run the one-line marker do-file through
/// `bin` and classify. Separate from locating so the classification — and
/// in particular what its [`Probe::Absent`] reason is allowed to quote — can
/// be tested against a stub binary without touching the process environment.
///
/// # Errors
/// I/O failures preparing the probe directory, as for [`probe`].
fn verify(root: &Utf8Path, bin: Utf8PathBuf) -> Result<Probe> {
    let dir = tempfile::tempdir().context("create probe tempdir")?;
    let cwd = Utf8Path::from_path(dir.path()).context("probe tempdir is not UTF-8")?;
    std::fs::write(
        cwd.join("probe.do"),
        "set more off\nfile open h using \"probe.ok\", write replace\n\
         file write h \"ok\"\nfile close h\n",
    )?;
    match run_batch(root, &bin, cwd, "probe.do") {
        Ok(_) if cwd.join("probe.ok").exists() => Ok(Probe::Usable(Stata { bin })),
        // An expired licence logs the banner and "Your license has expired"
        // and nothing else, so the tail IS the licence block: redacted_tail
        // keeps the verdict and drops the licensee (module header, rule 4).
        Ok(log_text) => Ok(Probe::Absent(format!(
            "{bin} launched but could not run a do-file (expired licence?); log tail: {:?}",
            redacted_tail(&log_text, 3)
        ))),
        Err(e) => Ok(Probe::Absent(format!("{bin}: {e:#}"))),
    }
}

/// The binary to try: `$STATA_BIN` verbatim when set, else the conventional
/// install locations, else `stata-mp`/`stata-se`/`stata` on `$PATH`.
fn locate() -> Option<Utf8PathBuf> {
    if let Ok(v) = std::env::var(STATA_BIN_ENV) {
        let p = Utf8PathBuf::from(v);
        return p.is_file().then_some(p);
    }
    let candidates = [
        "/Applications/Stata/StataMP.app/Contents/MacOS/stata-mp",
        "/Applications/Stata/StataSE.app/Contents/MacOS/stata-se",
        "/usr/local/stata18/stata-mp",
        "/usr/local/stata18/stata-se",
        "/usr/local/stata/stata-mp",
        "/usr/local/stata/stata-se",
    ];
    for c in candidates {
        let p = Utf8PathBuf::from(c);
        if p.is_file() {
            return Some(p);
        }
    }
    for name in ["stata-mp", "stata-se", "stata"] {
        if let Some(p) = which(name) {
            return Some(p);
        }
    }
    None
}

fn which(name: &str) -> Option<Utf8PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let cand = dir.join(name);
        if cand.is_file() {
            return Utf8PathBuf::from_path_buf(cand).ok();
        }
    }
    None
}

/// Run one do-file in `cwd` through `scripts/run-stata.sh`, wait (bounded),
/// and return the log text. The script — not this function — is the one
/// place that spells the batch invocation, so the workflow, the probe and
/// the cases all run Stata the same way.
///
/// # Errors
/// Spawn failure, timeout, or a missing log — all of which mean "not usable",
/// never "the numbers differ".
fn run_batch(root: &Utf8Path, bin: &Utf8Path, cwd: &Utf8Path, dofile: &str) -> Result<String> {
    let script = root.join("scripts/run-stata.sh");
    if !script.is_file() {
        bail!("{script} does not exist");
    }
    let mut child = Command::new("bash")
        .arg(script.as_std_path())
        .arg(dofile)
        .arg(cwd.as_std_path())
        .env(STATA_BIN_ENV, bin.as_std_path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("spawn bash scripts/run-stata.sh {dofile}"))?;

    let started = Instant::now();
    let status = loop {
        if let Some(s) = child.try_wait()? {
            break s;
        }
        if started.elapsed() > BATCH_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            bail!("timed out after {}s", BATCH_TIMEOUT.as_secs());
        }
        std::thread::sleep(Duration::from_millis(50));
    };

    // 77 from the script means it could not find/launch Stata at all. Any
    // OTHER status is deliberately ignored: the do-file's outcome is read
    // from the log, never from an exit code (module header).
    if status.code() == Some(crate::EXIT_SKIP) {
        bail!("run-stata.sh reported no usable Stata");
    }
    let log_name = format!("{}.log", dofile.strip_suffix(".do").unwrap_or(dofile));
    let log_path = cwd.join(log_name);
    std::fs::read_to_string(&log_path).with_context(|| format!("no log at {log_path}"))
}

/// The outcome of the live phase.
pub struct LiveOutcome {
    /// The comparison, across every case that ran.
    pub report: Report,
    /// Cases executed (an ADR-017 counter — asserted, not timed).
    pub cases_run: u32,
}

/// Run every case under `tests/difftest/cases/**` (or the named subset)
/// through `stata`, capture fresh, compare against the regenerated side.
///
/// # Errors
/// Environment problems: unreadable case tree, unwritable temp dirs, a Stata
/// that stopped launching mid-run. Numeric differences are report content.
pub fn run_live(root: &Utf8Path, stata: &Stata, only: &[String]) -> Result<LiveOutcome> {
    let cases_dir = root.join("tests/difftest/cases");
    let mut names: Vec<String> = Vec::new();
    for e in std::fs::read_dir(&cases_dir).with_context(|| format!("read {cases_dir}"))? {
        let e = e?;
        if e.file_type()?.is_dir() {
            names.push(e.file_name().to_string_lossy().into_owned());
        }
    }
    names.sort();
    if !only.is_empty() {
        names.retain(|n| only.iter().any(|o| o == n));
    }

    let a = auto();
    let mut report = Report::default();
    let mut cases_run = 0u32;
    for name in &names {
        let case_dir = cases_dir.join(name);
        let outcome = run_case(root, stata, &case_dir, name)?;
        cases_run += 1;
        report.counters.cases += 1;
        match outcome {
            CaseRun::Failed { rc, tail } => {
                report.mismatches.push(crate::compare::Mismatch {
                    case: name.clone(),
                    channel: "rc",
                    detail: format!("Stata ended with r({rc}); log tail: {tail:?}"),
                });
            }
            CaseRun::Captured(stata_records) => {
                let ours = corpus::regen(a, name);
                let our_records = capture::records_of(ours.kind, &ours.results);
                report.records(name, &stata_records, &our_records);
            }
        }
    }
    Ok(LiveOutcome { report, cases_run })
}

enum CaseRun {
    /// The do-file did not run clean; nothing to compare.
    Failed { rc: u32, tail: String },
    /// The fresh capture.
    Captured(Vec<stratum_proto::capture::CaptureRecord>),
}

/// One case, in its own temp cwd (module header). The layout mirrors what a
/// hand run would do: `prologue.do`, `ado/stratum_capture.ado` and `case.do`
/// copied in, a two-line `driver.do` running prologue then case, capture
/// written to `capture.jsonl` by the case itself.
fn run_case(root: &Utf8Path, stata: &Stata, case_dir: &Utf8Path, name: &str) -> Result<CaseRun> {
    let dir = tempfile::tempdir().with_context(|| format!("tempdir for case {name}"))?;
    let cwd = Utf8Path::from_path(dir.path()).context("case tempdir is not UTF-8")?;

    std::fs::copy(case_dir.join("case.do"), cwd.join("case.do"))
        .with_context(|| format!("{name}: copy case.do"))?;
    std::fs::copy(
        root.join("tests/difftest/prologue.do"),
        cwd.join("prologue.do"),
    )
    .context("copy prologue.do")?;
    let ado_dst = cwd.join("ado");
    std::fs::create_dir(&ado_dst)?;
    for e in std::fs::read_dir(root.join("tests/difftest/ado").as_std_path())? {
        let e = e?;
        std::fs::copy(
            e.path(),
            ado_dst.join(e.file_name().to_string_lossy().as_ref()),
        )?;
    }
    std::fs::write(
        cwd.join("driver.do"),
        "do \"prologue.do\"\ndo \"case.do\"\n",
    )?;

    let log_text = run_batch(root, &stata.bin, cwd, "driver.do")?;
    let rc = log::final_rc(&log_text);
    if rc != 0 {
        return Ok(CaseRun::Failed {
            rc,
            tail: redacted_tail(&log_text, 5),
        });
    }
    let cap_path = cwd.join("capture.jsonl");
    let cap_text = std::fs::read_to_string(&cap_path)
        .with_context(|| format!("{name}: ran clean but wrote no {cap_path}"))?;
    let records = capture::read(&cap_text).map_err(|e| anyhow::anyhow!("{name}: {e}"))?;
    Ok(CaseRun::Captured(records))
}

/// The last `n` lines of a log, joined with ` | `, for a message — the ONLY
/// way log text leaves this module (module header, rule 4). The whole log is
/// redacted before the cut, not after: the institution line under
/// `Licensed to:` is recognisable only by the line above it, and a tail that
/// starts on it would otherwise print it whole.
fn redacted_tail(text: &str, n: usize) -> String {
    let redacted = log::redact_licence(text);
    let lines: Vec<&str> = redacted.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join(" | ")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The stub proves the two live-half rules without any Stata: the fake
    /// binary EXITS 0 while its log says `r(111);` — and the harness must
    /// believe the log. Hermetic: a shell script pretending to be Stata, no
    /// external application, no window.
    #[test]
    #[cfg(unix)]
    fn the_log_outranks_the_exit_code() {
        let root = crate::repo_root();
        let dir = tempfile::tempdir().expect("tempdir");
        let cwd = Utf8Path::from_path(dir.path()).expect("utf8");

        // A fake stata: ignores its arguments, writes a failing log into the
        // batch cwd (the real convention: log lands in $PWD), exits 0 like
        // the real one.
        use std::os::unix::fs::PermissionsExt as _;
        let fake = cwd.join("fake-stata");
        std::fs::write(
            &fake,
            "#!/bin/sh\nprintf '. do driver.do\\nsomething broke\\nr(111);\\n' > driver.log\nexit 0\n",
        )
        .expect("write stub");
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).expect("chmod");

        std::fs::write(cwd.join("driver.do"), "di 1\n").expect("write do");
        let log_text = run_batch(&root, &fake, cwd, "driver.do").expect("stub ran");
        assert_eq!(log::final_rc(&log_text), 111, "rc comes from the log");
    }

    /// An expired licence launches, writes the licence block plus "Your
    /// license has expired" as the ENTIRE log, and exits 0 — so the SKIP
    /// reason's log tail is the licence block itself. The reason must still
    /// say why (the verdict line) and must not say who (serial, name,
    /// institution). Hermetic: a shell script, an invented licensee, no
    /// Stata, no window. Drives `verify` — the real probe minus `locate`,
    /// which reads the process environment tests share.
    #[test]
    #[cfg(unix)]
    fn an_expired_licence_skip_reason_names_no_one() {
        let root = crate::repo_root();
        let dir = tempfile::tempdir().expect("tempdir");
        let cwd = Utf8Path::from_path(dir.path()).expect("utf8");

        use std::os::unix::fs::PermissionsExt as _;
        let fake = cwd.join("fake-expired-stata");
        std::fs::write(
            &fake,
            "#!/bin/sh\n\
             printf 'Stata license: Single-user 4-core , expiring 22 Aug 2026\\n' > probe.log\n\
             printf 'Serial number: 501809\\n' >> probe.log\n\
             printf '  Licensed to: Ada Example\\n' >> probe.log\n\
             printf '               Example Institute of Technology\\n' >> probe.log\n\
             printf 'Your license has expired\\n' >> probe.log\n\
             exit 0\n",
        )
        .expect("write stub");
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).expect("chmod");

        let Probe::Absent(reason) = verify(&root, fake).expect("probe ran") else {
            panic!("a binary that writes no marker file is not usable");
        };
        assert!(reason.contains("expired licence?"), "{reason}");
        assert!(
            reason.contains("Your license has expired"),
            "verdict kept: {reason}"
        );
        assert!(
            reason.contains("Licensed to: [redacted]"),
            "label kept: {reason}"
        );
        for pii in ["501809", "Ada Example", "Example Institute", "expiring 22"] {
            assert!(
                !reason.contains(pii),
                "{pii:?} leaked into the SKIP reason: {reason}"
            );
        }
    }

    #[test]
    fn a_pointed_at_nothing_binary_is_absent_not_an_error() {
        // `locate` honours STATA_BIN verbatim; a path that is not a file is
        // simply no Stata. Exercised through `locate`'s parts rather than the
        // env-mutating whole (tests share a process).
        assert!(!Utf8Path::new("/nonexistent/stata-mp").is_file());
        assert!(which("definitely-not-a-real-binary-name").is_none());
    }
}
