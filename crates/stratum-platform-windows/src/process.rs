//! Supervised children, the Job Object, and CPU topology — 08 §5.6.
//!
//! # The Job Object W07 could not supply
//!
//! `apps/desktop/src-tauri/src/engine_host.rs` sets
//! `CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW` — everything reachable through
//! `std` alone — and then says, in
//! `orphan::WINDOWS_JOB_OBJECT_NOTE`: *"Job Object assignment belongs to
//! `stratum-platform-windows` (W24)"*, because creating one needs the `windows`
//! crate and ARCHITECTURE §5 allows that only inside this crate. Its own note
//! records the consequence — *"until that lands, a hard-killed Stratum leaks
//! its engine on Windows"* — and the missing third kill-the-parent test. This
//! module is that assignment.
//!
//! **One job per child, not one per host.** Both shapes satisfy
//! `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`; only this one also gives
//! [`SupervisedChild::terminate_tree`](stratum_platform::SupervisedChild::terminate_tree) a correct implementation. With a single
//! shared job, terminating "the tree" of one engine would terminate every
//! engine in the application — and §26 is explicitly multi-window. Per child,
//! `TerminateJobObject` kills exactly that engine plus whatever a do-file's
//! `shell` command spawned under it, and our own death still closes every job
//! handle we hold, so no configuration leaks. The cost is one kernel handle per
//! supervised engine, which is not a number that grows.
//!
//! **The supervision is armed, then verified.** `SetInformationJobObject` can
//! succeed while a nesting policy silently drops the limit, and
//! `AssignProcessToJobObject` can succeed against a job the child is not
//! actually in. Either would produce a child that looks supervised and is not —
//! which is exactly the state W07's note describes, arrived at with no error
//! anywhere. So [`ProcessHost::spawn_supervised`](stratum_platform::ProcessHost::spawn_supervised) reads the limit flags back
//! with `QueryInformationJobObject` and confirms membership with
//! `IsProcessInJob`, and fails the spawn if either says no. Two extra syscalls
//! per engine launch, once per engine, against a leak nobody would notice until
//! a user's machine had eight dead engines holding their dataset open.
//!
//! # `physical_cores`, not the logical count
//!
//! `GetSystemInfo().dwNumberOfProcessors` counts hyperthreads. Sizing four
//! BLAS-shaped pools from it oversubscribes the machine by 2×, which is the
//! classic "our statistics app is slower on the big machine" bug.
//! `GetLogicalProcessorInformationEx(RelationProcessorCore)` returns one record
//! per **physical** core, and on an Intel P/E part each record carries an
//! `EfficiencyClass` — so the same single call answers
//! [`ProcessHost::performance_cores`](stratum_platform::ProcessHost::performance_cores) too.
//!
//! The record walk is a **pure function over the returned bytes**
//! ([`parse_topology`]), because the buffer is a sequence of *variable-length*
//! records and treating it as an array of a fixed-size struct is the standard
//! way to get this wrong. As a pure function it is exercised against synthetic
//! buffers on every host, including the heterogeneous one this developer
//! machine cannot produce.

use stratum_platform::Result;

/// `CREATE_NEW_PROCESS_GROUP`. Required for `CTRL_BREAK_EVENT` to be
/// deliverable to the child alone.
pub const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
/// `CREATE_NO_WINDOW`. Without it every engine launch flashes a console window
/// over the IDE. The child still *has* a console, which is what makes the
/// control event above possible.
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// `RelationProcessorCore`, the `LOGICAL_PROCESSOR_RELATIONSHIP` discriminant.
pub const RELATION_PROCESSOR_CORE: i32 = 0;

/// The creation flags for a supervised engine child.
///
/// A function rather than a `const` so the pairing is one named thing: they are
/// not independent choices, and `CREATE_NO_WINDOW` without
/// `CREATE_NEW_PROCESS_GROUP` produces a child that cannot be interrupted.
#[must_use]
pub const fn creation_flags() -> u32 {
    CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW
}

/// What one `GetLogicalProcessorInformationEx(RelationProcessorCore)` buffer
/// says about this machine.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Topology {
    /// Physical cores. Never the logical count.
    pub physical: usize,
    /// Performance cores on a heterogeneous CPU; `None` when every core has
    /// the same `EfficiencyClass`, which is the honest answer to "which cores
    /// should I prefer?" on a homogeneous part.
    pub performance: Option<usize>,
    /// **The counter (ADR-017).** Records the walk visited. One linear pass:
    /// this equals the number of records in the buffer, never more, and the
    /// buffer is read exactly once per record header.
    pub records_visited: usize,
}

/// Walk a `SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX` buffer.
///
/// Layout, from `winnt.h`, which is ABI and not a preference:
///
/// ```text
/// 0..4    Relationship : LOGICAL_PROCESSOR_RELATIONSHIP (i32)
/// 4..8    Size         : u32     <- the stride; records are NOT uniform
/// 8..9    Flags        : u8      | PROCESSOR_RELATIONSHIP, when Relationship
/// 9..10   EfficiencyClass : u8   | is RelationProcessorCore
/// ```
///
/// A record whose `Size` is nonsense truncates the walk rather than looping or
/// reading past the end: this buffer comes from the kernel, but the same
/// function is what the tests feed adversarial input to.
#[must_use]
pub fn parse_topology(buf: &[u8]) -> Topology {
    let mut classes: Vec<u8> = Vec::new();
    let mut visited = 0usize;
    let mut off = 0usize;

    while off + 8 <= buf.len() {
        let relationship = i32::from_le_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]]);
        let size =
            u32::from_le_bytes([buf[off + 4], buf[off + 5], buf[off + 6], buf[off + 7]]) as usize;
        // A zero or sub-header size would spin forever; a size past the end
        // means the buffer was truncated under us.
        if size < 8 || off + size > buf.len() {
            break;
        }
        visited += 1;
        if relationship == RELATION_PROCESSOR_CORE && size >= 10 {
            classes.push(buf[off + 9]);
        }
        off += size;
    }

    let physical = classes.len();
    let performance = classes.iter().copied().max().and_then(|top| {
        // One distinct class means a homogeneous CPU.
        (classes.iter().any(|c| *c != top)).then(|| classes.iter().filter(|c| **c == top).count())
    });
    Topology {
        physical,
        performance,
        records_visited: visited,
    }
}

/// The environment a supervised child gets, built from this process's own.
///
/// `vars_os`, not `vars`: `std::env::vars()` **panics** on a variable that is
/// not valid Unicode, and a supervisor that aborts because the user has one odd
/// variable is not a supervisor. On Windows that is not hypothetical — the
/// environment block is UTF-16 and an unpaired surrogate survives in it. A
/// non-UTF-8 variable is dropped rather than passed through, because
/// [`stratum_platform::EnvPolicy::resolve`] speaks `BTreeMap<String, String>`
/// on every platform.
///
/// The four thread variables are forced by `resolve` itself, last, so nothing
/// a caller puts in `overrides` can reopen them.
#[must_use]
pub fn child_env(
    policy: &stratum_platform::EnvPolicy,
) -> std::collections::BTreeMap<String, String> {
    policy.resolve(
        std::env::vars_os()
            .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?))),
    )
}

/// Named so the mapping is reviewable rather than five magic numbers at a call
/// site. `Background` is not a priority class at all — it is
/// `PROCESS_MODE_BACKGROUND_BEGIN`, which lowers I/O priority as well, and is
/// the only thing that stops "Run all stale blocks" from making the editor
/// stutter on a spinning disk.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PriorityRequest {
    /// A plain `SetPriorityClass` value.
    Class(u32),
    /// Enter the background mode (priority *and* I/O).
    BackgroundBegin,
}

/// Map a [`stratum_platform::QosClass`] onto what Windows actually has.
#[must_use]
pub const fn priority_request(qos: stratum_platform::QosClass) -> PriorityRequest {
    use stratum_platform::QosClass as Q;
    match qos {
        // Never HIGH_PRIORITY_CLASS: that class preempts the shell and the
        // input stack, and the UI thread does not need to win against
        // `explorer.exe`.
        Q::UserInteractive => PriorityRequest::Class(0x0000_8000), // ABOVE_NORMAL
        Q::UserInitiated | Q::Default => PriorityRequest::Class(0x0000_0020), // NORMAL
        Q::Utility => PriorityRequest::Class(0x0000_4000),         // BELOW_NORMAL
        Q::Background => PriorityRequest::BackgroundBegin,
    }
}

/// A cache that probes at most once, and says how many times it did.
///
/// Used for the CPU topology and for the login-shell environment: both are
/// answers that cannot change while the process runs, both sit on paths the UI
/// touches (every engine spawn, every Settings paint), and both cost a syscall
/// or a registry walk. The `probes` counter is the ADR-017 assertion —
/// *round-trips*, not milliseconds — and it is readable from a test because the
/// type is pure and generic over the probe.
#[derive(Debug, Default)]
pub struct Probe<T> {
    cell: std::sync::OnceLock<T>,
    probes: std::sync::atomic::AtomicUsize,
}

impl<T: Clone> Probe<T> {
    /// Empty.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            cell: std::sync::OnceLock::new(),
            probes: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// The cached value, probing once if it is not there yet.
    ///
    /// A **failed** probe is not cached. A registry that was momentarily
    /// unreadable, or a notification service that had not started, must not
    /// poison the answer for the rest of a session that may last all day.
    ///
    /// # Errors
    /// Whatever `read` reports.
    pub fn get(&self, read: impl FnOnce() -> Result<T>) -> Result<T> {
        if let Some(v) = self.cell.get() {
            return Ok(v.clone());
        }
        self.probes
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let v = read()?;
        // Losing the race is fine: the other thread's value is equivalent.
        let _ = self.cell.set(v.clone());
        Ok(v)
    }

    /// How many times `read` has been called. Two threads racing the first call
    /// can legitimately make this 2; it is never proportional to call count.
    #[must_use]
    pub fn probes(&self) -> usize {
        self.probes.load(std::sync::atomic::Ordering::Relaxed)
    }
}

#[cfg(target_os = "windows")]
pub use sys::{process_is_in_a_job, WindowsProcessHost};

#[cfg(target_os = "windows")]
mod sys {
    use std::io::{Read, Write};
    use std::os::windows::io::AsRawHandle;
    use std::os::windows::process::CommandExt;
    use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
    use std::time::Duration;

    use stratum_platform::{
        ExitStatus, PlatformError, ProcessHost, ProcessSpec, QosClass, Result, SupervisedChild,
    };
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
    use windows::Win32::System::Console::{
        AttachConsole, FreeConsole, GenerateConsoleCtrlEvent, SetConsoleCtrlHandler,
        CTRL_BREAK_EVENT,
    };
    use windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, IsProcessInJob,
        JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject,
        TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows::Win32::System::SystemInformation::{
        GetLogicalProcessorInformationEx, GlobalMemoryStatusEx, LOGICAL_PROCESSOR_RELATIONSHIP,
        MEMORYSTATUSEX, SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX,
    };
    use windows::Win32::System::Threading::{
        GetCurrentProcess, OpenProcess, SetPriorityClass, WaitForSingleObject,
        PROCESS_CREATION_FLAGS, PROCESS_MODE_BACKGROUND_BEGIN, PROCESS_MODE_BACKGROUND_END,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };

    use super::{
        child_env, creation_flags, parse_topology, priority_request, PriorityRequest, Probe,
        Topology, RELATION_PROCESSOR_CORE,
    };
    use crate::win;

    fn oops(e: &windows::core::Error) -> PlatformError {
        win::classify(e.code().0, e.message())
    }

    /// A kernel handle we own, closed on drop.
    ///
    /// Stored as the raw pointer's integer form because `HANDLE` is a
    /// `*mut c_void` and is therefore neither `Send` nor `Sync`, while
    /// [`SupervisedChild`] is `Send`. The pointer is not dereferenced by us —
    /// it is an opaque kernel index — so moving it between threads is exactly
    /// as safe as the OS says it is.
    #[derive(Debug)]
    struct OwnedHandle(isize);

    // SAFETY: the value is an opaque kernel handle, never dereferenced in this
    // process. Win32 handles are process-wide and usable from any thread.
    unsafe impl Send for OwnedHandle {}

    impl OwnedHandle {
        const fn raw(&self) -> HANDLE {
            HANDLE(self.0 as *mut core::ffi::c_void)
        }
    }

    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            if self.0 != 0 {
                // SAFETY: we created this handle and no copy of it escaped.
                // Closing the last handle to the job is what
                // JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE acts on.
                let _ = unsafe { CloseHandle(self.raw()) };
            }
        }
    }

    /// [`ProcessHost`] for Windows.
    #[derive(Debug, Default)]
    pub struct WindowsProcessHost {
        topology: Probe<Topology>,
    }

    impl WindowsProcessHost {
        /// Construct. Probes nothing until asked.
        #[must_use]
        pub const fn new() -> Self {
            Self {
                topology: Probe::new(),
            }
        }

        /// The cached topology. See [`Probe`] for why the probe count, not the
        /// wall time, is the assertion.
        fn topology(&self) -> Topology {
            self.topology
                .get(|| Ok(read_topology()))
                .unwrap_or_default()
        }
    }

    /// One `GetLogicalProcessorInformationEx` call, sized by the OS.
    ///
    /// The first call is expected to fail with `ERROR_INSUFFICIENT_BUFFER` and
    /// to write the required length; that is the documented protocol, not an
    /// error path.
    fn read_topology() -> Topology {
        let mut len: u32 = 0;
        // SAFETY: a null buffer with a live length pointer is the documented
        // way to ask for the required size.
        let _ = unsafe {
            GetLogicalProcessorInformationEx(
                LOGICAL_PROCESSOR_RELATIONSHIP(RELATION_PROCESSOR_CORE),
                None,
                std::ptr::addr_of_mut!(len),
            )
        };
        if len == 0 {
            return Topology::default();
        }
        let mut buf = vec![0u8; len as usize];
        // SAFETY: `buf` is `len` bytes and stays alive across the call. The
        // cast is to the struct pointer the signature names; the kernel writes
        // a packed sequence of variable-length records into it, which
        // `parse_topology` walks by their own `Size` fields.
        let ok = unsafe {
            GetLogicalProcessorInformationEx(
                LOGICAL_PROCESSOR_RELATIONSHIP(RELATION_PROCESSOR_CORE),
                Some(
                    buf.as_mut_ptr()
                        .cast::<SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX>(),
                ),
                std::ptr::addr_of_mut!(len),
            )
        }
        .is_ok();
        if !ok {
            return Topology::default();
        }
        buf.truncate(len as usize);
        parse_topology(&buf)
    }

    /// Create a job with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`.
    fn create_job() -> Result<OwnedHandle> {
        // SAFETY: an anonymous job with default security.
        let job = unsafe { CreateJobObjectW(None, PCWSTR::null()) }.map_err(|e| oops(&e))?;
        let owned = OwnedHandle(job.0 as isize);

        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: `info` is a live, fully-initialised structure of exactly the
        // length passed, and the class matches its type.
        unsafe {
            SetInformationJobObject(
                owned.raw(),
                JobObjectExtendedLimitInformation,
                std::ptr::addr_of!(info).cast(),
                u32::try_from(std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>())
                    .unwrap_or(u32::MAX),
            )
        }
        .map_err(|e| oops(&e))?;

        // Read the flag back. `SetInformationJobObject` succeeds under a
        // nesting policy that then drops the limit, and a job that reports
        // itself armed while it is not is precisely the failure this whole
        // mechanism exists to prevent — the engine would leak on a hard kill
        // with nothing in the logs. One extra syscall per engine launch.
        let mut back = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        // SAFETY: `back` is a live structure of exactly the length passed.
        unsafe {
            QueryInformationJobObject(
                Some(owned.raw()),
                JobObjectExtendedLimitInformation,
                std::ptr::addr_of_mut!(back).cast(),
                u32::try_from(std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>())
                    .unwrap_or(u32::MAX),
                None,
            )
        }
        .map_err(|e| oops(&e))?;
        if back.BasicLimitInformation.LimitFlags & JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
            != JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
        {
            return Err(PlatformError::BackendUnavailable(
                "the job object did not keep JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE; a supervised \
                 engine would outlive a hard-killed Stratum"
                    .to_owned(),
            ));
        }
        Ok(owned)
    }

    /// Whether a process is inside any job object.
    ///
    /// Public so that `tests/job_object.rs` can observe the supervision from
    /// outside rather than trusting that `spawn_supervised` did what it says.
    /// A `NULL` job handle is the documented way to ask "any job at all".
    ///
    /// # Errors
    /// [`PlatformError::PermissionDenied`] for a process this session may not
    /// open, [`PlatformError::Os`] otherwise.
    pub fn process_is_in_a_job(pid: u32) -> Result<bool> {
        // SAFETY: the narrowest access right that answers the question; the
        // handle is closed by `OwnedHandle` on the way out.
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }
            .map_err(|e| oops(&e))?;
        let owned = OwnedHandle(handle.0 as isize);
        let mut in_job = windows::core::BOOL(0);
        // SAFETY: `owned` is a live process handle; `None` asks about any job.
        unsafe { IsProcessInJob(owned.raw(), None, std::ptr::addr_of_mut!(in_job)) }
            .map_err(|e| oops(&e))?;
        Ok(in_job.as_bool())
    }

    impl ProcessHost for WindowsProcessHost {
        fn spawn_supervised(&self, spec: ProcessSpec) -> Result<Box<dyn SupervisedChild>> {
            let env = child_env(&spec.env);

            let mut cmd = Command::new(spec.program.as_std_path());
            cmd.args(&spec.args)
                .env_clear()
                .envs(&env)
                .creation_flags(creation_flags())
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                // Inherited on purpose: the engine's tracing goes to the
                // supervisor's stderr, where a crash reporter can find it.
                .stderr(Stdio::inherit());
            if let Some(dir) = &spec.cwd {
                cmd.current_dir(dir.as_std_path());
            }

            // The job is created BEFORE the spawn so that a failure to set it
            // up never leaves an unsupervised engine running.
            let job = if spec.kill_on_parent_exit {
                Some(create_job()?)
            } else {
                None
            };

            let mut child = cmd.spawn()?;
            if let Some(job) = &job {
                let handle = HANDLE(child.as_raw_handle());
                // SAFETY: `child` owns the process handle and outlives this
                // call. Assignment after spawn leaves a window of a few
                // microseconds in which the child could itself spawn something
                // outside the job; `std` cannot create a suspended process, and
                // closing that window is not worth reimplementing
                // `CreateProcessW`. The engine does no work before its first
                // stdin frame, which we have not sent yet.
                if let Err(e) = unsafe { AssignProcessToJobObject(job.raw(), handle) } {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(oops(&e));
                }
                // Confirm the assignment rather than assume it. A child that
                // is running but unsupervised is worse than one that failed to
                // start: it looks healthy and leaks on every crash.
                let mut in_job = windows::core::BOOL(0);
                // SAFETY: both handles are live for the duration of the call.
                let checked = unsafe {
                    IsProcessInJob(handle, Some(job.raw()), std::ptr::addr_of_mut!(in_job))
                };
                if checked.is_err() || !in_job.as_bool() {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(PlatformError::BackendUnavailable(
                        "the child did not join its job object; it would outlive a hard-killed \
                         Stratum"
                            .to_owned(),
                    ));
                }
            }

            let pid = child.id();
            let stdin = child.stdin.take().ok_or_else(|| {
                PlatformError::BackendUnavailable("child stdin was not piped".to_owned())
            })?;
            let stdout = child.stdout.take().ok_or_else(|| {
                PlatformError::BackendUnavailable("child stdout was not piped".to_owned())
            })?;

            Ok(Box::new(WindowsChild {
                child,
                pid,
                job,
                stdin,
                stdout,
            }))
        }

        fn physical_cores(&self) -> usize {
            // Never `dwNumberOfProcessors`: see the module docs.
            self.topology().physical.max(1)
        }

        fn performance_cores(&self) -> Option<usize> {
            self.topology().performance
        }

        fn set_process_qos(&self, qos: QosClass) -> Result<()> {
            // SAFETY: a pseudo-handle constant; not a resource to close.
            let me = unsafe { GetCurrentProcess() };
            match priority_request(qos) {
                PriorityRequest::BackgroundBegin => {
                    // SAFETY: `me` is the current-process pseudo-handle.
                    match unsafe { SetPriorityClass(me, PROCESS_MODE_BACKGROUND_BEGIN) } {
                        Ok(()) => Ok(()),
                        // Already there. The caller asked for a state.
                        Err(e)
                            if win::win32_code(e.code().0)
                                == Some(win::ERROR_PROCESS_MODE_ALREADY_BACKGROUND) =>
                        {
                            Ok(())
                        }
                        Err(e) => Err(oops(&e)),
                    }
                }
                PriorityRequest::Class(class) => {
                    // Leaving background mode is a separate call from setting a
                    // class, and skipping it leaves the process with a lowered
                    // I/O priority that no priority class can restore.
                    // SAFETY: as above.
                    let _ = unsafe { SetPriorityClass(me, PROCESS_MODE_BACKGROUND_END) };
                    // SAFETY: as above; `class` is one of the documented
                    // priority-class constants.
                    unsafe { SetPriorityClass(me, PROCESS_CREATION_FLAGS(class)) }
                        .map_err(|e| oops(&e))
                }
            }
        }

        fn available_memory(&self) -> Option<u64> {
            let mut m = MEMORYSTATUSEX {
                dwLength: u32::try_from(std::mem::size_of::<MEMORYSTATUSEX>()).ok()?,
                ..Default::default()
            };
            // SAFETY: `dwLength` is set to the struct's own size, which is the
            // one precondition of this call.
            unsafe { GlobalMemoryStatusEx(std::ptr::addr_of_mut!(m)) }.ok()?;
            // Physical, not virtual. On 64-bit Windows `ullAvailVirtual` is
            // terabytes and answering with it would make the out-of-core
            // threshold never fire; what we are actually asking is "how much
            // can we take without paging".
            Some(m.ullAvailPhys)
        }
    }

    /// A running child, alone in its own job and its own process group.
    struct WindowsChild {
        child: Child,
        pid: u32,
        job: Option<OwnedHandle>,
        stdin: ChildStdin,
        stdout: ChildStdout,
    }

    impl SupervisedChild for WindowsChild {
        fn pid(&self) -> u32 {
            self.pid
        }

        fn stdin(&mut self) -> &mut (dyn Write + Send) {
            &mut self.stdin
        }

        fn stdout(&mut self) -> &mut (dyn Read + Send) {
            &mut self.stdout
        }

        fn interrupt(&self) -> Result<()> {
            send_ctrl_break(self.pid)
        }

        fn terminate_tree(&self) -> Result<()> {
            let Some(job) = &self.job else {
                return Err(PlatformError::Unsupported(
                    "this child was spawned without kill_on_parent_exit, so it has no job object \
                     and its grandchildren cannot be reached",
                ));
            };
            // The job, not the process: a do-file that ran `shell python …`
            // has a grandchild holding the dataset's file handles, and
            // `TerminateProcess` on the engine alone leaks it. This is the
            // whole reason W07 could not implement `terminate_tree` and left
            // it to this crate.
            // SAFETY: a job handle we created and still own.
            //
            // No "already gone" arm: `TerminateJobObject` on a job whose
            // processes have all exited SUCCEEDS, so there is no error here to
            // swallow, and `ERROR_ACCESS_DENIED` on a job we created ourselves
            // would mean something took our handle. Reporting it as `Ok(())`
            // would be the silent-success failure this layer exists to avoid.
            unsafe { TerminateJobObject(job.raw(), 1) }.map_err(|e| oops(&e))
        }

        fn wait_timeout(&mut self, d: Duration) -> Result<Option<ExitStatus>> {
            let ms = u32::try_from(d.as_millis()).unwrap_or(u32::MAX);
            let handle = HANDLE(self.child.as_raw_handle());
            // SAFETY: `self.child` owns the handle for the duration of the
            // call. A wait, unlike a poll loop, costs nothing while it waits.
            let r = unsafe { WaitForSingleObject(handle, ms) };
            if r == WAIT_TIMEOUT {
                return Ok(None);
            }
            if r != WAIT_OBJECT_0 {
                let e = windows::core::Error::from_thread();
                return Err(oops(&e));
            }
            // The wait only says "signalled"; the code comes from `std`, which
            // also reaps.
            Ok(self.child.try_wait()?.map(|s| ExitStatus {
                code: s.code(),
                // Windows has no signals. `terminate_tree` shows up as exit
                // code 1, which is what we passed to `TerminateJobObject`.
                signal: None,
            }))
        }
    }

    /// `GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT)` to the child's group.
    ///
    /// The wart this function exists for: a control event can only be sent by a
    /// process attached to the **same console** as the target, and
    /// `CREATE_NO_WINDOW` gives the child its own. A GUI supervisor therefore
    /// has to attach to the child's console for the duration of the call. The
    /// `SetConsoleCtrlHandler(None, true)` around it is belt and braces —
    /// a break addressed to the child's group cannot reach ours, but we are
    /// briefly sharing its console and the cost of being sure is one call.
    fn send_ctrl_break(pid: u32) -> Result<()> {
        // SAFETY: attaching to a console we do not own is exactly what this
        // entry point is for; failure is reported, not assumed away.
        let attached = unsafe { AttachConsole(pid) };
        match &attached {
            Ok(()) => {}
            // We already have a console of our own — a console build of the
            // supervisor. The child inherited it, so the event can go direct.
            Err(e) if win::win32_code(e.code().0) == Some(win::ERROR_ACCESS_DENIED) => {}
            // The child has no console at all; there is nothing to interrupt
            // cooperatively and saying so beats pretending we tried.
            Err(e) => return Err(oops(e)),
        }

        // SAFETY: a null handler with `add = true` means "ignore control
        // events in this process", which is the documented idiom here.
        let guarded = unsafe { SetConsoleCtrlHandler(None, true) }.is_ok();
        // SAFETY: the process group id equals the child's pid because it was
        // spawned with CREATE_NEW_PROCESS_GROUP.
        let sent = unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, pid) };
        if guarded {
            // SAFETY: removing the handler we just added.
            let _ = unsafe { SetConsoleCtrlHandler(None, false) };
        }
        if attached.is_ok() {
            // SAFETY: detaching the console we attached above. Never called
            // when the attach failed, so a console build keeps its own.
            let _ = unsafe { FreeConsole() };
        }
        sent.map_err(|e| oops(&e))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    /// Build one `SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX` record.
    ///
    /// `size` is deliberately a parameter: the real kernel emits records whose
    /// length varies with `GroupCount`, and a walk that assumes a fixed stride
    /// works on a laptop and silently miscounts on a two-group server.
    fn record(relationship: i32, size: usize, efficiency_class: u8) -> Vec<u8> {
        let mut r = vec![0u8; size];
        r[0..4].copy_from_slice(&relationship.to_le_bytes());
        r[4..8].copy_from_slice(&(size as u32).to_le_bytes());
        if size >= 10 {
            r[9] = efficiency_class;
        }
        r
    }

    /// A record whose header claims `declared` bytes, whatever its real
    /// length. The kernel never emits one; the harness does.
    fn header_claiming(declared: u32) -> Vec<u8> {
        let mut r = vec![0u8; 8];
        r[0..4].copy_from_slice(&RELATION_PROCESSOR_CORE.to_le_bytes());
        r[4..8].copy_from_slice(&declared.to_le_bytes());
        r
    }

    fn buffer(records: &[Vec<u8>]) -> Vec<u8> {
        records.iter().flatten().copied().collect()
    }

    /// A homogeneous eight-core part: eight physical cores, nothing to prefer.
    #[test]
    fn a_homogeneous_cpu_reports_cores_and_no_preference() {
        let buf = buffer(&vec![record(RELATION_PROCESSOR_CORE, 48, 0); 8]);
        let t = parse_topology(&buf);
        assert_eq!(t.physical, 8);
        assert_eq!(t.performance, None);
        assert_eq!(t.records_visited, 8);
    }

    /// An Intel P/E part: 8 performance cores at efficiency class 1 and 16
    /// efficiency cores at class 0. `physical_cores` must still be 24 — every
    /// core is a real core — while `performance_cores` is 8. This is the shape
    /// no machine in this project can produce on demand, which is exactly why
    /// the walk is a pure function.
    #[test]
    fn a_heterogeneous_cpu_separates_performance_cores() {
        let mut rs = vec![record(RELATION_PROCESSOR_CORE, 48, 1); 8];
        rs.extend(vec![record(RELATION_PROCESSOR_CORE, 48, 0); 16]);
        let t = parse_topology(&buffer(&rs));
        assert_eq!(t.physical, 24);
        assert_eq!(t.performance, Some(8));
        assert_eq!(t.records_visited, 24);
    }

    /// THE COUNTER (ADR-017). Records visited equals records present — one
    /// linear pass, no rescanning — and the stride comes from each record's
    /// own `Size`, so a buffer of mixed-length records is walked correctly.
    #[test]
    fn the_walk_uses_each_records_own_size_and_visits_each_once() {
        let rs = vec![
            record(RELATION_PROCESSOR_CORE, 48, 0),
            record(RELATION_PROCESSOR_CORE, 80, 1), // two group masks
            record(RELATION_PROCESSOR_CORE, 48, 1),
        ];
        let buf = buffer(&rs);
        let t = parse_topology(&buf);
        assert_eq!(t.records_visited, 3);
        assert_eq!(t.physical, 3);
        assert_eq!(t.performance, Some(2));
    }

    /// The kernel will not do this; the test harness does, and a walk that
    /// spins or reads past the end on a zero `Size` is a hang in the engine
    /// supervisor rather than a wrong number.
    #[test]
    fn a_malformed_record_truncates_the_walk_rather_than_looping() {
        let mut buf = buffer(&[record(RELATION_PROCESSOR_CORE, 48, 0)]);
        buf.extend(header_claiming(0));
        buf.extend(record(RELATION_PROCESSOR_CORE, 48, 0));
        let t = parse_topology(&buf);
        assert_eq!(t.records_visited, 1);
        assert_eq!(t.physical, 1);

        // A stride shorter than the header would also step backwards.
        let mut tiny = buffer(&[record(RELATION_PROCESSOR_CORE, 48, 0)]);
        tiny.extend(header_claiming(4));
        assert_eq!(parse_topology(&tiny).records_visited, 1);

        // A record claiming more bytes than remain.
        let mut short = record(RELATION_PROCESSOR_CORE, 48, 0);
        short.truncate(20);
        assert_eq!(parse_topology(&short).records_visited, 0);
        assert_eq!(parse_topology(&[]).records_visited, 0);
    }

    /// Relations other than `RelationProcessorCore` are visited (they consume
    /// stride) but are not cores. Asking for one relation does not guarantee
    /// the kernel returns only that one on every Windows build.
    #[test]
    fn only_processor_core_records_are_counted_as_cores() {
        let buf = buffer(&[
            record(RELATION_PROCESSOR_CORE, 48, 0),
            record(2 /* RelationCache */, 64, 3),
            record(RELATION_PROCESSOR_CORE, 48, 0),
        ]);
        let t = parse_topology(&buf);
        assert_eq!(t.physical, 2);
        assert_eq!(t.records_visited, 3);
    }

    /// `CREATE_NO_WINDOW` alone produces a child that cannot be interrupted;
    /// `CREATE_NEW_PROCESS_GROUP` alone flashes a console over the IDE on every
    /// engine launch. They are one decision.
    #[test]
    fn the_creation_flags_are_both_of_them() {
        assert_eq!(creation_flags(), 0x0800_0200);
        assert_eq!(
            creation_flags() & CREATE_NEW_PROCESS_GROUP,
            CREATE_NEW_PROCESS_GROUP
        );
        assert_eq!(creation_flags() & CREATE_NO_WINDOW, CREATE_NO_WINDOW);
    }

    #[test]
    fn background_is_a_mode_not_a_priority_class() {
        use stratum_platform::QosClass;
        assert_eq!(
            priority_request(QosClass::Background),
            PriorityRequest::BackgroundBegin
        );
        assert_eq!(
            priority_request(QosClass::Utility),
            PriorityRequest::Class(0x0000_4000)
        );
        // Never HIGH_PRIORITY_CLASS (0x80): it preempts the input stack.
        assert!(matches!(
            priority_request(QosClass::UserInteractive),
            PriorityRequest::Class(c) if c != 0x80
        ));
    }

    /// THE COUNTER (ADR-017). The CPU topology and the login-shell environment
    /// are probed **once**, not once per engine spawn and not once per Settings
    /// paint, and the assertion is the round-trip count rather than a duration.
    #[test]
    fn a_probe_reads_once_however_often_it_is_asked() {
        let p: Probe<usize> = Probe::new();
        for _ in 0..1000 {
            assert_eq!(p.get(|| Ok(41 + 1)).unwrap(), 42);
        }
        assert_eq!(p.probes(), 1);
    }

    /// A transient failure must not poison the answer for the rest of a session
    /// that may last all day — the Settings pane opening one second before the
    /// notification service starts is a real sequence.
    #[test]
    fn a_failed_probe_is_not_cached() {
        let p: Probe<usize> = Probe::new();
        assert!(p
            .get(|| Err(stratum_platform::PlatformError::Unsupported("nope")))
            .is_err());
        assert_eq!(p.get(|| Ok(7)).unwrap(), 7);
        assert_eq!(p.get(|| Ok(9)).unwrap(), 7);
        assert_eq!(p.probes(), 2);
    }
}
