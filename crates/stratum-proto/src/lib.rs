//! Stratum's shared wire contract.
//!
//! Everything in `docs/CONTRACTS.md` §§1–9 and §9.1 lives here, and nowhere
//! else. The crate contains **no logic and no I/O**: two total functions
//! ([`BlockId::is_real`] and [`styled::to_plain`]) are the whole executable
//! surface. Consumers that need one of these types write
//! `pub use stratum_proto::Span;` rather than declaring a structurally
//! identical twin — a second declaration with no conversion between them is a
//! silent-at-compile-time class of bug (A10).
//!
//! Module layout is named by CONTRACTS.md itself; each module's header cites the
//! section it transcribes.
//!
//! # Rules that hold across the whole crate
//!
//! * **Time is a `u64`.** [`UnixMs`] is the only time representation on the
//!   wire (A2). `time` is not a dependency, because this crate is reachable
//!   from `stratum-parse`, which must build for `wasm32-unknown-unknown`
//!   (ARCHITECTURE §8.4), and `time` pulls in a locale/tz surface that
//!   invariant forbids. Rendering a `UnixMs` for a human is an L3+ concern.
//! * **Every enum that crosses the wire is explicitly tagged** (CONTRACTS §15).
//!   Nothing is externally tagged by accident and nothing is positional; that
//!   is also why the transport uses `rmp_serde::to_vec_named` rather than a
//!   positional encoding.
//! * **Additive-only within a schema major.** New variants and new fields
//!   carrying `#[serde(default)]` are allowed; renaming a field, reordering, or
//!   changing a field's type is a `STREAM_SCHEMA` bump.
//! * **Numbers destined for a human arrive pre-formatted.** Every payload that
//!   a renderer draws carries display strings produced by `stratum_core::fmt`
//!   alongside the raw `f64` (A6). A renderer that reformats a number will
//!   disagree with the classic text on the first tie-breaking case.

pub mod block;
pub mod capture;
pub mod complete;
pub mod data;
pub mod defuse;
pub mod diagnostic;
pub mod engine;
pub mod exec;
pub mod frame;
pub mod ids;
pub mod introspect;
pub mod repro;
pub mod result;
pub mod session;
pub mod status;
pub mod styled;
pub mod token;

// NOTE FOR W07 (transport/codec). `docs/ownership.toml` carves
// `crates/stratum-proto/src/frame.rs` out of W00's ownership and gives it to
// W07 — the only declared exception in the whole partition. Adding
//
//     pub mod frame;
//
// to this list is therefore W07's edit to a W00 file, and the only one.
// `BulkRef` (CONTRACTS §10) already lives in [`engine`], because
// `EngineResponse::Bulk` cannot compile without it; `frame.rs` should
// `pub use crate::engine::BulkRef;` rather than declare a second one.

pub use block::*;
pub use capture::*;
pub use complete::*;
pub use data::*;
pub use defuse::*;
pub use diagnostic::*;
pub use engine::*;
pub use exec::*;
pub use ids::*;
pub use introspect::*;
pub use repro::*;
pub use result::*;
pub use session::*;
pub use status::*;
pub use token::*;

/// Milliseconds since 1970-01-01T00:00:00Z. The ONLY time representation on the
/// wire (A2). Never a formatted string, never a float, never `OffsetDateTime`.
pub type UnixMs = u64;
