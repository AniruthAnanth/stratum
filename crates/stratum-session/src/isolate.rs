//! Clean-run isolation — `Isolation::Subprocess`, and the write sandbox.
//!
//! ARCHITECTURE §7.7 ends with the sentence this module exists to satisfy:
//!
//! > `Isolation::Subprocess` (a real `stratum run --clean` child) is the **only**
//! > thing that may set the §16 tick "✓ File runs from clean state". We never
//! > infer it from static analysis.
//!
//! Two halves, and they are independent on purpose. [`WriteSandbox`] is a pure
//! path function with no I/O at all, so the "does `save` escape?" question is
//! answered by a unit test rather than by watching a filesystem; [`CleanRun`] is
//! the process half — spawn, stream NDJSON, reap.
//!
//! # Why the sandbox is a redirect and not a refusal
//!
//! Refusing `save` would make the verification run take a *different path
//! through the file* from the run it is verifying: `capture save` would set a
//! non-zero `_rc`, an `if _rc` branch would go the other way, and the thing we
//! proved runs clean is not the thing the user runs. Redirecting keeps control
//! flow identical and keeps the user's outputs intact, which is what design `07`
//! §10's R01 asks for.

use std::io::{BufRead, BufReader, Read};
use std::process::{Command, Stdio};

use camino::{Utf8Path, Utf8PathBuf};

pub use stratum_proto::Isolation;

/// A verb that writes something outside the engine.
///
/// The list is design `07` §10's, verbatim: "`save`, `export`, `outfile`,
/// `erase`, `rmdir`, `copy`, `shell`/`!`, and `putexcel`", plus the two file
/// handles a do-file can hold open. Adding a write verb to the runtime without
/// adding it here is a sandbox escape, which is why this is a closed enum and
/// [`WriteVerb::ALL`] exists for the test to iterate.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum WriteVerb {
    /// `save`, `saveold`.
    Save,
    /// `export delimited`, `export excel`, `export sasxport`.
    Export,
    /// `outfile`.
    Outfile,
    /// `erase`, `rm`.
    Erase,
    /// `rmdir`.
    Rmdir,
    /// `copy` — the destination side.
    Copy,
    /// `shell`, `!`, `winexec`.
    Shell,
    /// `putexcel`.
    Putexcel,
    /// `graph export`.
    GraphExport,
    /// `file write` / `file open ... , write|append`.
    FileWrite,
    /// `postfile`.
    Postfile,
}

impl WriteVerb {
    /// Every write verb the sandbox covers.
    pub const ALL: [WriteVerb; 11] = [
        WriteVerb::Save,
        WriteVerb::Export,
        WriteVerb::Outfile,
        WriteVerb::Erase,
        WriteVerb::Rmdir,
        WriteVerb::Copy,
        WriteVerb::Shell,
        WriteVerb::Putexcel,
        WriteVerb::GraphExport,
        WriteVerb::FileWrite,
        WriteVerb::Postfile,
    ];

    /// The Stata command word, for diagnostics.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            WriteVerb::Save => "save",
            WriteVerb::Export => "export",
            WriteVerb::Outfile => "outfile",
            WriteVerb::Erase => "erase",
            WriteVerb::Rmdir => "rmdir",
            WriteVerb::Copy => "copy",
            WriteVerb::Shell => "shell",
            WriteVerb::Putexcel => "putexcel",
            WriteVerb::GraphExport => "graph export",
            WriteVerb::FileWrite => "file write",
            WriteVerb::Postfile => "postfile",
        }
    }

    /// True for the verbs that name a path. `shell` is the one that does not,
    /// and it is contained rather than redirected.
    #[must_use]
    pub fn has_path(self) -> bool {
        !matches!(self, WriteVerb::Shell)
    }
}

/// Where a sandboxed write actually goes.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Sandboxed {
    /// The path verb writes here instead.
    Path(Utf8PathBuf),
    /// `shell` runs with this working directory and with `TMPDIR`/`TEMP`/`TMP`
    /// pointing inside the scratch root, so a subprocess that writes a relative
    /// path or a temp file still lands in the sandbox.
    Contained {
        /// The child's working directory.
        cwd: Utf8PathBuf,
        /// Environment overrides to apply to the child, sorted by name.
        env: Vec<(String, String)>,
    },
}

impl Sandboxed {
    /// The path this redirect resolves to — the shell's cwd for
    /// [`Sandboxed::Contained`].
    #[must_use]
    pub fn path(&self) -> &Utf8Path {
        match self {
            Sandboxed::Path(p) => p.as_path(),
            Sandboxed::Contained { cwd, .. } => cwd.as_path(),
        }
    }
}

/// Redirects every write verb into a scratch directory.
///
/// The mapping is *injective and total*: two different source paths never land
/// on one sandbox path, so a do-file that writes `a/out.dta` and `b/out.dta`
/// still writes two files, and a do-file that then `use`s what it saved finds
/// it. That is what makes a sandboxed run take the same branch as the real one.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct WriteSandbox {
    root: Utf8PathBuf,
    /// Relative paths are resolved against this before mapping, so that the
    /// mapping does not depend on the *process* cwd — which a second session in
    /// the same engine is free to have changed.
    base: Utf8PathBuf,
}

impl WriteSandbox {
    /// A sandbox rooted at `root`, resolving relative paths against `base`
    /// (the session's cwd — checklist item 7).
    #[must_use]
    pub fn new(root: impl Into<Utf8PathBuf>, base: impl Into<Utf8PathBuf>) -> Self {
        Self {
            root: root.into(),
            base: base.into(),
        }
    }

    /// The scratch root.
    #[must_use]
    pub fn root(&self) -> &Utf8Path {
        self.root.as_path()
    }

    /// Where `verb` on `path` actually writes.
    ///
    /// `path` is ignored for [`WriteVerb::Shell`], which has none.
    #[must_use]
    pub fn redirect(&self, verb: WriteVerb, path: &Utf8Path) -> Sandboxed {
        if !verb.has_path() {
            let cwd = self.root.join("shell");
            let tmp = self.root.join("tmp");
            let mut env: Vec<(String, String)> = ["TMPDIR", "TEMP", "TMP"]
                .into_iter()
                .map(|k| (k.to_owned(), tmp.to_string()))
                .collect();
            env.sort();
            return Sandboxed::Contained { cwd, env };
        }
        Sandboxed::Path(self.map_path(path))
    }

    /// True when `path` is already inside the scratch root — a sandboxed run
    /// that reads back what it wrote must not be redirected twice.
    #[must_use]
    pub fn contains(&self, path: &Utf8Path) -> bool {
        path.starts_with(&self.root)
    }

    /// The path mapping, exposed because it is the whole security property and
    /// deserves its own assertions.
    ///
    /// A source path becomes `<root>/fs/<flattened>`, where the flattening
    /// replaces the root marker, every `..` and every drive prefix with a
    /// literal component. Nothing is dropped, so the mapping stays injective;
    /// nothing is interpreted, so `../../etc/passwd` cannot climb out.
    #[must_use]
    pub fn map_path(&self, path: &Utf8Path) -> Utf8PathBuf {
        if self.contains(path) {
            return path.to_owned();
        }
        // `has_root()` as well as `is_absolute()`: on Windows `/etc/passwd`
        // has a root but no prefix, so `is_absolute()` is false for it and
        // `join` would then splice it onto the base in a way that depends on
        // the platform. The sandbox must map one input to one output on all
        // three, so anything already rooted is taken as-is.
        let absolute = if path.is_absolute() || path.has_root() {
            path.to_owned()
        } else {
            self.base.join(path)
        };
        let mut out = self.root.join("fs");
        for c in absolute.components() {
            match c {
                camino::Utf8Component::RootDir => out.push("_root"),
                camino::Utf8Component::CurDir => out.push("_cur"),
                camino::Utf8Component::ParentDir => out.push("_up"),
                camino::Utf8Component::Prefix(p) => {
                    // A Windows drive or UNC prefix. Kept, with its separators
                    // neutralised, so `C:\x` and `D:\x` stay distinct.
                    out.push(p.as_str().replace([':', '\\', '/'], "_"));
                }
                camino::Utf8Component::Normal(s) => out.push(s),
            }
        }
        out
    }
}

/// What went wrong spawning or reaping a clean-run child.
#[derive(Debug, thiserror::Error)]
pub enum IsolateError {
    /// The child could not be spawned at all.
    #[error("could not spawn {program}: {source}")]
    Spawn {
        /// The program we tried to run.
        program: String,
        /// The OS error.
        #[source]
        source: std::io::Error,
    },
    /// Reading the child's stdout failed part-way through.
    #[error("reading clean-run output failed after {lines} line(s): {source}")]
    Stream {
        /// How many lines were delivered before the failure.
        lines: u64,
        /// The OS error.
        #[source]
        source: std::io::Error,
    },
    /// Waiting on the child failed.
    #[error("could not reap the clean-run child: {0}")]
    Reap(#[source] std::io::Error),
}

/// How a clean-run child finished.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CleanRunOutcome {
    /// The child's exit code, or `None` when a signal killed it.
    pub code: Option<i32>,
    /// NDJSON lines delivered to the sink. A **counter**, per ADR-017: the
    /// property "we streamed the child's output rather than buffering it and
    /// reading it after exit" is a count, not a duration.
    pub lines: u64,
    /// Bytes of stdout seen, excluding the newlines.
    pub bytes: u64,
    /// Everything the child put on stderr. NDJSON is stdout-only (W09's
    /// acceptance), so this is chatter, and it is captured rather than inherited
    /// so a clean run cannot scribble on the engine's own log.
    pub stderr: String,
    /// True once `wait()` returned — i.e. the child was reaped and is not a
    /// zombie. Always true on the `Ok` path; it is a field rather than an
    /// invariant so a test can assert on the value it actually observed.
    pub reaped: bool,
}

impl CleanRunOutcome {
    /// True when the child exited 0.
    #[must_use]
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }
}

/// May spec §16's "✓ File runs from clean state" be ticked?
///
/// ARCHITECTURE §7.7's closing sentence, as a function:
///
/// > `Isolation::Subprocess` (a real `stratum run --clean` child) is the **only**
/// > thing that may set the §16 tick. We never infer it from static analysis.
///
/// The signature is the argument. There is no parameter for a lint result, a
/// `Taint` set or an `EffectSet`, so a caller that has only analysed the file
/// has nothing to pass and cannot reach a `true`; and an
/// [`Isolation::InProcess`] run cannot earn the tick even when it succeeded,
/// because an in-process "clean" run shares the OS process — its cwd, its
/// environment, its open handles — with the interactive session, and those are
/// three of the sixteen items.
#[must_use]
pub fn clean_state_tick(isolation: Isolation, outcome: &CleanRunOutcome) -> bool {
    matches!(isolation, Isolation::Subprocess) && outcome.success() && outcome.reaped
}

/// The `stratum run --clean --sandbox` invocation, as data.
///
/// Kept separate from running it so that [`CleanRun::command_line`] can be
/// asserted without spawning anything — the flags are a contract with W09's CLI,
/// and a test that only checks the child's *output* would not notice `--clean`
/// going missing.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CleanRun {
    /// The `stratum` binary. Supplied rather than discovered: an engine that
    /// searched `PATH` for its own name would be one `PATH` away from verifying
    /// reproducibility with somebody else's build.
    pub program: Utf8PathBuf,
    /// The entry `.do` file.
    pub entry: Utf8PathBuf,
    /// The scratch root. `None` runs unsandboxed, which only the CLI's own
    /// `stratum run` does — the Verify action always passes one.
    pub sandbox: Option<Utf8PathBuf>,
    /// Add `--deterministic` (CONTRACTS §7.2), which is what makes two clean
    /// runs byte-comparable.
    pub deterministic: bool,
}

impl CleanRun {
    /// A verification run of `entry` into `scratch`.
    #[must_use]
    pub fn verify(
        program: impl Into<Utf8PathBuf>,
        entry: impl Into<Utf8PathBuf>,
        scratch: impl Into<Utf8PathBuf>,
    ) -> Self {
        Self {
            program: program.into(),
            entry: entry.into(),
            sandbox: Some(scratch.into()),
            deterministic: true,
        }
    }

    /// The argument vector, in the order ARCHITECTURE §3's diagram writes it.
    ///
    /// **This is a contract with W09's CLI, and as of this writing the CLI does
    /// not honour half of it.** `crates/stratum-cli/src/cli.rs`'s `ExecCommon`
    /// accepts `--json` and `--deterministic` but has neither `--clean` (it
    /// documents `run` as *always* clean-state, so the flag is redundant but
    /// must still parse) nor `--sandbox DIR` (which has no equivalent at all —
    /// `SessionConfigWire::write_sandbox` is the field it would fill). A child
    /// spawned with this argv against today's CLI exits on a clap parse error
    /// rather than running.
    ///
    /// The argv here follows the plan and ARCHITECTURE §3 rather than the
    /// current CLI on purpose: a verification run without `--sandbox` would
    /// overwrite the user's own outputs, which is the one thing R01 exists to
    /// prevent. Escalated in W08b's return; W09 owns `cli.rs`.
    #[must_use]
    pub fn args(&self) -> Vec<String> {
        let mut v = vec![
            "run".to_owned(),
            self.entry.to_string(),
            "--json".to_owned(),
            "--clean".to_owned(),
        ];
        if let Some(root) = &self.sandbox {
            v.push("--sandbox".to_owned());
            v.push(root.to_string());
        }
        if self.deterministic {
            v.push("--deterministic".to_owned());
        }
        v
    }

    /// Program plus arguments, for a diagnostic or an assertion.
    #[must_use]
    pub fn command_line(&self) -> Vec<String> {
        let mut v = vec![self.program.to_string()];
        v.extend(self.args());
        v
    }

    /// The working directory the child runs in — checklist item 7, applied to a
    /// process instead of to a `Session`.
    #[must_use]
    pub fn child_cwd(&self) -> Option<&Utf8Path> {
        self.entry.parent().filter(|p| !p.as_str().is_empty())
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new(self.program.as_std_path());
        cmd.args(self.args());
        if let Some(dir) = self.child_cwd() {
            cmd.current_dir(dir.as_std_path());
        }
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd
    }

    /// Spawn the child, stream its NDJSON to `on_line`, and reap it.
    ///
    /// `on_line` is called **as each line arrives**, not after the child exits:
    /// a 40-second clean run whose progress only appeared at the end would make
    /// the Verify button look hung, and buffering the whole stream is O(output)
    /// memory for no benefit. The sink gets the line without its terminator.
    ///
    /// The child is reaped on every path. On a read error the child is killed
    /// first and then waited on, because returning without waiting is how a
    /// verification run becomes a zombie that outlives the session that asked
    /// for it.
    ///
    /// # Errors
    ///
    /// [`IsolateError`] — spawn, stream or reap.
    pub fn stream<F>(&self, mut on_line: F) -> Result<CleanRunOutcome, IsolateError>
    where
        F: FnMut(&str),
    {
        let mut child = self
            .command()
            .spawn()
            .map_err(|source| IsolateError::Spawn {
                program: self.program.to_string(),
                source,
            })?;

        // stderr is drained on its own thread, not after stdout closes. A pipe
        // holds ~64 KiB; a child that filled its stderr while we were still
        // reading stdout would block on the write and never reach EOF on
        // stdout, and the two of us would wait for each other forever. This is
        // the classic two-pipe deadlock and it only shows up under load, which
        // is the worst time to find it.
        let drain = child.stderr.take().map(|mut err| {
            std::thread::spawn(move || {
                let mut s = String::new();
                let _ = err.read_to_string(&mut s);
                s
            })
        });

        let stdout = child
            .stdout
            .take()
            .expect("stdout is piped by CleanRun::command");
        let mut reader = BufReader::new(stdout);

        let mut lines = 0u64;
        let mut bytes = 0u64;
        let mut buf = String::new();
        loop {
            buf.clear();
            match reader.read_line(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    bytes += n as u64;
                    let line = buf.trim_end_matches(['\n', '\r']);
                    // A trailing newline on the last line produces one empty
                    // read; NDJSON is one object per line, so an empty line is
                    // not a record and must not be counted as one.
                    if !line.is_empty() {
                        lines += 1;
                        on_line(line);
                    }
                }
                Err(source) => {
                    // Kill first, then reap: `wait` on a child that is still
                    // producing output would block until it finished writing to
                    // a pipe nobody is reading.
                    let _ = child.kill();
                    let _ = child.wait();
                    drop(drain.map(std::thread::JoinHandle::join));
                    return Err(IsolateError::Stream { lines, source });
                }
            }
        }

        let status = child.wait().map_err(IsolateError::Reap)?;
        let stderr = drain
            .map(|h| h.join().unwrap_or_default())
            .unwrap_or_default();
        Ok(CleanRunOutcome {
            code: status.code(),
            lines,
            bytes,
            stderr,
            reaped: true,
        })
    }
}
