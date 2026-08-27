//! Frame versioning: what changed, and coarsely enough to be free.
//!
//! Two counters, because the UI asks two different questions and answering them
//! with one number costs a full repaint on every keystroke-speed `replace`:
//!
//! * [`DataVersion`] — *values* moved. Bumped by any write through the barrier.
//!   This is what invalidates a cached page and what
//!   `DatasetStateId` ("Dataset state: D17", spec §13) carries on the wire.
//! * [`FrameEpoch`] — the *shape* moved: a variable added, dropped, renamed,
//!   recast, or the observation count changed. A Data Editor that has laid out
//!   columns only needs to relayout when the epoch moves.
//!
//! Both are monotonic per frame and never reused, so "is my page stale" is an
//! integer comparison rather than a diff.

use serde::{Deserialize, Serialize};
use stratum_proto::DatasetStateId;

/// Monotonic per-frame value version.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
#[serde(transparent)]
#[repr(transparent)]
pub struct DataVersion(pub u64);

impl DataVersion {
    /// The version a freshly created frame starts at.
    pub const INITIAL: DataVersion = DataVersion(1);

    /// The next version. Never wraps in any realistic session — one bump per
    /// written command at 1 kHz is 584 million years.
    #[inline]
    #[must_use]
    pub fn next(self) -> DataVersion {
        DataVersion(self.0 + 1)
    }
}

impl From<DataVersion> for DatasetStateId {
    fn from(v: DataVersion) -> Self {
        DatasetStateId(v.0)
    }
}

impl std::fmt::Display for DataVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "D{}", self.0)
    }
}

/// Monotonic per-frame *shape* version.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
#[serde(transparent)]
#[repr(transparent)]
pub struct FrameEpoch(pub u64);

impl FrameEpoch {
    /// The epoch a freshly created frame starts at.
    pub const INITIAL: FrameEpoch = FrameEpoch(1);

    /// The next epoch.
    #[inline]
    #[must_use]
    pub fn next(self) -> FrameEpoch {
        FrameEpoch(self.0 + 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_data_version_is_a_dataset_state_id_on_the_wire() {
        // Spec §13's "Dataset state: D17" is this number, not a second one.
        let v = DataVersion(17);
        assert_eq!(DatasetStateId::from(v), DatasetStateId(17));
        assert_eq!(v.to_string(), "D17");
    }

    #[test]
    fn versions_are_monotonic() {
        assert!(DataVersion::INITIAL.next() > DataVersion::INITIAL);
        assert!(FrameEpoch::INITIAL.next() > FrameEpoch::INITIAL);
    }
}
