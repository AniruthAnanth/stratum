//! macOS process supervision, against the real kernel.
#![cfg(target_os = "macos")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;
use std::time::Duration;

use stratum_platform::{EnvPolicy, ProcessHost, ProcessSpec, QosClass};
use stratum_platform_macos::MacosProcessHost;

fn sysctl(name: &str) -> usize {
    let out = Command::new("/usr/sbin/sysctl")
        .args(["-n", name])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().parse().unwrap()
}

/// The acceptance bullet, against `sysctl(8)` rather than against our own
/// reimplementation of it.
#[test]
fn physical_cores_is_perflevel0_and_never_the_logical_count() {
    let host = MacosProcessHost::new();
    let logical = sysctl("hw.logicalcpu");
    let got = host.physical_cores();

    if sysctl("hw.nperflevels") > 1 {
        let p_cores = sysctl("hw.perflevel0.logicalcpu");
        assert_eq!(
            got, p_cores,
            "physical_cores must be hw.perflevel0.logicalcpu"
        );
        assert_eq!(host.performance_cores(), Some(p_cores));
        assert!(
            got < logical,
            "on a heterogeneous CPU the P-core count is strictly below the logical count"
        );
    } else {
        assert_eq!(got, sysctl("hw.physicalcpu"));
        assert_eq!(host.performance_cores(), None);
    }
    assert!(got >= 1 && got <= logical);
}

/// The other acceptance bullet: the four thread variables are forced on a real
/// child, not merely computed by `EnvPolicy`.
#[test]
fn a_spawned_child_sees_the_four_forced_thread_vars() {
    // A user's shell profile pinning this to the machine width is exactly the
    // situation the forcing exists for.
    std::env::set_var("OMP_NUM_THREADS", "999");

    let host = MacosProcessHost::new();
    let mut child = host
        .spawn_supervised(ProcessSpec::new("/usr/bin/env", 3))
        .unwrap();

    let mut out = String::new();
    child.stdout().read_to_string(&mut out).unwrap();
    let seen: Vec<&str> = out.lines().collect();

    for key in EnvPolicy::FORCED_THREAD_VARS {
        assert!(
            seen.contains(&format!("{key}=3").as_str()),
            "{key}=3 missing from the child environment:\n{out}"
        );
    }
    assert!(!seen.contains(&"OMP_NUM_THREADS=999"));

    let status = child
        .wait_timeout(Duration::from_secs(10))
        .unwrap()
        .unwrap();
    assert!(status.success());
}

#[test]
fn interrupt_and_terminate_reach_the_whole_process_group() {
    let host = MacosProcessHost::new();
    // `sh -c` with a backgrounded grandchild: killing only the leader would
    // leave `sleep 300` behind, which is the leak this test exists for.
    let spec = ProcessSpec::new("/bin/sh", 1)
        .arg("-c")
        .arg("sleep 300 & echo $! ; wait");
    let mut child = host.spawn_supervised(spec).unwrap();

    let mut buf = [0_u8; 32];
    let n = child.stdout().read(&mut buf).unwrap();
    let grandchild: i32 = String::from_utf8_lossy(&buf[..n]).trim().parse().unwrap();

    assert!(child
        .wait_timeout(Duration::from_millis(50))
        .unwrap()
        .is_none());
    child.terminate_tree().unwrap();

    let status = child.wait_timeout(Duration::from_secs(5)).unwrap().unwrap();
    assert_eq!(status.signal, Some(9));

    // Give the kernel a moment to reap the group, then prove the grandchild is
    // gone: `kill -0` fails with ESRCH.
    std::thread::sleep(Duration::from_millis(200));
    let alive = Command::new("/bin/kill")
        .args(["-0", &grandchild.to_string()])
        .status()
        .unwrap()
        .success();
    assert!(!alive, "grandchild {grandchild} survived terminate_tree");

    // Terminating an already-dead group is a state, not an event.
    child.terminate_tree().unwrap();
    child.interrupt().unwrap();
}

#[test]
fn qos_and_memory_are_answered_not_guessed() {
    let host = MacosProcessHost::new();
    for qos in [
        QosClass::UserInteractive,
        QosClass::UserInitiated,
        QosClass::Default,
        QosClass::Utility,
        QosClass::Background,
    ] {
        host.set_process_qos(qos).unwrap();
    }
    // Put the test thread back where it started.
    host.set_process_qos(QosClass::Default).unwrap();

    let avail = host.available_memory().unwrap();
    let total: u64 = String::from_utf8_lossy(
        &Command::new("/usr/sbin/sysctl")
            .args(["-n", "hw.memsize"])
            .output()
            .unwrap()
            .stdout,
    )
    .trim()
    .parse()
    .unwrap();
    assert!(avail > 0 && avail < total, "available {avail} of {total}");
}
