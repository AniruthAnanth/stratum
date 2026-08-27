//! Supervised children and CPU topology on macOS — 08 §5.6.
//!
//! # `hw.perflevel0.logicalcpu`
//!
//! On Apple Silicon `hw.ncpu` and `hw.logicalcpu` count performance **and**
//! efficiency cores. Sizing a parallel reduction from that number means every
//! chunk boundary waits on a core that is three to four times slower, and
//! sizing four BLAS-shaped thread pools from it oversubscribes the machine —
//! which is how a statistics application ends up slower on more cores.
//! `hw.perflevel0.logicalcpu` is the performance-core count and is what
//! [`MacosProcessHost::physical_cores`] returns; on an Intel Mac, which has no
//! perflevels, it falls back to `hw.physicalcpu` (never `hw.logicalcpu`, which
//! counts hyperthreads).
//!
//! # `kill_on_parent_exit`
//!
//! macOS has no `PR_SET_PDEATHSIG`. Two mechanisms stand in, and the second is
//! the one that actually holds:
//!
//! 1. [`MacosChild`] kills its process group on `Drop`, which covers every
//!    orderly shutdown and every panic that unwinds.
//! 2. The child's stdin is a pipe held only by us. When this process dies for
//!    *any* reason the pipe reaches EOF, and `stratum serve`'s framed-stdio
//!    reader (CONTRACTS §10) treats EOF as "the supervisor is gone" and exits.
//!    That is what covers `SIGKILL` of the parent, where no code of ours runs.

use std::io::{Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use stratum_platform::{
    ExitStatus, PlatformError, ProcessHost, ProcessSpec, QosClass, Result, SupervisedChild,
};

// `pthread_set_qos_class_self_np` is in libSystem but is not declared by the
// `libc` crate.
extern "C" {
    fn pthread_set_qos_class_self_np(qos_class: u32, relative_priority: i32) -> libc::c_int;
}

// <sys/qos.h>. These are ABI, not a preference.
const QOS_CLASS_USER_INTERACTIVE: u32 = 0x21;
const QOS_CLASS_USER_INITIATED: u32 = 0x19;
const QOS_CLASS_DEFAULT: u32 = 0x15;
const QOS_CLASS_UTILITY: u32 = 0x11;
const QOS_CLASS_BACKGROUND: u32 = 0x09;

/// [`ProcessHost`] for macOS.
#[derive(Clone, Copy, Debug, Default)]
pub struct MacosProcessHost;

impl MacosProcessHost {
    /// Construct.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl ProcessHost for MacosProcessHost {
    fn spawn_supervised(&self, spec: ProcessSpec) -> Result<Box<dyn SupervisedChild>> {
        // `vars_os`, not `vars`: `std::env::vars()` PANICS on an environment
        // variable that is not valid Unicode, and a supervisor that aborts
        // because the user has one odd variable is not a supervisor. A
        // non-UTF-8 variable is dropped rather than passed through, because the
        // child's environment is a `BTreeMap<String, String>` on every platform.
        let inherited = std::env::vars_os()
            .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)));
        let env = spec.env.resolve(inherited);

        let mut cmd = Command::new(spec.program.as_std_path());
        cmd.args(&spec.args)
            .env_clear()
            .envs(&env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Inherited on purpose: the engine's tracing goes to the
            // supervisor's stderr, where a crash reporter can find it.
            .stderr(Stdio::inherit());
        if let Some(dir) = &spec.cwd {
            cmd.current_dir(dir.as_std_path());
        }

        // SAFETY: `setpgid` is async-signal-safe, which is the whole
        // requirement on a `pre_exec` closure. It allocates nothing and touches
        // no shared state.
        unsafe {
            use std::os::unix::process::CommandExt;
            cmd.pre_exec(|| {
                if libc::setpgid(0, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }

        let mut child = cmd.spawn()?;
        let pid = child.id();
        let stdin = child.stdin.take().ok_or_else(|| {
            PlatformError::BackendUnavailable("child stdin was not piped".to_owned())
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            PlatformError::BackendUnavailable("child stdout was not piped".to_owned())
        })?;

        Ok(Box::new(MacosChild {
            child,
            // We called `setpgid(0, 0)`, so the child leads a group whose id is
            // its own pid.
            pgid: pid as libc::pid_t,
            stdin,
            stdout,
            kill_on_drop: spec.kill_on_parent_exit,
        }))
    }

    fn physical_cores(&self) -> usize {
        sysctl_usize("hw.perflevel0.logicalcpu")
            .or_else(|| sysctl_usize("hw.physicalcpu"))
            .unwrap_or(1)
            .max(1)
    }

    fn performance_cores(&self) -> Option<usize> {
        // One perflevel means a homogeneous CPU, and the honest answer to
        // "which cores should I prefer?" is then "there is nothing to prefer".
        match sysctl_usize("hw.nperflevels") {
            Some(n) if n > 1 => sysctl_usize("hw.perflevel0.logicalcpu"),
            _ => None,
        }
    }

    fn set_process_qos(&self, qos: QosClass) -> Result<()> {
        let class = match qos {
            QosClass::UserInteractive => QOS_CLASS_USER_INTERACTIVE,
            QosClass::UserInitiated => QOS_CLASS_USER_INITIATED,
            QosClass::Default => QOS_CLASS_DEFAULT,
            QosClass::Utility => QOS_CLASS_UTILITY,
            QosClass::Background => QOS_CLASS_BACKGROUND,
        };
        // SAFETY: a libSystem call taking two scalars; `class` is one of the
        // documented constants.
        let rc = unsafe { pthread_set_qos_class_self_np(class, 0) };
        if rc == 0 {
            Ok(())
        } else {
            Err(PlatformError::Os {
                code: i64::from(rc),
                message: "pthread_set_qos_class_self_np failed".to_owned(),
            })
        }
    }

    fn available_memory(&self) -> Option<u64> {
        let page = sysctl_usize("hw.pagesize")? as u64;
        // SAFETY: `host_statistics64` writes `count` integers into `stats`;
        // `count` is derived from the size of the struct it fills.
        #[allow(
            deprecated,
            reason = "libc points at `mach2`; one more crate to \
            reach `mach_host_self` is not worth it for a single call"
        )]
        let stats = unsafe {
            let mut stats = std::mem::zeroed::<libc::vm_statistics64>();
            let mut count = libc::HOST_VM_INFO64_COUNT;
            let rc = libc::host_statistics64(
                libc::mach_host_self(),
                libc::HOST_VM_INFO64,
                std::ptr::addr_of_mut!(stats).cast(),
                &mut count,
            );
            if rc != libc::KERN_SUCCESS {
                return None;
            }
            stats
        };
        // What a large allocation could plausibly get: genuinely free pages,
        // plus the two categories the kernel reclaims without swapping —
        // inactive and purgeable. Wired and compressed pages are not available
        // at any price, and counting them is how an out-of-core threshold ends
        // up never firing.
        let pages = u64::from(stats.free_count)
            + u64::from(stats.inactive_count)
            + u64::from(stats.purgeable_count);
        Some(pages * page)
    }
}

/// A running child in its own process group.
struct MacosChild {
    child: Child,
    pgid: libc::pid_t,
    stdin: ChildStdin,
    stdout: ChildStdout,
    kill_on_drop: bool,
}

impl MacosChild {
    fn signal_group(&self, sig: libc::c_int) -> Result<()> {
        // SAFETY: `killpg` with a pgid we created and a constant signal.
        let rc = unsafe { libc::killpg(self.pgid, sig) };
        if rc == 0 {
            return Ok(());
        }
        let err = std::io::Error::last_os_error();
        // ESRCH: the group is already gone. The caller asked for a state.
        if err.raw_os_error() == Some(libc::ESRCH) {
            return Ok(());
        }
        Err(PlatformError::Os {
            code: err.raw_os_error().unwrap_or(-1).into(),
            message: err.to_string(),
        })
    }
}

impl SupervisedChild for MacosChild {
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
                    // and `SIGCHLD` handling is a process-global resource we
                    // must not claim from a library.
                    std::thread::sleep(Duration::from_millis(2).min(deadline - Instant::now()));
                }
            }
        }
    }
}

impl Drop for MacosChild {
    fn drop(&mut self) {
        if self.kill_on_drop {
            let _ = self.signal_group(libc::SIGKILL);
            // Reap, so the child does not linger as a zombie in `ps` for the
            // rest of the IDE's session.
            let _ = self.child.wait();
        }
    }
}

/// Read an integer `sysctl` by name. `None` when the key does not exist on this
/// machine, which is the normal answer for `hw.perflevel*` on Intel.
fn sysctl_usize(name: &str) -> Option<usize> {
    let Ok(cname) = std::ffi::CString::new(name) else {
        return None;
    };
    let mut value: u64 = 0;
    let mut len = std::mem::size_of::<u64>();
    // SAFETY: `cname` is NUL-terminated, and the out buffer's length is passed
    // by pointer exactly as sysctlbyname requires.
    let rc = unsafe {
        libc::sysctlbyname(
            cname.as_ptr(),
            std::ptr::addr_of_mut!(value).cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return None;
    }
    // Some keys are 4 bytes and some are 8; a short read leaves the high half
    // as the zero we initialised it to.
    usize::try_from(value).ok()
}
