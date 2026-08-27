//! Supervised children and CPU topology on Linux — 08 §5.6.
//!
//! # `PR_SET_PDEATHSIG` has a trap, and this module walks around both halves
//!
//! `prctl(PR_SET_PDEATHSIG, SIGKILL)` is the Linux answer to
//! [`ProcessSpec::kill_on_parent_exit`], and it is subtler than it looks.
//!
//! **Trap one: the signal fires when the parent THREAD exits, not the parent
//! process.** This is documented in `prctl(2)` and is routinely missed. Tauri
//! calls into the platform layer from whatever thread the IPC handler is on,
//! and those threads come and go — so a naive implementation kills the engine
//! the moment one worker finishes, which presents as "the IDE randomly loses
//! its engine under load". [`LinuxProcessHost`] therefore forks from a single
//! **process-lifetime spawner thread**, so the thread PDEATHSIG watches lives
//! exactly as long as the process does.
//!
//! **Trap two: the parent can die between `fork` and `prctl`.** The child then
//! has an already-dead parent, the signal it just armed will never be raised,
//! and it is an orphan holding the dataset's file handles. The remedy is one
//! comparison: re-read `getppid()` *after* arming, and if it is no longer the
//! pid we recorded before forking, exit immediately.
//!
//! A third mechanism backstops both, exactly as on macOS: the child's stdin is
//! a pipe held only by us, so when this process dies for *any* reason —
//! including `SIGKILL`, where no code of ours runs — the pipe reaches EOF and
//! `stratum serve`'s framed-stdio reader (CONTRACTS §10) treats that as "the
//! supervisor is gone".
//!
//! # `physical_cores` is three ceilings, not one number
//!
//! See [`crate::procfs`]. Hyperthreads, `sched_getaffinity` and the cgroup CPU
//! quota are each independently a hard limit, and a researcher on a cluster
//! hits all three at once.

use std::io::{Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{mpsc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use stratum_platform::{
    ExitStatus, PlatformError, ProcessHost, ProcessSpec, QosClass, Result, SupervisedChild,
};

use crate::procfs;

/// `ioprio_set(2)`'s `IOPRIO_WHO_PROCESS`.
const IOPRIO_WHO_PROCESS: libc::c_int = 1;
/// `IOPRIO_CLASS_BE` and `IOPRIO_CLASS_IDLE`, shifted into place by
/// [`ioprio_value`].
const IOPRIO_CLASS_BE: libc::c_int = 2;
const IOPRIO_CLASS_IDLE: libc::c_int = 3;
/// `IOPRIO_CLASS_SHIFT` from `<linux/ioprio.h>`.
const IOPRIO_CLASS_SHIFT: libc::c_int = 13;

const fn ioprio_value(class: libc::c_int, data: libc::c_int) -> libc::c_int {
    (class << IOPRIO_CLASS_SHIFT) | data
}

/// [`ProcessHost`] for Linux.
#[derive(Clone, Copy, Debug, Default)]
pub struct LinuxProcessHost;

impl LinuxProcessHost {
    /// Construct.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

/// The CPU picture, read once.
///
/// Once, because it cannot change within a process — `sched_setaffinity` from
/// outside can, in principle, but a supervisor re-reading four `/proc` files on
/// every spawn and every out-of-core decision is work on a path that must not
/// have any.
#[derive(Clone, Copy, Debug)]
struct Topology {
    physical: usize,
    affinity: Option<usize>,
    cgroup: Option<usize>,
    performance: Option<usize>,
}

fn topology() -> &'static Topology {
    static TOPOLOGY: OnceLock<Topology> = OnceLock::new();
    TOPOLOGY.get_or_init(|| Topology {
        physical: std::fs::read_to_string("/proc/cpuinfo")
            .ok()
            .and_then(|s| procfs::cpuinfo_physical_cores(&s))
            .unwrap_or(1),
        affinity: affinity_count(),
        cgroup: cgroup_cpus(),
        performance: performance_cores(),
    })
}

/// How many CPUs this process may run on, from `sched_getaffinity`. `taskset`,
/// SLURM's `--cpus-per-task` and systemd's `AllowedCPUs` all narrow this.
fn affinity_count() -> Option<usize> {
    // SAFETY: `cpu_set_t` is a plain bitmask with no invalid representation,
    // and `sched_getaffinity` fills exactly `size_of::<cpu_set_t>()` bytes.
    let set = unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        if libc::sched_getaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &mut set) != 0 {
            return None;
        }
        set
    };
    // SAFETY: `CPU_COUNT` reads the mask we just filled.
    let n = unsafe { libc::CPU_COUNT(&set) };
    usize::try_from(n).ok().filter(|n| *n > 0)
}

/// The cgroup CPU quota, v2 first because that is what every distro in 08 §6.2
/// ships. Inside a cgroup namespace — which is what a container gets — these
/// paths are the container's own limits rather than the host's.
fn cgroup_cpus() -> Option<usize> {
    if let Ok(s) = std::fs::read_to_string("/sys/fs/cgroup/cpu.max") {
        if let Some(n) = procfs::cgroup_v2_cpus(&s) {
            return Some(n);
        }
    }
    let quota = std::fs::read_to_string("/sys/fs/cgroup/cpu/cpu.cfs_quota_us").ok()?;
    let period = std::fs::read_to_string("/sys/fs/cgroup/cpu/cpu.cfs_period_us").ok()?;
    procfs::cgroup_v1_cpus(&quota, &period)
}

/// The performance-core count of a heterogeneous CPU.
///
/// Intel hybrid parts publish `/sys/devices/cpu_core/cpus` directly, which is
/// both cheaper and more reliable than the capacity scan. ARM DynamIQ does not,
/// so the fallback reads each CPU's `cpu_capacity`.
fn performance_cores() -> Option<usize> {
    if let Ok(list) = std::fs::read_to_string("/sys/devices/cpu_core/cpus") {
        if let Some(n) = procfs::cpu_list_len(&list) {
            return Some(n);
        }
    }
    let mut capacities = Vec::new();
    for cpu in 0..1024u32 {
        let path = format!("/sys/devices/system/cpu/cpu{cpu}/cpu_capacity");
        let Ok(text) = std::fs::read_to_string(&path) else {
            // The CPUs are numbered contiguously; the first gap is the end.
            break;
        };
        let Ok(v) = text.trim().parse::<u32>() else {
            break;
        };
        capacities.push(v);
    }
    procfs::perf_cores_from_capacities(&capacities)
}

/// The job the spawner thread runs. Boxed rather than a `Command`, so the
/// closure can own the reply channel and the whole spawn — including
/// `pre_exec` — happens on that thread.
type Job = Box<dyn FnOnce() + Send + 'static>;

/// The process-lifetime spawner. `None` when the thread could not be created,
/// in which case we spawn inline and `PR_SET_PDEATHSIG` is watching the calling
/// thread instead — degraded, but the stdin-EOF backstop still holds.
fn spawner() -> Option<&'static Mutex<mpsc::Sender<Job>>> {
    static SPAWNER: OnceLock<Option<Mutex<mpsc::Sender<Job>>>> = OnceLock::new();
    SPAWNER
        .get_or_init(|| {
            let (tx, rx) = mpsc::channel::<Job>();
            std::thread::Builder::new()
                .name("stratum-spawner".to_owned())
                // 128 KiB: this thread runs a closure that builds a `Command`
                // and forks. The default 2 MiB is 2 MiB of address space held
                // for the life of the IDE for no reason.
                .stack_size(128 * 1024)
                .spawn(move || {
                    // Ends only when the sender is dropped, which is never:
                    // the sender lives in a `OnceLock` for the process
                    // lifetime. That is the point — see the module docs.
                    while let Ok(job) = rx.recv() {
                        job();
                    }
                })
                .ok()
                .map(|_| Mutex::new(tx))
        })
        .as_ref()
}

/// Build the `Command`, including the `pre_exec` that arms `PR_SET_PDEATHSIG`.
fn build_command(spec: &ProcessSpec, parent_pid: libc::pid_t) -> Command {
    // `vars_os`, not `vars`: `std::env::vars()` PANICS on an environment
    // variable that is not valid Unicode, and a supervisor that aborts because
    // the user has one odd variable is not a supervisor.
    let inherited = std::env::vars_os()
        .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)));
    let env = spec.env.resolve(inherited);

    let mut cmd = Command::new(spec.program.as_std_path());
    cmd.args(&spec.args)
        .env_clear()
        .envs(&env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        // Inherited on purpose: the engine's tracing goes to the supervisor's
        // stderr, where a crash reporter can find it.
        .stderr(Stdio::inherit());
    if let Some(dir) = &spec.cwd {
        cmd.current_dir(dir.as_std_path());
    }

    let pdeathsig = spec.kill_on_parent_exit;
    // SAFETY: everything in this closure is async-signal-safe — three syscalls
    // and an integer comparison. It allocates nothing and touches no shared
    // state, which is the whole requirement on a `pre_exec` closure.
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(move || {
            if pdeathsig {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                // Trap two, closed. If the parent died between the fork and
                // the prctl above, the signal we just armed will never fire.
                if libc::getppid() != parent_pid {
                    return Err(std::io::Error::other(
                        "the supervisor exited before the child could arm PR_SET_PDEATHSIG",
                    ));
                }
            }
            // Our own process group, so `interrupt` and `terminate_tree` can
            // reach a do-file's `shell` grandchildren too.
            if libc::setpgid(0, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    cmd
}

impl ProcessHost for LinuxProcessHost {
    fn spawn_supervised(&self, spec: ProcessSpec) -> Result<Box<dyn SupervisedChild>> {
        // SAFETY: `getpid` takes no arguments and cannot fail.
        let parent_pid = unsafe { libc::getpid() };
        // Read before `spec` moves into the spawner closure.
        let kill_on_drop = spec.kill_on_parent_exit;

        let mut child = match spawner() {
            Some(tx) => {
                let (reply_tx, reply_rx) = mpsc::channel::<std::io::Result<Child>>();
                let job: Job = Box::new(move || {
                    let mut cmd = build_command(&spec, parent_pid);
                    let _ = reply_tx.send(cmd.spawn());
                });
                tx.lock()
                    .map_err(|_| {
                        PlatformError::BackendUnavailable(
                            "the spawner channel lock was poisoned".to_owned(),
                        )
                    })?
                    .send(job)
                    .map_err(|_| {
                        PlatformError::BackendUnavailable(
                            "the spawner thread is gone; no child can be supervised".to_owned(),
                        )
                    })?;
                reply_rx
                    .recv()
                    .map_err(|_| {
                        PlatformError::BackendUnavailable(
                            "the spawner thread dropped the request".to_owned(),
                        )
                    })?
                    .map_err(PlatformError::Io)?
            }
            // Degraded: PDEATHSIG now watches the calling thread. Documented
            // in the module docs; the stdin-EOF backstop still holds.
            None => build_command(&spec, parent_pid).spawn()?,
        };

        let pid = child.id();
        let stdin = child.stdin.take().ok_or_else(|| {
            PlatformError::BackendUnavailable("child stdin was not piped".to_owned())
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            PlatformError::BackendUnavailable("child stdout was not piped".to_owned())
        })?;

        Ok(Box::new(LinuxChild {
            child,
            // We called `setpgid(0, 0)`, so the child leads a group whose id is
            // its own pid.
            pgid: pid as libc::pid_t,
            stdin,
            stdout,
            kill_on_drop,
        }))
    }

    fn physical_cores(&self) -> usize {
        let t = topology();
        procfs::effective_cores(t.physical, t.affinity, t.cgroup)
    }

    fn performance_cores(&self) -> Option<usize> {
        // Clamped by the same ceilings: eight P-cores are not eight P-cores
        // when the cgroup allows two CPUs.
        topology()
            .performance
            .map(|p| p.min(self.physical_cores()).max(1))
    }

    fn set_process_qos(&self, qos: QosClass) -> Result<()> {
        // Linux has no QoS class. The honest translation is scheduling
        // niceness plus an I/O class, which is what `nice(1)` and `ionice(1)`
        // set and what every scheduler on the box actually reads.
        let (nice, io) = match qos {
            QosClass::UserInteractive | QosClass::UserInitiated | QosClass::Default => {
                (0, ioprio_value(IOPRIO_CLASS_BE, 4))
            }
            QosClass::Utility => (5, ioprio_value(IOPRIO_CLASS_BE, 6)),
            QosClass::Background => (10, ioprio_value(IOPRIO_CLASS_IDLE, 0)),
        };

        // SAFETY: `setpriority` takes three scalars and cannot trap.
        if unsafe { libc::setpriority(libc::PRIO_PROCESS, 0, nice) } == -1 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EACCES) || err.raw_os_error() == Some(libc::EPERM) {
                // The Linux trap nobody expects: an unprivileged process can
                // RAISE its nice value but not lower it again. So going
                // Background → Default fails, permanently, without
                // CAP_SYS_NICE. Saying so beats a caller believing the UI is
                // back at normal priority when it is not.
                return Err(PlatformError::PermissionDenied(format!(
                    "cannot set nice to {nice}: an unprivileged process may raise its nice \
                     value but never lower it again (CAP_SYS_NICE)"
                )));
            }
            return Err(os_error(&err));
        }

        // `ioprio_set` has no libc wrapper on any platform.
        // SAFETY: a raw syscall with three integer arguments, exactly as
        // `ioprio_set(2)` declares them.
        if unsafe { libc::syscall(libc::SYS_ioprio_set, IOPRIO_WHO_PROCESS, 0, io) } == -1 {
            let err = std::io::Error::last_os_error();
            // A container or a kernel without the I/O scheduler refuses this
            // and it does not matter: the nice value already did the important
            // half. Reporting a hard error for it would make "run in the
            // background" unavailable on every seccomp-filtered runtime.
            if !matches!(
                err.raw_os_error(),
                Some(libc::EPERM | libc::ENOSYS | libc::EINVAL)
            ) {
                return Err(os_error(&err));
            }
        }
        Ok(())
    }

    fn available_memory(&self) -> Option<u64> {
        let host = std::fs::read_to_string("/proc/meminfo")
            .ok()
            .and_then(|s| procfs::mem_available(&s))?;
        // A container's memory limit is invisible in `/proc/meminfo`, which
        // reports the host's. Without this clamp a 4 GB container on a 512 GB
        // node decides a 200 GB dataset fits in memory, and the OOM killer
        // ends the session with no diagnostic anywhere.
        match cgroup_available_memory() {
            Some(limit) => Some(host.min(limit)),
            None => Some(host),
        }
    }
}

/// `memory.max - memory.current` from cgroup v2, or the v1 equivalent.
/// `None` when there is no limit, which is the common case outside a container.
fn cgroup_available_memory() -> Option<u64> {
    let read = |path: &str| -> Option<u64> {
        let raw = std::fs::read_to_string(path).ok()?;
        let raw = raw.trim();
        if raw == "max" {
            return None;
        }
        raw.parse::<u64>().ok()
    };
    if let (Some(max), Some(current)) = (
        read("/sys/fs/cgroup/memory.max"),
        read("/sys/fs/cgroup/memory.current"),
    ) {
        return Some(max.saturating_sub(current));
    }
    let max = read("/sys/fs/cgroup/memory/memory.limit_in_bytes")?;
    let used = read("/sys/fs/cgroup/memory/memory.usage_in_bytes")?;
    // v1 spells "no limit" as a number near u64::MAX rather than as `max`.
    if max > (1 << 62) {
        return None;
    }
    Some(max.saturating_sub(used))
}

fn os_error(err: &std::io::Error) -> PlatformError {
    PlatformError::Os {
        code: err.raw_os_error().unwrap_or(-1).into(),
        message: err.to_string(),
    }
}

/// A running child leading its own process group.
struct LinuxChild {
    child: Child,
    pgid: libc::pid_t,
    stdin: ChildStdin,
    stdout: ChildStdout,
    kill_on_drop: bool,
}

impl LinuxChild {
    fn signal_group(&self, sig: libc::c_int) -> Result<()> {
        // SAFETY: `killpg` with a pgid we created and a constant signal.
        if unsafe { libc::killpg(self.pgid, sig) } == 0 {
            return Ok(());
        }
        let err = std::io::Error::last_os_error();
        // ESRCH: the group is already gone. The caller asked for a state.
        if err.raw_os_error() == Some(libc::ESRCH) {
            return Ok(());
        }
        Err(os_error(&err))
    }
}

impl SupervisedChild for LinuxChild {
    fn pid(&self) -> u32 {
        self.child.id()
    }

    fn stdin(&mut self) -> &mut (dyn Write + Send) {
        &mut self.stdin
    }

    fn stdout(&mut self) -> &mut (dyn Read + Send) {
        &mut self.stdout
    }

    fn interrupt(&self) -> Result<()> {
        // The GROUP, not the process: a do-file that shelled out has children
        // of its own, and interrupting only the engine leaves them running.
        self.signal_group(libc::SIGINT)
    }

    fn terminate_tree(&self) -> Result<()> {
        self.signal_group(libc::SIGKILL)
    }

    fn wait_timeout(&mut self, d: Duration) -> Result<Option<ExitStatus>> {
        use std::os::unix::process::ExitStatusExt;

        let deadline = Instant::now() + d;
        loop {
            match self.child.try_wait()? {
                Some(status) => {
                    return Ok(Some(ExitStatus {
                        code: status.code(),
                        signal: status.signal(),
                    }))
                }
                None if Instant::now() >= deadline => return Ok(None),
                None => {
                    // A 2 ms poll rather than a blocking `waitpid`: this has to
                    // return on the deadline even when the child never exits,
                    // and `SIGCHLD` handling is a process-global resource a
                    // library must not claim.
                    //
                    // `saturating_duration_since`, not `deadline - Instant::now()`:
                    // the guard above proved `now < deadline` when it ran, but
                    // the subtraction happens later, and `Instant`'s `Sub`
                    // PANICS on a negative interval. A supervisor that aborts
                    // because a scheduler preempted it for 2 ms between two
                    // lines is a supervisor that aborts under exactly the load
                    // that makes it matter.
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    std::thread::sleep(Duration::from_millis(2).min(remaining));
                }
            }
        }
    }
}

impl Drop for LinuxChild {
    fn drop(&mut self) {
        if self.kill_on_drop {
            let _ = self.signal_group(libc::SIGKILL);
            // Reap, so the child does not linger as a zombie in `ps` for the
            // rest of the IDE's session.
            let _ = self.child.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use stratum_platform::EnvPolicy;

    use super::*;

    /// The environment a `Command` will actually give the child, after
    /// `env_clear` plus the explicit set.
    fn env_of(cmd: &Command) -> BTreeMap<String, String> {
        cmd.get_envs()
            .filter_map(|(k, v)| {
                Some((
                    k.to_str()?.to_owned(),
                    v.and_then(|v| v.to_str()).unwrap_or_default().to_owned(),
                ))
            })
            .collect()
    }

    /// W10's last acceptance bullet, asserted on THIS platform's spawn path
    /// rather than only on `EnvPolicy` in the trait crate: leaving these unset
    /// is the single most common cause of a statistics application being
    /// slower on many cores than on few.
    #[test]
    fn every_spawned_child_gets_the_four_forced_thread_variables() {
        let spec = ProcessSpec::new("/usr/bin/stratum", 6).arg("serve");
        let env = env_of(&build_command(&spec, 1));
        for k in EnvPolicy::FORCED_THREAD_VARS {
            assert_eq!(env.get(k).map(String::as_str), Some("6"), "{k}");
        }
    }

    /// The inherited value a user's shell profile sets is the exact value that
    /// produces the oversubscription bug, and an override must not reopen it.
    #[test]
    fn an_override_cannot_reopen_the_thread_variables_through_this_path() {
        let mut spec = ProcessSpec::new("/usr/bin/stratum", 2);
        spec.env = EnvPolicy::with_threads(2)
            .set("OMP_NUM_THREADS", "64")
            .set("STRATUM_LOG", "debug");
        let env = env_of(&build_command(&spec, 1));
        assert_eq!(env.get("OMP_NUM_THREADS").map(String::as_str), Some("2"));
        assert_eq!(env.get("STRATUM_LOG").map(String::as_str), Some("debug"));
    }

    /// Whatever the machine, the three ceilings compose to something a thread
    /// pool can be sized from — never zero, and never more than the topology.
    #[test]
    fn the_reported_core_count_is_usable_on_this_machine() {
        let host = LinuxProcessHost::new();
        let cores = host.physical_cores();
        assert!(cores >= 1);
        assert!(cores <= topology().physical.max(1));
        if let Some(p) = host.performance_cores() {
            assert!(p >= 1 && p <= cores);
        }
        // `/proc/meminfo` exists on every Linux this ships to, so an absent
        // answer here means the parser broke rather than the kernel being old.
        assert!(host.available_memory().is_some_and(|m| m > 0));
    }
}
