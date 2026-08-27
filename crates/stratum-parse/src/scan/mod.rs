//! The scanner: physical bytes → logical lines → logical executable regions.
//!
//! * [`logical`] is design 02 §§2–3: comments, strings, `#delimit`, and where a
//!   logical line ends.
//! * [`region`] is design 02 §5: the shared definition of a LOGICAL EXECUTABLE
//!   REGION that the editor gutter, the run commands and the headless CLI all
//!   code against.
//! * [`marker`] is spec §3: `%%` cell markers and the sections they open.
//! * [`state`] is the boundary fingerprint that makes incremental
//!   re-segmentation converge instead of rescanning to end of file.
//!
//! Nothing here expands a macro or parses an expression. Segmentation consults a
//! command table in exactly one place — the `end`-block opener test of §5.3 —
//! and never looks past the first word of a line.

pub mod logical;
pub mod marker;
pub mod region;
pub mod state;

pub use logical::{Derived, DerivedText, LogicalLine};
pub use marker::marker_title;
pub use region::{
    resegment, resegment_with_stats, segment, segment_with, HeadInfo, IdxRange, PrefixChain,
    Region, RegionShape, ResegmentStats, SegmentOptions, Segmentation, SourceEdit,
};
pub use state::ScanState;
