//! CONTRACTS.md §8 — dataset metadata and paging.
//!
//! The bytes of a [`PageRequest`]'s answer do **not** travel through serde: §8.1
//! specifies the `SDP1` binary layout, fetched over
//! `stratum-asset://localhost/frame/{session}/{frame}/page?…` because scrolling
//! needs `AbortController` cancellation and browser caching, and a Tauri command
//! gives neither (A13). What is declared here is the request that names a page,
//! and the metadata the panes render around it.

use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};

use crate::diagnostic::Confidence;
use crate::ids::{DatasetStateId, ExecutionId, OrderId, VarId, VarIdx};

/// **AMENDED (A10). Declared here and NOWHERE else.** `stratum-core` writes
/// `pub use stratum_proto::StorageType;` and owns only the promotion ladder
/// (`promote(a, b) -> StorageType`) over it. The pre-audit plan gave W01 a
/// second declaration in `stratum-core/src/types.rs`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum StorageType {
    Byte,
    Int,
    Long,
    Float,
    Double,
    Str { width: u16 },
    StrL,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct VariableInfo {
    pub idx: VarIdx,
    pub id: VarId,
    pub name: String,
    pub ty: StorageType,
    pub label: String,
    /// "%8.0gc"
    pub format: String,
    pub value_label: Option<String>,
    pub n_missing: u64,
    /// spec §20 "Created by analysis.do:42" + the statement text.
    pub provenance: Option<Provenance>,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct Provenance {
    pub file: Option<Utf8PathBuf>,
    pub line: u32,
    pub col: u32,
    pub statement: String,
    pub exec: ExecutionId,
    pub confidence: Confidence,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct FrameInfo {
    pub name: String,
    pub n_obs: u64,
    pub n_vars: u32,
    pub sorted_by: Vec<String>,
    pub changed: bool,
    pub state: DatasetStateId,
}

/// Expensive fields stay lazy (spec §20).
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct QuickSummary {
    pub var: String,
    pub state: DatasetStateId,
    pub n: u64,
    pub n_missing: u64,
    pub mean: Option<f64>,
    pub median: Option<f64>,
    pub sd: Option<f64>,
    pub min: Option<f64>,
    pub max: Option<f64>,
    /// label → pre-formatted value
    pub display: Vec<(String, String)>,
    /// 24 bins
    pub sparkline: Option<Vec<u32>>,
    /// True when the dataset was too large and we returned metadata only.
    pub deferred: bool,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum DataEvent {
    FrameChanged {
        frame: String,
        state: DatasetStateId,
    },
    VarAdded {
        frame: String,
        var: VariableInfo,
    },
    VarDropped {
        frame: String,
        name: String,
    },
    VarModified {
        frame: String,
        name: String,
        idx: VarIdx,
    },
    VarRenamed {
        frame: String,
        from: String,
        to: String,
    },
    TypeChanged {
        frame: String,
        name: String,
        from: StorageType,
        to: StorageType,
    },
    ObsCountChanged {
        frame: String,
        n_obs: u64,
    },
    SortChanged {
        frame: String,
        keys: Vec<String>,
    },
    FrameCreated {
        frame: String,
    },
    FrameDropped {
        frame: String,
    },
    CurrentFrame {
        frame: String,
    },
}

// ---------------------------------------------------------------------------
// §8.1 `DataPage` — the binary transport
// ---------------------------------------------------------------------------

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct PageRequest {
    pub frame: String,
    /// Which snapshot the UI believes it is showing. If the frame has advanced,
    /// the response's `state` differs and the UI invalidates. This is exactly
    /// spec §13's "Dataset state: D17", reused rather than reinvented.
    pub state: DatasetStateId,
    pub row0: u64,
    pub nrows: u32,
    pub cols: Vec<VarIdx>,
    /// **AMENDED (A13).** A HANDLE to an engine-side view order, established by
    /// `DataOrderSet`. `None` ⇒ dataset order.
    ///
    /// This was `Option<Vec<u64>>` — "a permutation of observation indices" —
    /// which is unfillable by its only sender and catastrophic if it were: the
    /// Data Editor's request arguments are serialised by the webview, so a
    /// sorted 10 M-row view meant **80 MB of JSON per 40-row fetch** against a
    /// 12 ms budget, while `06` §15.3 simultaneously required that sorting happen
    /// in Rust and never in the frontend. The frontend now declares intent
    /// (`OrderSpec`) once and scrolls against a `u32`.
    pub order: Option<OrderId>,
    pub render: RenderMode,
    /// Stale responses are dropped by the client.
    pub seq: u32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum RenderMode {
    /// Cells already formatted per each variable's `StataFormat`, value labels
    /// applied. Formatting happens in the CORE, so `list`, the Data Editor and
    /// the inline cards cannot disagree.
    Display,
    /// Raw values for editing: f64 + per-cell missing tag; strings as bytes.
    Edit,
}
