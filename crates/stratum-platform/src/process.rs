//! Supervised child processes and CPU topology — 08 §5.6.
//!
//! The desktop supervises `stratum serve` as a child. This is the crux of §26
//! (multi-window) and §12/§13 (interrupting a running block without killing the
//! IDE), and it is an adapter rather than `std::process` because the three
//! OSes disagree about every part of it: Windows needs a Job Object with
//! `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` plus `CREATE_NEW_PROCESS_GROUP` for
//! `CTRL_BREAK_EVENT`; macOS needs `setpgid` in `pre_exec` and `killpg`; Linux
//! needs `prctl(PR_SET_PDEATHSIG)`.
//!
//! # `physical_cores`, not the logical count
//!
//! Returning the logical count here is the classic "our stats app is slower on
//! the 24-core machine" bug: every BLAS-shaped library sizes its thread pool
//! from it, they multiply, and the machine spends its time in the scheduler.
//! On Apple Silicon the right answer is `hw.perflevel0.logicalcpu` — the
//! performance-core count — not `hw.ncpu`, because the efficiency cores make a
//! parallel reduction wait on its slowest chunk.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::time::Duration;

use camino::Utf8PathBuf;

use crate::Result;

/// How a child's environment is built.
///
/// There is no `Default`: the thread count is the one field nobody should get
/// by accident, so [`EnvPolicy::with_threads`] is the only constructor.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct EnvPolicy {
    /// Start from the parent's environment. Off only for a hermetic child.
    pub inherit: bool,
    /// The `--threads` value. Written into all four of
    /// [`EnvPolicy::FORCED_THREAD_VARS`].
    pub threads: usize,
    /// Extra variables, applied after the inherited set.
    pub overrides: BTreeMap<String, String>,
    /// Variables to strip from the inherited set — a secret the child has no
    /// business seeing, or a `STATA*` variable that would change semantics.
    pub remove: BTreeSet<String>,
}

impl EnvPolicy {
    /// The four thread-count variables that are FORCED on every spawned child.
    ///
    /// Leaving these unset is, measurably, the single most common cause of a
    /// statistics application being slower on many cores than on few: each
    /// library defaults its pool to the logical core count, and two such pools
    /// on one machine oversubscribe it by construction.
    pub const FORCED_THREAD_VARS: [&'static str; 4] = [
        "OMP_NUM_THREADS",
        "OPENBLAS_NUM_THREADS",
        "MKL_NUM_THREADS",
        "RAYON_NUM_THREADS",
    ];

    /// Inherit the parent environment and pin the thread count. `threads` is
    /// clamped to at least 1, because `OMP_NUM_THREADS=0` is undefined
    /// behaviour in every implementation that reads it.
    #[must_use]
    pub fn with_threads(threads: usize) -> Self {
        Self {
            inherit: true,
            threads: threads.max(1),
            overrides: BTreeMap::new(),
            remove: BTreeSet::new(),
        }
    }

    /// Add an override.
    #[must_use]
    pub fn set(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.overrides.insert(key.into(), value.into());
        self
    }

    /// Strip a variable from the inherited environment.
    #[must_use]
    pub fn unset(mut self, key: impl Into<String>) -> Self {
        self.remove.insert(key.into());
        self
    }

    /// Compute the child's environment from the parent's.
    ///
    /// Order is deliberate and is the contract: inherited → removals →
    /// overrides → **forced thread variables last**. A caller cannot
    /// accidentally (or deliberately) put `OMP_NUM_THREADS` back to the machine
    /// width through `overrides`; if a build ever needs to, it changes
    /// `threads`, which is the honest way to say it.
    #[must_use]
    pub fn resolve<I, K, V>(&self, inherited: I) -> BTreeMap<String, String>
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        let mut env: BTreeMap<String, String> = if self.inherit {
            inherited
                .into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect()
        } else {
            BTreeMap::new()
        };
        for k in &self.remove {
            env.remove(k);
        }
        for (k, v) in &self.overrides {
            env.insert(k.clone(), v.clone());
        }
        let n = self.threads.max(1).to_string();
        for k in Self::FORCED_THREAD_VARS {
            env.insert(k.to_owned(), n.clone());
        }
        env
    }
}

/// What to spawn.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ProcessSpec {
    /// Absolute path to the executable.
    pub program: Utf8PathBuf,
    /// Arguments, not including `argv[0]`.
    pub args: Vec<String>,
    /// Working directory. `None` inherits.
    pub cwd: Option<Utf8PathBuf>,
    /// Environment policy.
    pub env: EnvPolicy,
    /// The important one. When true the child — and everything it spawned —
    /// dies with us even if we crash rather than exit. Job object on Windows,
    /// `PR_SET_PDEATHSIG` on Linux, a watchdog on the process group on macOS.
    pub kill_on_parent_exit: bool,
}

impl ProcessSpec {
    /// A supervised engine child: inherit, pin threads, die with the parent.
    #[must_use]
    pub fn new(program: impl Into<Utf8PathBuf>, threads: usize) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            cwd: None,
            env: EnvPolicy::with_threads(threads),
            kill_on_parent_exit: true,
        }
    }

    /// Append an argument.
    #[must_use]
    pub fn arg(mut self, a: impl Into<String>) -> Self {
        self.args.push(a.into());
        self
    }

    /// Set the working directory.
    #[must_use]
    pub fn cwd(mut self, dir: impl Into<Utf8PathBuf>) -> Self {
        self.cwd = Some(dir.into());
        self
    }
}

/// Scheduling priority. macOS maps these to QoS classes, Windows to priority
/// classes plus `PROCESS_MODE_BACKGROUND_BEGIN`, Linux to `nice` and
/// `ioprio`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum QosClass {
    /// The UI thread. Never used for the engine.
    UserInteractive,
    /// A run the user is watching.
    UserInitiated,
    /// The OS default.
    Default,
    /// "Run all stale blocks" while the user works elsewhere.
    Utility,
    /// Indexing, warm caches.
    Background,
}

/// How a child ended. Our own type, not `std::process::ExitStatus`: Windows has
/// no signal number and we need to distinguish "exited 1" from "killed" on both.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ExitStatus {
    /// The exit code, when it exited normally.
    pub code: Option<i32>,
    /// The signal that killed it, on Unix.
    pub signal: Option<i32>,
}

impl ExitStatus {
    /// Exited with status 0.
    #[must_use]
    pub const fn success(&self) -> bool {
        matches!(self.code, Some(0)) && self.signal.is_none()
    }
}

/// A running child we own.
pub trait SupervisedChild: Send {
    /// The OS process id, for logs and for the crash reporter.
    fn pid(&self) -> u32;

    /// The child's stdin. Always piped: the control plane is framed
    /// MessagePack over stdio (CONTRACTS §10).
    fn stdin(&mut self) -> &mut (dyn Write + Send);

    /// The child's stdout.
    fn stdout(&mut self) -> &mut (dyn Read + Send);

    /// Cooperative cancel: `SIGINT` to the process group on Unix,
    /// `GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT)` on Windows. The engine
    /// turns this into `BlockState::Interrupted` (§12) — it is not a kill, and
    /// a well-behaved child keeps running.
    ///
    /// # Errors
    /// [`crate::PlatformError::Os`] if the signal could not be delivered.
    fn interrupt(&self) -> Result<()>;

    /// Hard kill of the child AND everything it spawned. A do-file may `shell`
    /// out, and orphaning that grandchild leaks a process holding the dataset's
    /// file handles.
    ///
    /// # Errors
    /// [`crate::PlatformError::Os`] on failure. Killing an already-dead child
    /// is `Ok(())`.
    fn terminate_tree(&self) -> Result<()>;

    /// Wait up to `d`. `Ok(None)` means still running.
    ///
    /// # Errors
    /// [`crate::PlatformError::Os`] if the wait itself failed.
    fn wait_timeout(&mut self, d: Duration) -> Result<Option<ExitStatus>>;
}

/// Spawning and CPU topology.
pub trait ProcessHost: Send + Sync {
    /// Spawn a supervised child with piped stdio.
    ///
    /// # Errors
    /// [`crate::PlatformError::Io`] when the program is missing or not
    /// executable; [`crate::PlatformError::Os`] when the OS-specific
    /// supervision (job object, process group) could not be set up.
    fn spawn_supervised(&self, spec: ProcessSpec) -> Result<Box<dyn SupervisedChild>>;

    /// Cores worth running a numeric kernel on — never the logical count. See
    /// the module docs.
    fn physical_cores(&self) -> usize;

    /// The performance-core count on a heterogeneous CPU (Apple Silicon,
    /// Intel P/E). `None` when the CPU is homogeneous, which is the answer the
    /// scheduler wants: "there is nothing to prefer".
    fn performance_cores(&self) -> Option<usize>;

    /// Set the *calling process's* scheduling class.
    ///
    /// # Errors
    /// [`crate::PlatformError::Unsupported`] where the OS has no equivalent;
    /// [`crate::PlatformError::Os`] when the call failed.
    fn set_process_qos(&self, qos: QosClass) -> Result<()>;

    /// Bytes of memory a large allocation could plausibly get right now.
    /// `None` when the OS will not say — an estimate that is wrong is worse
    /// than no estimate, because the out-of-core threshold is derived from it.
    fn available_memory(&self) -> Option<u64>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inherited() -> Vec<(&'static str, &'static str)> {
        vec![
            ("PATH", "/usr/bin"),
            ("OMP_NUM_THREADS", "64"),
            ("SECRET", "hunter2"),
        ]
    }

    #[test]
    fn the_four_thread_vars_are_forced_on_every_child() {
        let env = EnvPolicy::with_threads(6).resolve(inherited());
        for k in EnvPolicy::FORCED_THREAD_VARS {
            assert_eq!(env.get(k).map(String::as_str), Some("6"), "{k}");
        }
        assert_eq!(env.get("PATH").map(String::as_str), Some("/usr/bin"));
    }

    /// The inherited `OMP_NUM_THREADS=64` from a user's shell profile is
    /// exactly the value that produces the oversubscription bug.
    #[test]
    fn an_inherited_thread_var_is_overwritten_not_respected() {
        let env = EnvPolicy::with_threads(2).resolve(inherited());
        assert_eq!(env["OMP_NUM_THREADS"], "2");
    }

    #[test]
    fn an_override_cannot_reopen_the_thread_vars() {
        let env = EnvPolicy::with_threads(3)
            .set("OMP_NUM_THREADS", "64")
            .set("STRATUM_LOG", "debug")
            .resolve(inherited());
        assert_eq!(env["OMP_NUM_THREADS"], "3");
        assert_eq!(env["STRATUM_LOG"], "debug");
    }

    #[test]
    fn removals_happen_before_overrides() {
        let env = EnvPolicy::with_threads(1)
            .unset("SECRET")
            .resolve(inherited());
        assert!(!env.contains_key("SECRET"));
    }

    #[test]
    fn no_inherit_starts_empty_but_still_forces_threads() {
        let mut p = EnvPolicy::with_threads(4);
        p.inherit = false;
        let env = p.resolve(inherited());
        assert!(!env.contains_key("PATH"));
        assert_eq!(env.len(), EnvPolicy::FORCED_THREAD_VARS.len());
    }

    #[test]
    fn zero_threads_is_clamped_to_one() {
        let env = EnvPolicy::with_threads(0).resolve(Vec::<(String, String)>::new());
        assert_eq!(env["RAYON_NUM_THREADS"], "1");
    }
}
