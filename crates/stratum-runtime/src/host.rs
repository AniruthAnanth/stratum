//! [`FsHost`] — the shipping [`RuntimeHost`]: the real filesystem, the real
//! clock, and `stratum-dta` behind the recorded doors.
//!
//! This is the ONE place the interpreter's world is allowed to be real.
//! Nothing here is reachable from a command body except through `ExecCtx`'s
//! wrappers, which record every call into the access log (design 03 §6.3) —
//! that is what keeps a direct `std::fs` call in this file from being the
//! unrecorded read the module system everywhere else forbids.
//!
//! # The sysuse tree
//!
//! `sysuse <name>` resolves to a REAL file, `<ado base>/<first-letter>/
//! <name>.dta`, and the caller prints the real path it loaded — never a faked
//! one (`packaging/README.md`, "The shipped ado tree"). The base directory is
//! found once, at construction, in this order:
//!
//! 1. **`STRATUM_ADO_BASE`** — an explicit environment override; always wins.
//!    The packaged desktop host sets it on the engine child it spawns.
//! 2. **Executable-adjacent** — an `ado/base` tree next to this process's own
//!    executable (the dev-tree layout `cargo xtask dist stage` produces).
//!
//! When neither exists, `sysuse` answers `r(601)` with the dataset's name —
//! honestly absent, exactly as [`NoHost`](crate::ctx::NoHost) answers.

use std::time::{SystemTime, UNIX_EPOCH};

use camino::{Utf8Path, Utf8PathBuf};
use stratum_dta::{DtaError, Release};
use stratum_parse::StataError;
use stratum_proto::UnixMs;

use crate::ctx::{LoadedData, RuntimeHost};

/// The filesystem-backed [`RuntimeHost`] the shipping binary installs.
#[derive(Clone, Debug, Default)]
pub struct FsHost {
    /// The resolved sysuse base, or `None` when this build ships no tree.
    ado_base: Option<Utf8PathBuf>,
}

impl FsHost {
    /// A host resolving the sysuse tree from the environment (see the module
    /// header for the order).
    #[must_use]
    pub fn new() -> Self {
        Self {
            ado_base: resolve_ado_base(),
        }
    }

    /// A host with an explicit sysuse base — tests, and callers that already
    /// resolved one.
    #[must_use]
    pub fn with_ado_base(base: impl Into<Utf8PathBuf>) -> Self {
        Self {
            ado_base: Some(base.into()),
        }
    }

    /// The sysuse base this host resolved, for diagnostics (`doctor`).
    #[must_use]
    pub fn ado_base(&self) -> Option<&Utf8Path> {
        self.ado_base.as_deref()
    }
}

/// `STRATUM_ADO_BASE` first, executable-adjacent `ado/base` second.
fn resolve_ado_base() -> Option<Utf8PathBuf> {
    if let Ok(base) = std::env::var("STRATUM_ADO_BASE") {
        if !base.is_empty() {
            return Some(Utf8PathBuf::from(base));
        }
    }
    let exe = std::env::current_exe().ok()?;
    let adjacent = exe.parent()?.join("ado").join("base");
    if adjacent.is_dir() {
        return Utf8PathBuf::from_path_buf(adjacent).ok();
    }
    None
}

fn not_found(path: &Utf8Path) -> StataError {
    StataError::new(601, format!("file {path} not found")).token(path.to_string())
}

fn cannot_open(path: &Utf8Path) -> StataError {
    StataError::new(603, format!("file {path} could not be opened")).token(path.to_string())
}

/// A read-side [`DtaError`] as the Stata return code `use` reports.
///
/// The io cases keep Stata's own spellings (`errors.log` pins the 601 one);
/// everything else is `r(610)` carrying the reader's precise diagnostic —
/// "not Stata format" alone is not something a user can act on when the file
/// IS a `.dta` whose one variable name the frame refuses.
fn read_error(path: &Utf8Path, e: &DtaError) -> StataError {
    match e.rc() {
        601 => not_found(path),
        603 => cannot_open(path),
        _ => StataError::new(610, format!("file {path} not Stata format: {e}"))
            .token(path.to_string()),
    }
}

impl RuntimeHost for FsHost {
    fn load_dataset(&mut self, path: &Utf8Path) -> Result<LoadedData, StataError> {
        let ds = stratum_dta::read_dta(path).map_err(|e| read_error(path, &e))?;
        let bridge = ds.into_frame("default").map_err(|e| read_error(path, &e))?;
        Ok(LoadedData {
            frame: bridge.frame,
            timestamp: bridge.timestamp,
        })
    }

    fn save_dataset(
        &mut self,
        path: &Utf8Path,
        frame: &stratum_data::Frame,
    ) -> Result<(), StataError> {
        // R118 is Stata 18's own default; `write_dta` re-derives the release
        // from the dataset's shape, so this choice only keys the strL packing
        // `from_frame` prepares.
        let ds = stratum_dta::Dataset::from_frame(frame, Release::R118)
            .map_err(|e| StataError::new(603, format!("cannot save {path}: {e}")))?;
        stratum_dta::write_dta(path, &ds).map_err(|_| cannot_open(path))
    }

    fn sysuse_path(&mut self, name: &str) -> Result<Utf8PathBuf, StataError> {
        let missing =
            || StataError::new(601, format!("file {name}.dta not found")).token(name.to_owned());
        let base = self.ado_base.as_ref().ok_or_else(missing)?;
        let first = name
            .chars()
            .next()
            .filter(char::is_ascii)
            .ok_or_else(missing)?;
        let path = base
            .join(first.to_ascii_lowercase().to_string())
            .join(format!("{name}.dta"));
        if !path.is_file() {
            return Err(missing());
        }
        Ok(path)
    }

    fn read_text(&mut self, path: &Utf8Path) -> Result<String, StataError> {
        std::fs::read_to_string(path).map_err(|_| not_found(path))
    }

    fn erase(&mut self, path: &Utf8Path) -> Result<(), StataError> {
        std::fs::remove_file(path).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => not_found(path),
            _ => cannot_open(path),
        })
    }

    fn exists(&mut self, path: &Utf8Path) -> bool {
        path.as_std_path().exists()
    }

    fn now_ms(&mut self) -> UnixMs {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
    }

    fn env(&mut self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }
}
