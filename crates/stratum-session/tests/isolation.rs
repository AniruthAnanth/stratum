//! `Isolation::Subprocess` and the write sandbox.
//!
//! ARCHITECTURE §7.7: a real `stratum run --clean` child is the **only** thing
//! that may tick "✓ File runs from clean state". Two properties make that claim
//! honest, and they are tested separately because they fail separately:
//!
//! * the child is really spawned, its NDJSON is really *streamed* rather than
//!   buffered until exit, and it is really reaped; and
//! * every write verb lands inside the scratch directory, per verb.
//!
//! # The streaming proof has no clock in it
//!
//! ADR-017 forbids asserting a duration. So `streams_rather_than_buffering`
//! is a **handshake**: the child writes its first line and then blocks until the
//! *sink* — running in this process, inside `CleanRun::stream` — creates a file.
//! If the implementation buffered the child's output until exit, the child would
//! be waiting for a sink that is waiting for EOF, and the test would fail on the
//! child's own bounded timeout with a `TIMEOUT` record rather than hang. Nothing
//! is timed; the assertion is on which records arrived.

use camino::{Utf8Path, Utf8PathBuf};
use stratum_session::isolate::{
    clean_state_tick, CleanRun, CleanRunOutcome, Isolation, Sandboxed, WriteSandbox, WriteVerb,
};

/// The four spawn tests stand in a `#[cfg(unix)]` module.
///
/// The child they drive is a `/bin/sh` script, because W09's `stratum run` does
/// not exist yet and a verification run needs *something* real to spawn, stream
/// and reap. The logic under test — argv, streaming, reaping, exit codes — is
/// platform-independent and lives in `isolate.rs`; only the fixture is POSIX.
/// The Windows fixture belongs with W25's Tier-2 harness, which is where a real
/// `stratum.exe` first exists. Reported in W08b's return.
#[cfg(unix)]
mod child {
    use std::fs;
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    use camino::{Utf8Path, Utf8PathBuf};
    use stratum_session::isolate::CleanRun;
    use tempfile::TempDir;

    fn utf8(dir: &TempDir) -> Utf8PathBuf {
        Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).expect("tempdir path is UTF-8")
    }

    /// Write an executable `/bin/sh` script and return its path.
    fn script(dir: &Utf8Path, name: &str, body: &str) -> Utf8PathBuf {
        let path = dir.join(name);
        let mut f = fs::File::create(&path).expect("creating the fake stratum binary");
        write!(f, "#!/bin/sh\n{body}").expect("writing the fake stratum binary");
        drop(f);
        let mut perms = fs::metadata(&path).expect("stat").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&path, perms).expect("chmod +x");
        path
    }
    #[test]
    fn spawns_streams_and_reaps_a_child() {
        let dir = TempDir::new().expect("tempdir");
        let root = utf8(&dir);
        let bin = script(
            &root,
            "stratum",
            r#"echo "$@" > "$(dirname "$0")/argv.txt"
    pwd > "$(dirname "$0")/cwd.txt"
    echo '{"v":1,"t":"ev","corr":0,"body":{"run_started":{}}}'
    echo '{"v":1,"t":"ev","corr":0,"body":{"block_finished":{}}}'
    echo '{"v":1,"t":"ev","corr":0,"body":{"run_finished":{}}}'
    echo 'loading auto.dta' >&2
    exit 0
    "#,
        );
        let entry = root.join("analysis.do");
        fs::write(&entry, "sysuse auto\n").expect("writing the entry file");

        let run = CleanRun::verify(&bin, &entry, root.join("scratch"));
        let mut lines = Vec::new();
        let out = run
            .stream(|l| lines.push(l.to_owned()))
            .expect("the child spawns, streams and reaps");

        // Counters, not durations (ADR-017).
        assert_eq!(out.lines, 3, "one NDJSON record per line, none swallowed");
        assert_eq!(lines.len(), 3);
        assert!(out.reaped, "wait() returned: the child is not a zombie");
        assert_eq!(out.code, Some(0));
        assert!(out.success());
        assert!(lines[0].contains("run_started"));
        assert!(lines[2].contains("run_finished"));
        assert_eq!(
            out.bytes,
            lines.iter().map(|l| l.len() as u64 + 1).sum::<u64>()
        );

        // stdout is NDJSON only; chatter is on stderr and is captured rather than
        // inherited, so a clean run cannot scribble on the engine's own log.
        assert!(out.stderr.contains("loading auto.dta"));
        assert!(!lines.iter().any(|l| l.contains("loading auto.dta")));

        // The child really received the flags, rather than merely being described
        // by `command_line()`.
        let argv = fs::read_to_string(root.join("argv.txt")).expect("the child wrote its argv");
        assert!(argv.contains("--clean"), "argv was: {argv}");
        assert!(argv.contains("--sandbox"), "argv was: {argv}");
        assert!(argv.contains("--json"), "argv was: {argv}");
    }

    #[test]
    fn streams_rather_than_buffering_until_exit() {
        let dir = TempDir::new().expect("tempdir");
        let root = utf8(&dir);
        let go = root.join("go");
        // The child emits one record, then blocks until the sink says it saw it.
        // Bounded, so a buffering implementation FAILS rather than hangs.
        let bin = script(
            &root,
            "stratum",
            &format!(
                r#"echo '{{"seq":1}}'
    i=0
    while [ ! -f "{go}" ] && [ $i -lt 200 ]; do i=$((i+1)); sleep 0.05; done
    if [ -f "{go}" ]; then echo '{{"seq":2}}'; else echo '{{"seq":"TIMEOUT"}}'; fi
    exit 0
    "#
            ),
        );
        let entry = root.join("analysis.do");
        fs::write(&entry, "\n").expect("writing the entry file");

        let run = CleanRun::verify(&bin, &entry, root.join("scratch"));
        let mut seen = Vec::new();
        let out = run
            .stream(|l| {
                if l.contains("\"seq\":1") {
                    fs::write(&go, b"ok").expect("the sink acknowledges line 1");
                }
                seen.push(l.to_owned());
            })
            .expect("the child spawns, streams and reaps");

        assert_eq!(out.lines, 2);
        assert!(
            seen[1].contains("\"seq\":2"),
            "the child never saw the acknowledgement, so the first line was not \
             delivered until EOF: {seen:?}"
        );
    }

    #[test]
    fn a_failing_child_is_reported_and_still_reaped() {
        let dir = TempDir::new().expect("tempdir");
        let root = utf8(&dir);
        let bin = script(&root, "stratum", "echo '{\"seq\":1}'\nexit 9\n");
        let entry = root.join("analysis.do");
        fs::write(&entry, "\n").expect("writing the entry file");

        let out = CleanRun::verify(&bin, &entry, root.join("scratch"))
            .stream(|_| {})
            .expect("a non-zero exit is an outcome, not an error");
        assert_eq!(out.code, Some(9));
        assert!(!out.success());
        assert!(out.reaped);
        assert_eq!(out.lines, 1);
    }

    #[test]
    fn a_missing_binary_is_an_error_not_a_panic() {
        let run = CleanRun::verify("/nonexistent/stratum", "/p/a.do", "/scratch");
        let err = run.stream(|_| {}).expect_err("there is no such program");
        assert!(matches!(
            err,
            stratum_session::isolate::IsolateError::Spawn { .. }
        ));
    }
}

// ── the invocation ──────────────────────────────────────────────────────────

#[test]
fn the_child_is_stratum_run_clean_sandbox() {
    let run = CleanRun::verify("/opt/stratum/bin/stratum", "/p/analysis.do", "/scratch/r1");
    assert_eq!(
        run.command_line(),
        [
            "/opt/stratum/bin/stratum",
            "run",
            "/p/analysis.do",
            "--json",
            "--clean",
            "--sandbox",
            "/scratch/r1",
            "--deterministic",
        ]
    );
    // Item 7 again, applied to a process: the child runs from the entry file's
    // own directory, not from wherever the engine happens to be.
    assert_eq!(run.child_cwd(), Some(Utf8Path::new("/p")));

    // `--sandbox` is not optional in a verification run, and dropping it is the
    // regression this assertion exists for: an unsandboxed Verify would let a
    // reproducibility check overwrite the user's own outputs.
    assert!(run.args().iter().any(|a| a == "--sandbox"));
    assert!(run.args().iter().any(|a| a == "--clean"));
}

// ── the §16 tick ────────────────────────────────────────────────────────────

fn outcome(code: Option<i32>) -> CleanRunOutcome {
    CleanRunOutcome {
        code,
        lines: 12,
        bytes: 480,
        stderr: String::new(),
        reaped: true,
    }
}

#[test]
fn only_a_subprocess_run_may_tick_runs_from_clean_state() {
    // ARCHITECTURE §7.7's closing sentence. An in-process "clean" run shares the
    // OS process — its cwd, its environment, its open handles — with the
    // interactive session, and those are three of the sixteen items, so a
    // successful one still earns nothing.
    assert!(clean_state_tick(Isolation::Subprocess, &outcome(Some(0))));
    assert!(!clean_state_tick(Isolation::InProcess, &outcome(Some(0))));

    // A child that failed proves nothing either, and neither does one we never
    // reaped — an unreaped child may still be writing.
    assert!(!clean_state_tick(Isolation::Subprocess, &outcome(Some(1))));
    assert!(!clean_state_tick(Isolation::Subprocess, &outcome(None)));
    let unreaped = CleanRunOutcome {
        reaped: false,
        ..outcome(Some(0))
    };
    assert!(!clean_state_tick(Isolation::Subprocess, &unreaped));

    // And there is no parameter for a lint result, a `Taint` set or an
    // `EffectSet`: a caller holding only static analysis has nothing to pass.
    // That is the "we never infer it from static analysis" half, expressed in
    // the signature rather than in a comment.
}

// ── spawn, stream, reap ─────────────────────────────────────────────────────

// ── the write sandbox, per verb ─────────────────────────────────────────────

fn sandbox() -> WriteSandbox {
    WriteSandbox::new("/scratch/r1", "/projects/wage-study")
}

#[test]
fn save_redirects_into_the_scratch_directory() {
    let s = sandbox();
    let out = s.redirect(WriteVerb::Save, Utf8Path::new("results/final.dta"));
    assert_eq!(
        out,
        Sandboxed::Path("/scratch/r1/fs/_root/projects/wage-study/results/final.dta".into())
    );
    assert!(s.contains(out.path()));
}

#[test]
fn export_redirects_into_the_scratch_directory() {
    let s = sandbox();
    let out = s.redirect(WriteVerb::Export, Utf8Path::new("/tmp/table1.csv"));
    assert_eq!(
        out,
        Sandboxed::Path("/scratch/r1/fs/_root/tmp/table1.csv".into())
    );
    assert!(s.contains(out.path()));
}

#[test]
fn erase_redirects_into_the_scratch_directory() {
    // The verb that would otherwise DESTROY the user's data during a
    // reproducibility check. It is redirected, so `erase results/final.dta`
    // deletes the sandbox's copy and the real file survives.
    let s = sandbox();
    let out = s.redirect(WriteVerb::Erase, Utf8Path::new("results/final.dta"));
    assert_eq!(
        out,
        Sandboxed::Path("/scratch/r1/fs/_root/projects/wage-study/results/final.dta".into())
    );
    assert!(s.contains(out.path()));
}

#[test]
fn shell_is_contained_rather_than_redirected() {
    // `shell` names no path, so there is nothing to rewrite. It is contained:
    // the child's working directory and its temp directory are both inside the
    // scratch root, so a subprocess writing a relative path still lands there.
    let s = sandbox();
    let out = s.redirect(WriteVerb::Shell, Utf8Path::new(""));
    let Sandboxed::Contained { cwd, env } = out else {
        panic!("shell must be contained, not path-redirected");
    };
    assert_eq!(cwd, "/scratch/r1/shell");
    assert!(s.contains(&cwd));
    let keys: Vec<&str> = env.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(keys, ["TEMP", "TMP", "TMPDIR"]);
    // The values are compared as paths, not strings: `join` inserts the host's
    // separator into them (`\` on Windows), and which one it is is not the
    // property under test — where the child's temp files land is.
    for (k, v) in &env {
        assert_eq!(Utf8Path::new(v), Utf8Path::new("/scratch/r1/tmp"), "{k}");
    }
}

#[test]
fn every_write_verb_lands_inside_the_scratch_root() {
    // The closed enum plus this loop is the actual guarantee: adding a write
    // verb to `WriteVerb` without a redirect for it does not compile past
    // `redirect`, and adding one that escapes fails here.
    let s = sandbox();
    for verb in WriteVerb::ALL {
        for probe in [
            "out.dta",
            "/etc/passwd",
            "../../../../etc/passwd",
            "./sub/./x.csv",
            "results/2026/table.tex",
        ] {
            let out = s.redirect(verb, Utf8Path::new(probe));
            assert!(
                s.contains(out.path()),
                "{} escaped the sandbox with {probe}: {}",
                verb.name(),
                out.path()
            );
        }
    }
}

#[test]
fn the_mapping_is_injective_so_a_sandboxed_run_takes_the_same_branch() {
    // Two files that differ must stay two files: a do-file that saves `a/out`
    // and `b/out` and then `use`s one of them has to find the right one, or the
    // verification run diverges from the run it is verifying.
    let s = sandbox();
    let probes = [
        "a/out.dta",
        "b/out.dta",
        "/a/out.dta",
        "../a/out.dta",
        "/etc/passwd",
        "../../etc/passwd",
    ];
    let mut mapped: Vec<Utf8PathBuf> = probes
        .iter()
        .map(|p| s.map_path(Utf8Path::new(p)))
        .collect();
    let before = mapped.len();
    mapped.sort();
    mapped.dedup();
    assert_eq!(mapped.len(), before, "two distinct sources collided");

    // The converse, and it is not a bug: two SPELLINGS of one file are one
    // file. `./a/out.dta` and `a/out.dta` name the same path, so they must map
    // to the same sandbox path — the same rule `stratum_runtime`'s `PathKey`
    // states for read dependencies.
    assert_eq!(
        s.map_path(Utf8Path::new("./a/out.dta")),
        s.map_path(Utf8Path::new("a/out.dta"))
    );
}

#[test]
fn a_path_already_inside_the_sandbox_is_not_redirected_twice() {
    // A sandboxed run that reads back what it wrote must see the same path it
    // wrote. Double-mapping would turn `save x` / `use x` into two files.
    let s = sandbox();
    let once = s.map_path(Utf8Path::new("out.dta"));
    let twice = s.map_path(&once);
    assert_eq!(once, twice);
    assert_eq!(
        s.redirect(WriteVerb::Save, &once),
        Sandboxed::Path(once.clone())
    );
}

#[test]
fn parent_components_cannot_climb_out() {
    let s = sandbox();
    let out = s.map_path(Utf8Path::new("../../../../../../etc/passwd"));
    assert!(s.contains(&out));
    assert!(
        !out.as_str().contains("/.."),
        "a literal `..` survived into the mapped path: {out}"
    );
}
