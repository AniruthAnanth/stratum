//! CONTRACTS.md §1 — identity, §1.1 — hashes and spans.
//!
//! Every id is a `#[serde(transparent)]` newtype over an integer, so the wire
//! carries a bare number and the `Display` prefix (`B41`, `R41`, `D17`) exists
//! only for humans and for the strings the UI shows in spec §13's status line.

use serde::{Deserialize, Serialize};

// The `$(#[$meta:meta])*` head is the one liberty taken with CONTRACTS.md §1's
// macro: without it the doc comment the contract attaches to `id!(OrderId, ...)`
// is dropped on the floor with an `unused_doc_comment` warning, because rustdoc
// does not document macro invocations.
macro_rules! id {
    ($(#[$meta:meta])* $name:ident, $ty:ty, $prefix:literal) => {
        $(#[$meta])*
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug,
                 Serialize, Deserialize)]
        #[cfg_attr(feature = "specta", derive(specta::Type))]
        #[serde(transparent)]
        #[repr(transparent)]
        pub struct $name(pub $ty);

        impl $name {
            pub const PREFIX: &'static str = $prefix;
        }
        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}{}", $prefix, self.0)
            }
        }
    };
}

id!(SessionId, u32, "S");
id!(SessionEpoch, u32, "epoch"); // bumps on clear-all / clean run
id!(RunId, u64, "run"); // one RunPlan submission
id!(ExecutionId, u64, "E"); // spec §13 "Execution 41"
id!(StateId, u64, "St"); // whole-session state
id!(DatasetStateId, u64, "D"); // spec §13 "Dataset state: D17"
id!(ResultId, u64, "R"); // spec §13 "Result: R41"
id!(BlockId, u64, "B"); // stable across edits; engine-allocated
id!(DocumentId, u32, "doc");
id!(FrameId, u32, "frame");
id!(VarId, u32, "var"); // column identity; survives rename, dies with drop
id!(VarIdx, u32, "vi"); // POSITION in storage order; NOT identity
id!(SectionId, u32, "sec");

impl BlockId {
    /// Command-bar runs, selection runs, CLI runs. Never a node in the staleness
    /// graph, but its writes DO bump versions and therefore DO make real blocks
    /// stale. See ARCHITECTURE §7.3 claim 2.
    pub const EPHEMERAL: BlockId = BlockId(0);

    /// **AMENDED (A3).** "This region has no block identity at all" — used for
    /// `RegionKind::Trivia` entries in `BlockMap::blocks`. It is NOT `EPHEMERAL`:
    /// conflating them meant that a `StatusChanged { changed: [(BlockId(0), …)] }`
    /// from a command-bar run repainted every comment and blank-line region in
    /// the document with that status, and `latest_by_block[0]` was ambiguous
    /// between "the last command-bar run" and "every trivia region".
    pub const NONE: BlockId = BlockId(u64::MAX);

    #[inline]
    pub fn is_real(self) -> bool {
        self != Self::EPHEMERAL && self != Self::NONE
    }
}

id!(
    /// Handle for an engine-side Data-Editor view order (A13). Allocated by
    /// `data_order_set`, freed by `data_order_drop`, scoped to one session.
    OrderId,
    u32,
    "ord"
);

// ---------------------------------------------------------------------------
// §1.1 Hashes and spans
// ---------------------------------------------------------------------------

/// blake3, first 16 bytes, over the CANONICAL TOKEN STREAM (see §1.2).
/// NOT over source text: comments, reindentation, and `///` reflow must be
/// provably staleness-neutral (spec §23).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct CodeHash(pub [u8; 16]);

/// blake3-128 over the raw UTF-8 bytes INCLUDING comments. UI only — used to
/// detect "the file changed on disk". Never used for staleness.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct TextHash(pub [u8; 16]);

/// blake3-128 over (dtype tag, nobs LE, little-endian value bytes,
/// missing-mask bitset, length-prefixed UTF-8 for string columns).
/// Endianness is normalised so digests compare across platforms (spec §38-E).
///
/// Deliberately not a `specta::Type`: a column digest is an engine-internal
/// comparison key that reaches no TypeScript surface, and CONTRACTS.md §1.1
/// omits the derive for that reason.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ColumnDigest(pub [u8; 16]);

/// Half-open byte range into a UTF-8 buffer. Always on a char boundary.
///
/// **AMENDED (A10). Declared here and NOWHERE else.** `stratum-core` and
/// `stratum-parse` write `pub use stratum_proto::Span;`. The pre-audit plan gave
/// `crates/stratum-parse/src/span.rs` its own copy; two structurally identical
/// types with no conversion is how `VariableInfo.ty` and `Variable.ty` stop
/// unifying, and it is a silent-at-compile-time class of bug.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

/// Half-open, 0-based physical line range.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct LineRange {
    pub start: u32,
    pub end: u32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct TextEdit {
    pub span: Span,
    /// Index into the sender's parallel `texts` table.
    pub text_index: u32,
}

/// The applied form, used at every boundary that actually carries strings.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct Edit {
    pub span: Span,
    pub text: String,
}
