//! IMPLEMENTATION_PLAN W22 / design 08 §8.5 — the packaging smoke assertions,
//! as a program so a contributor can run exactly what `smoke.yml` runs.
//!
//! The one non-negotiable idea (A33 / Scenario E): every assertion here runs
//! against the PACKAGED artifact, not the build tree. "The bundle is fine but
//! the webview cannot load the frontend" is the single most common Tauri
//! packaging failure, it is invisible to every other gate in this repository,
//! and `selftest` below is what catches it.
//!
//! None of the `#[test]`s in this file launch a process or a window; the
//! process-running verbs execute only when a human or a workflow invokes them.

use anyhow::{bail, ensure, Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use clap::{Args, Subcommand};
use serde_json::Value;

use crate::Ctx;

#[derive(Args)]
pub struct Cmd {
    #[command(subcommand)]
    action: Action,
}

#[derive(Subcommand)]
enum Action {
    /// Compare an NDJSON event stream against an expected one, semantically:
    /// volatile fields are ignored, floats use the difftest tolerance, extra
    /// records on OUR side are ignored, missing records fail (08 §9.5).
    AssertStream {
        /// The stream the packaged `stratum run <case> --json` produced.
        ours: Utf8PathBuf,
        /// The committed expectation (tests/smoke/expected.jsonl).
        expected: Utf8PathBuf,
    },
    /// Launch a packaged desktop app with `--selftest` and propagate its exit
    /// code, cross-checked against the verdict it prints. Exit 0 means the
    /// engine half answered AND the webview loaded the bundled frontend and
    /// called `app_ready`.
    Selftest {
        /// A `Stratum.app` bundle or the inner binary itself.
        #[arg(value_name = "APP")]
        app: Utf8PathBuf,
        /// Seconds to wait before declaring the app hung (the binary's own
        /// webview deadline is 60 s).
        #[arg(long, default_value_t = 120)]
        timeout: u64,
    },
    /// Mount a .dmg, assert Stratum.app is inside, unmount (macOS).
    Dmg {
        #[arg(value_name = "DMG")]
        dmg: Utf8PathBuf,
    },
    /// After uninstall on Windows: fail if any Stratum registry key survived.
    AssertRegistryClean,
}

pub fn run(ctx: &Ctx, cmd: &Cmd) -> Result<()> {
    match &cmd.action {
        Action::AssertStream { ours, expected } => assert_stream(ours, expected),
        Action::Selftest { app, timeout } => selftest(app, *timeout),
        Action::Dmg { dmg } => dmg_mounts(ctx, dmg),
        Action::AssertRegistryClean => registry_clean(),
    }
}

// ---------------------------------------------------------------------------
// assert-stream

/// Field names whose VALUES are machine- or moment-specific by contract
/// (CONTRACTS §7.2 zeroes or rewrites exactly these under `--deterministic`).
/// The committed expectation is the deterministic form; the packaged binary
/// emits the raw form; equality is defined modulo this set.
const VOLATILE: &[&str] = &[
    "started_at_ms",
    "finished_at_ms",
    "duration_us",
    "cwd",
    "stratum_version",
    "run",
    "session",
];

/// Path-bearing fields are compared by final path component: the committed
/// expectation is the `--deterministic` form (CONTRACTS §7.2: paths relative
/// to the entry file), while the raw stream carries whatever the invocation
/// used — `tests/smoke/hello.do`, or an absolute path. `source` on run events
/// and `file` on diagnostics are exactly the fields §7.2 rewrites this way.
const PATH_FIELDS: &[&str] = &["source", "file"];

fn source_eq(a: &str, b: &str) -> bool {
    let tail = |s: &str| {
        s.rsplit(['/', '\\'])
            .next()
            .map(str::to_owned)
            .unwrap_or_else(|| s.to_owned())
    };
    tail(a) == tail(b)
}

/// The difftest default tolerance (08 §9.4): rel 1e-12, abs 1e-14. Integers
/// compare exactly.
fn number_eq(a: &serde_json::Number, b: &serde_json::Number) -> bool {
    if let (Some(x), Some(y)) = (a.as_i64(), b.as_i64()) {
        return x == y;
    }
    match (a.as_f64(), b.as_f64()) {
        (Some(x), Some(y)) => {
            if x == y {
                return true;
            }
            if x.is_nan() || y.is_nan() {
                return x.is_nan() && y.is_nan();
            }
            let diff = (x - y).abs();
            diff <= 1e-14 || diff <= 1e-12 * x.abs().max(y.abs())
        }
        _ => false,
    }
}

fn semantically_eq(ours: &Value, expected: &Value, path: &str, why: &mut String) -> bool {
    match (ours, expected) {
        (Value::Object(o), Value::Object(e)) => {
            for (k, ev) in e {
                if VOLATILE.contains(&k.as_str()) {
                    continue;
                }
                let sub = format!("{path}.{k}");
                if PATH_FIELDS.contains(&k.as_str()) {
                    if let (Some(a), Some(b)) = (o.get(k).and_then(Value::as_str), ev.as_str()) {
                        if source_eq(a, b) {
                            continue;
                        }
                    }
                }
                match o.get(k) {
                    None => {
                        *why = format!("{sub}: missing (expected {ev})");
                        return false;
                    }
                    Some(ov) => {
                        if !semantically_eq(ov, ev, &sub, why) {
                            return false;
                        }
                    }
                }
            }
            // Extra keys on our side are ignored — richer metadata must not
            // break the expectation (08 §9.5's asymmetry).
            true
        }
        (Value::Array(o), Value::Array(e)) => {
            if o.len() != e.len() {
                *why = format!("{path}: array length {} != expected {}", o.len(), e.len());
                return false;
            }
            for (i, (ov, ev)) in o.iter().zip(e).enumerate() {
                if !semantically_eq(ov, ev, &format!("{path}[{i}]"), why) {
                    return false;
                }
            }
            true
        }
        (Value::Number(a), Value::Number(b)) => {
            let ok = number_eq(a, b);
            if !ok {
                *why = format!("{path}: {a} != {b} (tol rel 1e-12 / abs 1e-14)");
            }
            ok
        }
        (a, b) => {
            let ok = a == b;
            if !ok {
                *why = format!("{path}: {a} != expected {b}");
            }
            ok
        }
    }
}

fn parse_stream(path: &Utf8Path) -> Result<Vec<Value>> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?;
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .enumerate()
        .map(|(i, l)| {
            serde_json::from_str(l).with_context(|| format!("{path}:{}: invalid JSON", i + 1))
        })
        .collect()
}

fn assert_stream(ours_path: &Utf8Path, expected_path: &Utf8Path) -> Result<()> {
    let ours = parse_stream(ours_path)?;
    let expected = parse_stream(expected_path)?;

    // Ordered subsequence match: every expected record must appear, in order;
    // records only we emit are skipped over.
    let mut oi = 0usize;
    for (ei, exp) in expected.iter().enumerate() {
        let mut matched = false;
        let mut nearest = String::new();
        while oi < ours.len() {
            let mut why = String::new();
            if semantically_eq(&ours[oi], exp, "$", &mut why) {
                matched = true;
                oi += 1;
                break;
            }
            // Same event kind but different content is almost always the real
            // failure; remember the best explanation instead of a bare miss.
            if nearest.is_empty() && ours[oi].pointer("/body/event") == exp.pointer("/body/event") {
                nearest = why;
            }
            oi += 1;
        }
        if !matched {
            let hint = if nearest.is_empty() {
                "no record of this event kind remained".to_owned()
            } else {
                nearest
            };
            bail!(
                "assert-stream: expected record {} (of {}) never matched: {hint}\n  expected: {exp}",
                ei + 1,
                expected.len()
            );
        }
    }
    println!(
        "assert-stream: OK ({} expected records matched, {} of ours inspected)",
        expected.len(),
        oi
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// selftest

fn inner_binary(app: &Utf8Path) -> Result<Utf8PathBuf> {
    if app.is_file() {
        return Ok(app.to_owned());
    }
    let mac = app.join("Contents/MacOS/stratum-desktop");
    if mac.is_file() {
        return Ok(mac);
    }
    bail!("{app} is neither a binary nor a .app bundle containing one");
}

/// The verdict lines `stratum-desktop --selftest` prints on stderr
/// (apps/desktop/src-tauri/src/main.rs). The exit status is the signal this
/// verb propagates; these are the cross-check on it. A host whose exit status
/// and printed verdict disagree has a bug in its exit path — exactly what
/// happened once, when `AppHandle::exit(1)` turned into process status 0 and
/// a FAILED selftest read as green — and that contradiction must fail the
/// gate rather than be resolved in the status's favour.
const VERDICT_OK: &str = "selftest OK";
const VERDICT_FAILED: &str = "selftest FAILED";

fn selftest(app: &Utf8Path, timeout_s: u64) -> Result<()> {
    use std::io::BufRead;

    let bin = inner_binary(app)?;
    println!("smoke selftest: launching {bin} --selftest");
    let mut child = std::process::Command::new(bin.as_std_path())
        .arg("--selftest")
        .stderr(std::process::Stdio::piped())
        .spawn()
        .with_context(|| format!("spawning {bin}"))?;

    // Relay the app's stderr line by line — the CI log must still show what
    // the host said — and keep a copy for the verdict cross-check below.
    let stderr = child.stderr.take().context("the app's stderr was piped")?;
    let relay = std::thread::spawn(move || -> Vec<String> {
        let mut lines = Vec::new();
        for line in std::io::BufReader::new(stderr)
            .lines()
            .map_while(Result::ok)
        {
            eprintln!("{line}");
            lines.push(line);
        }
        lines
    });

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_s);
    let status = loop {
        if let Some(status) = child.try_wait().context("waiting on the app")? {
            break status;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            // The pipe closes with the child; the relay thread ends with it.
            let _ = relay.join();
            bail!("selftest hung: no exit within {timeout_s}s (webview deadlock?)");
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    };
    let log = relay
        .join()
        .map_err(|_| anyhow::anyhow!("the stderr relay thread panicked"))?;
    judge_selftest(status.success(), &status.to_string(), &log)?;
    println!("smoke selftest: OK (exit 0, verdict `{VERDICT_OK}` printed)");
    Ok(())
}

/// The selftest verdict: the exit status, cross-checked against what the host
/// printed. Pure, so it is unit-tested without launching anything.
fn judge_selftest(exited_ok: bool, status: &str, log: &[String]) -> Result<()> {
    let said_ok = log.iter().any(|l| l.contains(VERDICT_OK));
    let said_failed = log.iter().any(|l| l.contains(VERDICT_FAILED));

    ensure!(exited_ok, "selftest exited with {status}");
    ensure!(
        !said_failed,
        "selftest exited 0 but printed `{VERDICT_FAILED}`: the host's exit status contradicts \
         its own verdict — its exit path is lying, and a green here would be false"
    );
    ensure!(
        said_ok,
        "selftest exited 0 without printing `{VERDICT_OK}`: the webview half never reported \
         in — a pass must be stated, not inferred from a silent exit"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// dmg

fn dmg_mounts(ctx: &Ctx, dmg: &Utf8Path) -> Result<()> {
    if !cfg!(target_os = "macos") {
        bail!("smoke dmg uses hdiutil and runs on macOS only");
    }
    ensure!(dmg.is_file(), "{dmg} is not a file");
    let mount = ctx.path("target/smoke-dmg-mount");
    let _ = std::fs::remove_dir_all(&mount);
    std::fs::create_dir_all(&mount)?;

    let attach = std::process::Command::new("hdiutil")
        .args([
            "attach",
            "-nobrowse",
            "-readonly",
            "-mountpoint",
            mount.as_str(),
            dmg.as_str(),
        ])
        .output()
        .context("running hdiutil attach")?;
    ensure!(
        attach.status.success(),
        "hdiutil attach failed on {dmg}:\n{}",
        String::from_utf8_lossy(&attach.stderr)
    );

    let app = mount.join("Stratum.app");
    let ok = app.is_dir();
    let detach = std::process::Command::new("hdiutil")
        .args(["detach", mount.as_str()])
        .output()
        .context("running hdiutil detach")?;
    ensure!(ok, "{dmg} mounted but contains no Stratum.app");
    ensure!(
        detach.status.success(),
        "hdiutil detach failed:\n{}",
        String::from_utf8_lossy(&detach.stderr)
    );
    println!("smoke dmg: OK ({dmg} mounts and carries Stratum.app)");
    Ok(())
}

// ---------------------------------------------------------------------------
// registry

/// Exactly the keys `packaging/windows/file-assoc.nsh` writes. Uninstall must
/// delete these and nothing else; anything found here after uninstall is a
/// leftover (08 §6.3 "Uninstall deletes exactly these keys").
const REGISTRY_KEYS: &[&str] = &[
    r"HKCU\Software\Classes\Stratum.DoFile",
    r"HKCU\Software\Classes\Stratum.DtaFile",
    r"HKCU\Software\Stratum",
];

fn registry_clean() -> Result<()> {
    if !cfg!(target_os = "windows") {
        bail!("smoke assert-registry-clean runs on Windows only");
    }
    let mut leftovers = Vec::new();
    for key in REGISTRY_KEYS {
        let out = std::process::Command::new("reg")
            .args(["query", key])
            .output()
            .context("running reg query")?;
        if out.status.success() {
            leftovers.push(*key);
        }
    }
    // The OpenWithProgids VALUES live under keys other apps also use; query
    // the value, not the key.
    for (key, value) in [
        (
            r"HKCU\Software\Classes\.do\OpenWithProgids",
            "Stratum.DoFile",
        ),
        (
            r"HKCU\Software\Classes\.dta\OpenWithProgids",
            "Stratum.DtaFile",
        ),
    ] {
        let out = std::process::Command::new("reg")
            .args(["query", key, "/v", value])
            .output()
            .context("running reg query /v")?;
        if out.status.success() {
            leftovers.push(key);
        }
    }
    ensure!(
        leftovers.is_empty(),
        "registry not clean after uninstall: {leftovers:?}"
    );
    println!("assert-registry-clean: OK");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Value {
        serde_json::from_str(s).unwrap()
    }

    #[test]
    fn volatile_fields_do_not_fail_the_match() {
        let ours = v(
            r#"{"body":{"event":"run_started","seq":0,"cwd":"/Users/x/repo","started_at_ms":1755900000000,"stratum_version":"0.1.0","source":"tests/smoke/hello.do"}}"#,
        );
        let exp = v(
            r#"{"body":{"event":"run_started","seq":0,"cwd":"<cwd>","started_at_ms":0,"stratum_version":"<version>","source":"hello.do"}}"#,
        );
        let mut why = String::new();
        assert!(semantically_eq(&ours, &exp, "$", &mut why), "{why}");
    }

    #[test]
    fn a_wrong_rc_fails_the_match() {
        let ours = v(r#"{"body":{"event":"run_finished","seq":2,"rc":0}}"#);
        let exp = v(r#"{"body":{"event":"run_finished","seq":2,"rc":10}}"#);
        let mut why = String::new();
        assert!(!semantically_eq(&ours, &exp, "$", &mut why));
        assert!(why.contains("rc"), "diagnostic names the field: {why}");
    }

    #[test]
    fn floats_compare_by_tolerance_not_bytes() {
        let ours = v(r#"{"stat":0.30000000000000004}"#);
        let exp = v(r#"{"stat":0.3}"#);
        let mut why = String::new();
        assert!(semantically_eq(&ours, &exp, "$", &mut why), "{why}");
        // …but a real numeric difference is caught.
        let bad = v(r#"{"stat":0.31}"#);
        assert!(!semantically_eq(&bad, &exp, "$", &mut why));
    }

    #[test]
    fn extra_records_ours_ok_missing_records_fail() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let write = |name: &str, body: &str| -> Utf8PathBuf {
            let p = dir.path().join(name);
            let mut f = std::fs::File::create(&p).unwrap();
            writeln!(f, "{body}").unwrap();
            Utf8PathBuf::from_path_buf(p).unwrap()
        };
        let ours = write(
            "ours.jsonl",
            "{\"body\":{\"event\":\"run_started\",\"seq\":0}}\n\
             {\"body\":{\"event\":\"extra_only_we_emit\",\"seq\":1}}\n\
             {\"body\":{\"event\":\"run_finished\",\"seq\":2,\"rc\":0}}",
        );
        let exp_ok = write(
            "exp_ok.jsonl",
            "{\"body\":{\"event\":\"run_started\",\"seq\":0}}\n\
             {\"body\":{\"event\":\"run_finished\",\"seq\":2,\"rc\":0}}",
        );
        assert_stream(&ours, &exp_ok).unwrap();
        let exp_missing = write(
            "exp_missing.jsonl",
            "{\"body\":{\"event\":\"run_started\",\"seq\":0}}\n\
             {\"body\":{\"event\":\"diagnostic\",\"seq\":1}}\n\
             {\"body\":{\"event\":\"run_finished\",\"seq\":2,\"rc\":0}}",
        );
        assert!(assert_stream(&ours, &exp_missing).is_err());
    }

    fn lines(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    /// The one true pass: exit 0 AND the host said so.
    #[test]
    fn selftest_passes_only_when_status_and_verdict_agree() {
        let log = lines(&[
            "stratum-desktop: selftest: engine refused SessionOpen (...); continuing",
            "stratum-desktop: selftest OK (app_ready received)",
        ]);
        judge_selftest(true, "exit status: 0", &log).unwrap();
    }

    /// A non-zero status fails regardless of what was printed — the status is
    /// the signal the gate propagates.
    #[test]
    fn selftest_nonzero_status_fails() {
        let log =
            lines(&["stratum-desktop: selftest FAILED — the webview never invoked app_ready"]);
        let err = judge_selftest(false, "exit status: 1", &log).unwrap_err();
        assert!(err.to_string().contains("exit status: 1"), "{err}");
        // …and even a host that printed OK but exited non-zero is a failure.
        let log = lines(&["stratum-desktop: selftest OK (app_ready received)"]);
        assert!(judge_selftest(false, "exit status: 70", &log).is_err());
    }

    /// The false green that started this: FAILED on stderr, status 0. The
    /// contradiction fails the gate and the message names it.
    #[test]
    fn selftest_status_zero_with_failed_verdict_is_a_contradiction() {
        let log =
            lines(&["stratum-desktop: selftest FAILED — the webview never invoked app_ready"]);
        let err = judge_selftest(true, "exit status: 0", &log).unwrap_err();
        assert!(err.to_string().contains("contradicts"), "{err}");
    }

    /// A silent exit 0 is not a pass: the webview half must say it arrived.
    #[test]
    fn selftest_status_zero_without_ok_verdict_fails() {
        let err = judge_selftest(true, "exit status: 0", &[]).unwrap_err();
        assert!(err.to_string().contains("without printing"), "{err}");
    }

    /// The committed smoke expectation must stay parseable and carry the
    /// engine-gap diagnostic it documents (W08/W09's declared edge).
    #[test]
    fn committed_expected_stream_parses() {
        let root = Utf8Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .to_path_buf();
        let records = parse_stream(&root.join("tests/smoke/expected.jsonl")).unwrap();
        assert!(!records.is_empty());
        assert_eq!(
            records[0].pointer("/body/event"),
            Some(&Value::String("run_started".into()))
        );
    }
}
