//! Stratum's shared numeric primitives.
//!
//! This crate is the answer to three separate designs having written their own
//! `is_missing` and their own `%9.0g` (ARCHITECTURE C2, C12). It owns:
//!
//! * [`missing`] — THE bit-level definition of Stata's 27 missing values, the
//!   widen/narrow pair, and `canon`, which every arithmetic kernel calls.
//! * [`fmt`] — `%g`/`%f`/`%e`/`%s`/`%x`/`%t*` and the `StataFormat` grammar.
//! * [`math`] — the `libm` re-export, the ONLY transcendentals in the engine.
//! * [`reduce`] — the only parallel primitive, with a deterministic fold.
//! * [`gram`], [`sweep`] — `X'X` and the Stata-compatible collinearity rule.
//! * [`dist`] — the normal, t, chi-squared and F tails.
//! * [`types`], [`value`] — the promotion ladder and the two expression types.
//!
//! It has no dataset, no parser, no I/O and no platform layer, which is what
//! makes it testable in a plain synchronous loop and buildable for
//! `wasm32-unknown-unknown` (ARCHITECTURE §8.4).
//!
//! # What this crate does NOT declare
//!
//! `StorageType` and `Span` are declared in `stratum-proto` and re-exported
//! here (A10). A structurally identical twin with no conversion between the two
//! is a bug the compiler cannot see.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod dist;
pub mod fmt;
pub mod gram;
pub mod math;
pub mod missing;
pub mod reduce;
pub mod sweep;
pub mod types;
pub mod value;

pub use fmt::{fmt_f, fmt_fc, fmt_g, fmt_g5, FormatKind, StataFormat};
pub use missing::{canon, is_missing, missing_f64, tag_of, SYSMISS};
pub use reduce::{map_reduce_blocks, CHUNK_ROWS};
pub use types::StorageType;
pub use value::Value;

/// Re-exported so that a consumer of this crate never needs a direct dependency
/// on `stratum-proto` just to name a span (A10).
pub use stratum_proto::Span;
