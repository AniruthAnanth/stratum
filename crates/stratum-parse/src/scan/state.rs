//! `ScanState` — the boundary-state fingerprint (A25, deferred by design 02
//! §5.5 and required by the incremental gate).
//!
//! Design 02 §5.5's rule is "find the last region before the edit and rescan
//! from there to end of file". That is O(bytes after the edit), which is fine
//! when the edit is at the end of the file and useless when it is at line 100 of
//! a 2 MB file: every keystroke rescans ~2 MB and re-hashes every following
//! region, in wasm, inside a 6 ms budget. The fix is to stop the rescan as soon
//! as it PROVABLY re-converges with the old segmentation, and this type is the
//! proof obligation.
//!
//! The claim it encodes is narrow and checkable: every region boundary is a
//! clean point — brace depth 0, not inside a string, not inside a block comment,
//! not inside an `end`-terminated block — so the ONLY scanner state that varies
//! from one boundary to the next is the delimiter mode. Two consequences:
//!
//! 1. If the rescan reaches new-source offset `x` in state `S`, and the previous
//!    segmentation had a region starting at old-source offset `x - delta` in the
//!    same state `S`, then the logical lines from there on are byte-identical
//!    and scan identically.
//! 2. The grouping algorithm of 02 §5.2 only ever looks FORWARD from the line it
//!    is at (trivia run, `end` search, brace close, else-chain lookahead), so
//!    identical lines from `x` on group identically too.
//!
//! Together those make the whole tail reusable — spans shifted, hashes kept —
//! which is what turns the ≤ 8-regions-re-hashed acceptance bullet from a wish
//! into an invariant. If a future change adds cross-boundary scanner state (a
//! `version` pragma that changes tokenization, say), it goes in this struct and
//! convergence keeps working; that is why it is a struct and not a bare
//! `Delimiter`.

use stratum_proto::Delimiter;

/// The complete scanner state at a region boundary.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct ScanState {
    /// `#delimit` mode in force.
    pub delimiter: Delimiter,
}

impl ScanState {
    /// The state a do-file starts in. `#delimit` is file-scoped and resets to
    /// `cr` whenever a do-file begins ([P] #delimit).
    pub const START: ScanState = ScanState {
        delimiter: Delimiter::Cr,
    };

    /// Construct from the delimiter mode.
    #[inline]
    pub fn new(delimiter: Delimiter) -> Self {
        Self { delimiter }
    }

    /// A stable one-word fingerprint, for cheap comparison and for logging a
    /// convergence failure without printing a struct.
    #[inline]
    pub fn fingerprint(self) -> u64 {
        match self.delimiter {
            Delimiter::Cr => 0x01,
            Delimiter::Semi => 0x02,
        }
    }
}

impl Default for ScanState {
    fn default() -> Self {
        Self::START
    }
}
