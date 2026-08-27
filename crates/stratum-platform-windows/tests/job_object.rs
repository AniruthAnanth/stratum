//! The Windows kill-the-parent test W07 could not write.
//!
//! `apps/desktop/src-tauri/src/engine_host.rs` ships two of its three per-OS
//! orphan tests and says so in `orphan::WINDOWS_JOB_OBJECT_NOTE`: *"The Windows
//! one has no mechanism to test yet, and writing a green test against a
//! mechanism that does not exist would be worse than its absence. **W24 owes
//! it**, together with the Job Object itself."* This file is that debt.
//!
//! Three properties, and each is asserted the strongest way it can be:
//!
//! 1. **The child is inside a job.** Observed from outside, through
//!    `IsProcessInJob` on the child's pid, rather than trusted from the fact
//!    that the spawn returned `Ok`.
//! 2. **The job is armed to kill on close.** Not observable from another
//!    process — an anonymous job has no name to open — so it is checked inside
//!    the spawn instead, by reading the limit flags back; a spawn that returns
//!    `Ok` has already proved it. Windows' own guarantee does the rest, and it
//!    is a guarantee that holds even when the supervisor is `TerminateProcess`d
//!    and no code of ours runs, which is exactly the case a Rust-side watchdog
//!    cannot cover.
//! 3. **`terminate_tree` reaches a grandchild.** A do-file's `shell` command
//!    creates one, and killing only the engine leaks a process holding the
//!    dataset's file handles. This one is a real end-to-end kill.
//!
//! # Why every child here is PowerShell and not `cmd.exe`
//!
//! `std::process::Command` quotes its arguments for `CommandLineToArgvW`, which
//! is what PowerShell parses. `cmd.exe` does **not** parse its command line
//! that way, and a `/c "…& …>NUL"` argument comes out mangled in a manner that
//! depends on which metacharacters it contains. A test whose subject is process
//! supervision must not also be a test of `cmd`'s quoting rules.
//! `[Console]::Out.Flush()` for the same class of reason: PowerShell buffers a
//! redirected stdout, and a test that reads a pid before the child exits has to
//! say so explicitly.
#![cfg(target_os = "windows")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::{BufRead, BufReader};
use std::time::{Duration, Instant};

use stratum_platform::{EnvPolicy, ProcessHost, ProcessSpec};
use stratum_platform_windows::process::process_is_in_a_job;
use stratum_platform_windows::WindowsProcessHost;

fn system_root() -> String {
    std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_owned())
}

/// Windows PowerShell 5.1, which ships with every supported Windows.
fn powershell() -> camino::Utf8PathBuf {
    camino::Utf8PathBuf::from(format!(
        r"{}\System32\WindowsPowerShell\v1.0\powershell.exe",
        system_root()
    ))
}

fn script(body: &str) -> ProcessSpec {
    ProcessSpec {
        program: powershell(),
        args: vec![
            "-NoProfile".to_owned(),
            "-NonInteractive".to_owned(),
            "-Command".to_owned(),
            body.to_owned(),
        ],
        cwd: None,
        env: EnvPolicy::with_threads(1),
        kill_on_parent_exit: true,
    }
}

/// A child that announces itself and then waits, costing no CPU.
fn sleeper() -> ProcessSpec {
    script("[Console]::Out.WriteLine('ready'); [Console]::Out.Flush(); Start-Sleep -Seconds 300")
}

/// Whether a pid is still a live process, via `tasklist`, which is on every
/// Windows and needs no additional Win32 surface in a test.
fn alive(pid: u32) -> bool {
    let out = std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
        .output();
    match out {
        // `tasklist` prints "INFO: No tasks are running…" rather than failing,
        // so the exit status says nothing; the pid in the row is the signal.
        Ok(o) => String::from_utf8_lossy(&o.stdout).contains(&format!("\"{pid}\"")),
        Err(_) => false,
    }
}

fn wait_until_gone(pid: u32, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if !alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    !alive(pid)
}

fn first_line(child: &mut Box<dyn stratum_platform::SupervisedChild>) -> String {
    let mut line = String::new();
    let mut reader = BufReader::new(child.stdout());
    reader.read_line(&mut line).unwrap();
    line.trim().to_owned()
}

fn hard_kill(pid: u32) {
    let _ = std::process::Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .output();
}

/// Properties 1 and 2. A spawn that returned `Ok` has already had its limit
/// flags read back inside `spawn_supervised`; this adds the outside
/// observation that the child really is in a job.
#[test]
fn a_supervised_child_is_inside_a_job_object() {
    let host = WindowsProcessHost::new();
    let mut child = host.spawn_supervised(sleeper()).unwrap();
    let pid = child.pid();
    assert_eq!(first_line(&mut child), "ready");

    assert!(
        process_is_in_a_job(pid).unwrap(),
        "the engine is not in a job object: a hard-killed Stratum would leak it"
    );

    child.terminate_tree().unwrap();
    assert!(wait_until_gone(pid, Duration::from_secs(10)));
}

/// A child spawned WITHOUT `kill_on_parent_exit` gets no job, and
/// `terminate_tree` says so rather than quietly killing only the direct child
/// and reporting success for a tree it never reached.
#[test]
fn an_unsupervised_child_reports_that_it_has_no_tree_to_kill() {
    let host = WindowsProcessHost::new();
    let mut spec = sleeper();
    spec.kill_on_parent_exit = false;
    let mut child = host.spawn_supervised(spec).unwrap();
    let pid = child.pid();
    assert_eq!(first_line(&mut child), "ready");

    let err = child.terminate_tree().unwrap_err();
    assert!(err.is_unsupported(), "{err}");

    hard_kill(pid);
    assert!(wait_until_gone(pid, Duration::from_secs(10)));
}

/// Property 3, the one `TerminateProcess` on the direct child cannot do.
///
/// `Start-Process -PassThru` gives us a grandchild that is not the shell's
/// foreground job and whose pid we can read off our own stdout pipe. Killing
/// the child alone would leave it running; only the job reaches it.
#[test]
fn terminate_tree_reaches_a_grandchild_a_do_file_shelled_out_to() {
    let host = WindowsProcessHost::new();
    let mut child = host
        .spawn_supervised(script(
            "$p = Start-Process -FilePath ping -ArgumentList '-n','300','127.0.0.1' \
             -PassThru -WindowStyle Hidden; \
             [Console]::Out.WriteLine($p.Id); [Console]::Out.Flush(); \
             Start-Sleep -Seconds 300",
        ))
        .unwrap();
    let child_pid = child.pid();

    let line = first_line(&mut child);
    let grandchild: u32 = line
        .parse()
        .unwrap_or_else(|e| panic!("could not read the grandchild pid from {line:?}: {e}"));

    assert!(alive(grandchild), "the grandchild never started");
    assert!(
        process_is_in_a_job(grandchild).unwrap(),
        "a grandchild must inherit the job, or a do-file's `shell` command leaks a process \
         holding the dataset open"
    );

    child.terminate_tree().unwrap();

    assert!(
        wait_until_gone(child_pid, Duration::from_secs(15)),
        "the engine survived terminate_tree"
    );
    let gone = wait_until_gone(grandchild, Duration::from_secs(15));
    if !gone {
        hard_kill(grandchild);
    }
    assert!(
        gone,
        "the grandchild survived terminate_tree: this is the leak the Job Object exists to close"
    );
}

/// `wait_timeout` must return on the deadline even when the child never exits,
/// and must not spin a core while it waits — hence `WaitForSingleObject` rather
/// than a poll loop.
#[test]
fn wait_timeout_returns_none_on_a_live_child_and_a_status_on_a_dead_one() {
    let host = WindowsProcessHost::new();
    let mut child = host.spawn_supervised(sleeper()).unwrap();
    assert_eq!(first_line(&mut child), "ready");

    assert_eq!(
        child.wait_timeout(Duration::from_millis(200)).unwrap(),
        None
    );

    child.terminate_tree().unwrap();
    let status = child
        .wait_timeout(Duration::from_secs(15))
        .unwrap()
        .expect("a terminated child must report a status");
    assert!(!status.success());
    // Windows has no signals; a killed child is an exit code, never a signal.
    assert_eq!(status.signal, None);
}

/// The `EnvPolicy` acceptance bullet, on the real spawn path: the four thread
/// variables are forced on every child, whatever this process's own
/// environment says. A parent whose shell profile sets `OMP_NUM_THREADS` to the
/// machine width is exactly the case that produces the oversubscription bug.
#[test]
fn the_four_thread_variables_are_forced_on_the_real_spawn_path() {
    std::env::set_var("OMP_NUM_THREADS", "64");

    let host = WindowsProcessHost::new();
    let mut spec = script(
        "[Console]::Out.WriteLine(\"$env:OMP_NUM_THREADS $env:OPENBLAS_NUM_THREADS \
         $env:MKL_NUM_THREADS $env:RAYON_NUM_THREADS\"); [Console]::Out.Flush()",
    );
    spec.env = EnvPolicy::with_threads(3);

    let mut child = host.spawn_supervised(spec).unwrap();
    assert_eq!(first_line(&mut child), "3 3 3 3");
    let _ = child.wait_timeout(Duration::from_secs(15));
}
