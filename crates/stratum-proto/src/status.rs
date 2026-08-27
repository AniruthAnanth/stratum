//! CONTRACTS.md §3 — block status and staleness.
//!
//! The frontend never invents a status. It computes
//! `displayed = worse_of(local, kernel)`, where `local` is
//! `Stale { CodeChanged }` iff the locally-computed `CodeHash` differs from the
//! one recorded on the last execution and "no opinion" otherwise, over the total
//! order
//!
//! ```text
//! NeverRun < Broken < Failed < Interrupted < Stale < CurrentUnverifiable < Current
//!         (and Queued/Running always win, because they are facts, not judgements)
//! ```
//!
//! The local check may only ever move a block **toward more stale**
//! (ARCHITECTURE C20). The ranking itself is `STATUS_RANK` in
//! `apps/desktop/src/ipc/hand.ts`, which is hand-written precisely because it is
//! a rendering policy rather than a wire type.

use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};

use crate::ids::{BlockId, DatasetStateId, ExecutionId};

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum BlockStatus {
    /// `○`
    NeverRun,
    Queued {
        position: u32,
    },
    /// `▶`
    Running {
        exec: ExecutionId,
        started_ms: u64,
    },
    /// `✓`
    Current {
        exec: ExecutionId,
        dataset: DatasetStateId,
        duration_us: u64,
    },
    /// Ran cleanly, but `Taint::EXTERNAL` means INV-1 is unprovable. `✓⚠`
    CurrentUnverifiable {
        exec: ExecutionId,
        dataset: DatasetStateId,
        duration_us: u64,
        /// See [`Taint`] for why the TypeScript type is the raw `u16` rather
        /// than a generated named type.
        #[cfg_attr(feature = "specta", specta(type = u16))]
        taint: Taint,
    },
    /// `◌`
    Stale {
        reason: StaleReason,
        since: Option<ExecutionId>,
    },
    /// `✕`
    Failed {
        exec: ExecutionId,
        rc: u32,
    },
    Interrupted {
        exec: ExecutionId,
        rolled_back: bool,
    },
    /// Code references a name that no longer resolves. Distinct from Stale:
    /// re-running would ERROR, not merely produce different numbers. `✕!`
    Broken {
        reason: BrokenReason,
    },
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(tag = "why", rename_all = "snake_case")]
pub enum StaleReason {
    CodeChanged,
    EpochReset,
    /// Drives "income was modified at E44".
    InputChanged {
        key: DepKey,
        at: Option<ExecutionId>,
    },
    FileChanged {
        path: Utf8PathBuf,
    },
    /// A block above me is not Current and writes something I read.
    UpstreamPending {
        block: BlockId,
        via: DepKey,
    },
    /// A block above me is not Current and I cannot rule it out.
    UpstreamOpaque {
        block: BlockId,
    },
    RngShifted,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(tag = "why", rename_all = "snake_case")]
pub enum BrokenReason {
    UnresolvedName {
        name: String,
        suggestion: Option<String>,
    },
    UnknownCommand {
        name: String,
        suggestion: Option<String>,
    },
    MissingFile {
        path: Utf8PathBuf,
    },
}

/// Human-readable identification of one dependency slot. Rendered verbatim in
/// stale banners, so keep it short.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(tag = "ns", rename_all = "snake_case")]
pub enum DepKey {
    Var { frame: String, name: String },
    RowMembership { frame: String },
    RowOrder { frame: String },
    VarLayout { frame: String },
    Macro { name: String },
    Scalar { name: String },
    Matrix { name: String },
    Program { name: String },
    Estimates,
    RClass,
    SClass,
    Rng,
    Setting { name: String },
    Cwd,
    File { path: Utf8PathBuf },
}

bitflags::bitflags! {
    /// Why a block's INV-1 proof is weaker than "we know exactly what it read".
    ///
    /// A block that ran cleanly but carries `EXTERNAL` is `CurrentUnverifiable`,
    /// not `Current`: we cannot prove nothing outside the engine changed under
    /// it, and claiming otherwise would be the one lie the staleness model
    /// cannot afford.
    ///
    /// **CONTRACT DEVIATION, reported by W00.** CONTRACTS.md §3 puts
    /// `#[cfg_attr(feature = "specta", derive(specta::Type))]` on this block. It
    /// cannot compile: `bitflags!` expands to
    /// `pub struct Taint(<u16 as PublicFlags>::Internal)`, and specta has no
    /// `Type` impl for that private internal type. The two fields that carry a
    /// `Taint` across the wire instead carry `#[specta(type = u16)]`, which is
    /// also the honest TypeScript rendering of a bit set — the flag names are
    /// not on the wire in the non-human-readable encoding either.
    #[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
    pub struct Taint: u16 {
        const MACRO_VARLIST    = 1 << 0;
        const UNKNOWN_COMMAND  = 1 << 1;
        /// `capture noisily `cmd''
        const DYNAMIC_DISPATCH = 1 << 2;
        /// shell/python/java/plugin — unverifiable
        const EXTERNAL         = 1 << 3;
        const CLOCK            = 1 << 4;
        const ENVIRONMENT      = 1 << 5;
        const UNBOUNDED_LOOP   = 1 << 6;
        const FILE_DYNAMIC     = 1 << 7;
    }
}

/// **CONTRACT DEVIATION, reported by W00.** CONTRACTS.md §3 lists `Serialize,
/// Deserialize` among this type's derives. `bitflags` 2 answers those derives
/// with a *format-dependent* encoding — a `"A | B"` string when the format calls
/// itself human-readable, the raw bits otherwise — and `rmp-serde` 1 answers
/// `is_human_readable()` with `false` on its serializer and `true` on its
/// deserializer. The derived pair therefore writes `73` and then refuses to read
/// it: "invalid type: integer `73`, expected a string value of `|` separated
/// flags". Every `BlockStatus::CurrentUnverifiable` and every `ExecutionRecord`
/// on the desktop transport would have failed to decode.
///
/// Fixing it at the type rather than at the codec also settles the encoding the
/// way §15 needs it settled: the bits are the wire form in BOTH encodings, so a
/// flag rename is not a wire break, and `from_bits_retain` keeps a bit set by a
/// newer engine that this build has no name for — dropping it would silently
/// downgrade a `CurrentUnverifiable` block to `Current`, which is the one
/// direction the staleness model must never move on its own.
impl Serialize for Taint {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.bits().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Taint {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_bits_retain(u16::deserialize(deserializer)?))
    }
}
