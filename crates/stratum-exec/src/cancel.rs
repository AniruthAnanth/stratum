//! Cancellation — design 03 §9.2, CONTRACTS §6's ladder.
//!
//! A relaxed atomic load is one instruction and does not fence, so polling the
//! flag at every safepoint is free relative to any real work. That is the whole
//! reason the ladder can be built on cooperative checks rather than on signals:
//! `pthread_kill` into a thread holding an `Arc` clone leaves the dataset in an
//! undefined state, and an undefined dataset is worse than a slow cancel.
//!
//! Cancellation propagates as `Err(Interrupt)` through ordinary `Result`
//! returns, never as a panic unwind, because the runtime needs a defined state
//! to roll back to (INV-2).

use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;

use stratum_proto::CancelLevel;

/// The UI expects the engine to acknowledge an `Interrupt` within this budget.
pub const ACK_BUDGET_MS: u64 = 50;
/// If no `BlockFinished{Interrupted}` has arrived by now, the button becomes
/// *Force stop* and the next press sends `Abort`.
pub const ABORT_AFTER_MS: u64 = 2_000;
/// If the worker is still alive at this point the supervisor kills the engine
/// process, respawns it, and offers "Replay to Execution N" from the ledger.
pub const KILL_AFTER_MS: u64 = 4_000;

/// What a safepoint check returns when the run is being cancelled.
#[derive(Clone, Copy, PartialEq, Eq, Debug, thiserror::Error)]
pub enum Interrupt {
    /// `Esc` / `Ctrl+Break`. Roll back and report `Interrupted`.
    #[error("interrupted")]
    Break,
    /// *Force stop*. Same contract; the difference is only how long the
    /// supervisor will wait before it stops asking.
    #[error("aborted")]
    Kill,
}

/// "No level has been requested yet" for [`CancelState::requested_at_ms`].
///
/// `u64::MAX`, not zero: zero is a legal [`stratum_proto::UnixMs`], and a
/// sentinel that collides with a legal value made the ladder unreachable for any
/// clock whose origin is the run rather than 1970 — `escalation` measured
/// `None`, so *Force stop* never appeared and the supervisor never reached
/// `KillProcess`. A far-future sentinel cannot be reached by a real clock, and
/// if it somehow were, the failure is one extra `Wait`, which is the rung it is
/// always safe to be on.
const NEVER_REQUESTED: u64 = u64::MAX;

#[derive(Debug)]
struct CancelState {
    /// 0 = running, 1 = Break, 2 = Kill. Ordered so `>=` comparisons work and
    /// so a level can only ever escalate.
    flag: AtomicU8,
    /// When the first request arrived, in ms since the epoch, or
    /// [`NEVER_REQUESTED`].
    requested_at_ms: AtomicU64,
}

/// A cancellation flag shared between the requester and the running command.
#[derive(Clone, Debug)]
pub struct CancelToken(Arc<CancelState>);

impl Default for CancelToken {
    fn default() -> Self {
        Self::new()
    }
}

impl CancelToken {
    /// A token in the running state.
    #[must_use]
    pub fn new() -> Self {
        Self(Arc::new(CancelState {
            flag: AtomicU8::new(0),
            requested_at_ms: AtomicU64::new(NEVER_REQUESTED),
        }))
    }

    /// The safepoint check. One relaxed load; `#[inline]` because module 05
    /// calls it every 65 536 rows and between every estimator iteration.
    ///
    /// # Errors
    /// [`Interrupt::Break`] or [`Interrupt::Kill`] once a level has been
    /// requested.
    #[inline]
    pub fn check(&self) -> Result<(), Interrupt> {
        match self.0.flag.load(Ordering::Relaxed) {
            0 => Ok(()),
            1 => Err(Interrupt::Break),
            _ => Err(Interrupt::Kill),
        }
    }

    /// Request a level. Monotone: a `Kill` is never downgraded to a `Break` by
    /// a late-arriving request, and the timestamp records the FIRST request so
    /// the escalation ladder measures from the user's first press.
    pub fn request(&self, level: CancelLevel, now_ms: u64) {
        let bits = level as u8;
        let _ = self
            .0
            .flag
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |cur| {
                (bits > cur).then_some(bits)
            });
        let _ = self.0.requested_at_ms.compare_exchange(
            NEVER_REQUESTED,
            now_ms,
            Ordering::Relaxed,
            Ordering::Relaxed,
        );
    }

    /// The level currently requested, if any.
    #[must_use]
    pub fn level(&self) -> Option<CancelLevel> {
        match self.0.flag.load(Ordering::Relaxed) {
            0 => None,
            1 => Some(CancelLevel::Interrupt),
            _ => Some(CancelLevel::Abort),
        }
    }

    /// True once any level has been requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.flag.load(Ordering::Relaxed) != 0
    }

    /// Milliseconds since the first request, or `None` if none was made.
    #[must_use]
    pub fn elapsed_ms(&self, now_ms: u64) -> Option<u64> {
        match self.0.requested_at_ms.load(Ordering::Relaxed) {
            NEVER_REQUESTED => None,
            at => Some(now_ms.saturating_sub(at)),
        }
    }

    /// What the supervisor should do next, given how long the worker has been
    /// ignoring the request.
    #[must_use]
    pub fn escalation(&self, now_ms: u64) -> Escalation {
        let Some(elapsed) = self.elapsed_ms(now_ms) else {
            return Escalation::Wait;
        };
        if elapsed >= KILL_AFTER_MS {
            Escalation::KillProcess
        } else if elapsed >= ABORT_AFTER_MS {
            Escalation::OfferForceStop
        } else {
            Escalation::Wait
        }
    }
}

/// The rung of the cancel ladder a run is on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Escalation {
    /// Inside the budget; keep waiting for the safepoint.
    Wait,
    /// Past 2 s: the UI shows *Force stop*, which sends `Abort`.
    OfferForceStop,
    /// Past 4 s: the supervisor kills and respawns the engine process. The
    /// thread is never `pthread_kill`ed — the session is abandoned, marked
    /// `Poisoned`, and every block goes `Stale(EpochReset)` on restart.
    KillProcess,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_level_only_ever_escalates() {
        let t = CancelToken::new();
        assert_eq!(t.check(), Ok(()));
        t.request(CancelLevel::Abort, 1_000);
        assert_eq!(t.check(), Err(Interrupt::Kill));
        // A late Interrupt must not downgrade an Abort.
        t.request(CancelLevel::Interrupt, 1_010);
        assert_eq!(t.check(), Err(Interrupt::Kill));
        // …and the ladder measures from the FIRST press.
        assert_eq!(t.elapsed_ms(1_050), Some(50));
    }

    #[test]
    fn the_ladder_matches_the_contract() {
        let t = CancelToken::new();
        assert_eq!(t.escalation(9_999), Escalation::Wait);
        t.request(CancelLevel::Interrupt, 0);
        assert_eq!(t.escalation(ACK_BUDGET_MS), Escalation::Wait);
        assert_eq!(t.escalation(ABORT_AFTER_MS), Escalation::OfferForceStop);
        assert_eq!(t.escalation(KILL_AFTER_MS), Escalation::KillProcess);
    }
}
