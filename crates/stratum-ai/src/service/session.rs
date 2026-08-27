//! `07` §2.7 and §4.5 — cancellation, concurrency, and the session's counters.
//!
//! # There is never more than one in-flight request per surface
//!
//! That single rule is what makes ghost completion and quick-fix feel instant
//! rather than laggy. Issuing a request for a surface cancels the previous one
//! for that surface first ([`SurfaceCancellation::issue`]), so the answer the
//! user eventually sees is always the answer to the question they are currently
//! asking. A result explanation for a superseded run is worse than no
//! explanation.
//!
//! # The third permit is reserved, not shared
//!
//! `07` §2.7 gives the stack two permits and then says interactive surfaces hold
//! "a reserved third permit that batch work cannot take". Two semaphores rather
//! than one of size three, because a single semaphore cannot express "you may
//! have this permit and you may not": a file-scope auto-comment holding all
//! three would starve exactly the quick-fix the user is staring at.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

use super::surface::Surface;
use crate::context::tiers::PrivacyTier;
use crate::provider::types::TokenUsage;
use crate::tasks::cost::{CostSummary, ModelPrice};

/// `07` §2.7: two permits for everything.
pub const BATCH_PERMITS: usize = 2;

/// Plus one that only an interactive surface may take.
pub const INTERACTIVE_PERMITS: usize = 1;

/// One live cancellation token per surface.
#[derive(Debug, Default)]
pub struct SurfaceCancellation {
    tokens: Mutex<HashMap<Surface, CancellationToken>>,
    superseded: AtomicU64,
}

impl SurfaceCancellation {
    /// Start a request on `surface`, cancelling whatever was in flight there.
    #[must_use]
    pub fn issue(&self, surface: Surface) -> CancellationToken {
        let token = CancellationToken::new();
        let mut map = self
            .tokens
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(previous) = map.insert(surface, token.clone()) {
            if !previous.is_cancelled() {
                previous.cancel();
                self.superseded.fetch_add(1, Ordering::Relaxed);
            }
        }
        token
    }

    /// Cancel one surface — the stop button, or `Esc` in the panel.
    pub fn cancel(&self, surface: Surface) {
        let map = self
            .tokens
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(t) = map.get(&surface) {
            t.cancel();
        }
    }

    /// Cancel everything: the document version moved, the dataset state moved,
    /// or the selected result card changed, so every captured precondition is
    /// stale (`07` §2.7, context invalidation).
    pub fn cancel_all(&self) {
        let map = self
            .tokens
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for t in map.values() {
            t.cancel();
        }
    }

    /// How many requests were cancelled by a newer request on the same surface.
    ///
    /// A counter, not a duration (ADR-017): "typing fast produces exactly one
    /// surviving ghost request" is checkable, and "it felt responsive" is not.
    #[must_use]
    pub fn superseded(&self) -> u64 {
        self.superseded.load(Ordering::Relaxed)
    }

    /// Whether a surface currently has a live, uncancelled request.
    #[must_use]
    pub fn is_live(&self, surface: Surface) -> bool {
        let map = self
            .tokens
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        map.get(&surface).is_some_and(|t| !t.is_cancelled())
    }
}

/// The permit pool.
#[derive(Debug)]
pub struct Concurrency {
    batch: Arc<Semaphore>,
    interactive: Arc<Semaphore>,
}

/// A held permit. Dropping it releases the slot.
#[derive(Debug)]
pub struct Slot {
    _permit: OwnedSemaphorePermit,
    reserved: bool,
}

impl Slot {
    /// Whether this slot came from the interactive reserve.
    #[must_use]
    pub const fn reserved(&self) -> bool {
        self.reserved
    }
}

impl Default for Concurrency {
    fn default() -> Self {
        Self::new(BATCH_PERMITS, INTERACTIVE_PERMITS)
    }
}

impl Concurrency {
    /// Build with explicit pool sizes.
    #[must_use]
    pub fn new(batch: usize, interactive: usize) -> Self {
        Self {
            batch: Arc::new(Semaphore::new(batch)),
            interactive: Arc::new(Semaphore::new(interactive)),
        }
    }

    /// Take a slot for `surface`, waiting if necessary.
    ///
    /// An interactive surface takes the reserve first and only then queues for
    /// the general pool; a batch surface can never touch the reserve.
    ///
    /// # Panics
    /// Never: neither semaphore is ever closed, and `acquire_owned` only errors
    /// on a closed semaphore.
    pub async fn acquire(&self, surface: Surface) -> Slot {
        if surface.is_interactive() {
            if let Ok(permit) = self.interactive.clone().try_acquire_owned() {
                return Slot {
                    _permit: permit,
                    reserved: true,
                };
            }
        }
        let permit = self
            .batch
            .clone()
            .acquire_owned()
            .await
            .expect("the AI concurrency semaphore is never closed");
        Slot {
            _permit: permit,
            reserved: false,
        }
    }

    /// Slots free in the general pool.
    #[must_use]
    pub fn batch_available(&self) -> usize {
        self.batch.available_permits()
    }

    /// Slots free in the interactive reserve.
    #[must_use]
    pub fn interactive_available(&self) -> usize {
        self.interactive.available_permits()
    }
}

/// Everything the panel footer and the forced-preview rule need.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct SessionCounters {
    /// Running totals.
    pub cost: CostSummary,
    /// The highest tier any request in this session has actually sent at.
    ///
    /// `07` §4.5: the pre-send preview is *forced* whenever the effective tier
    /// increased since the last request in this session. Tracking the high-water
    /// mark rather than the previous value means a user who goes 1 → 3 → 1 → 3
    /// is asked once, not twice — the second 3 is not new information.
    pub highest_tier_sent: Option<PrivacyTier>,
}

impl SessionCounters {
    /// Whether the pre-send preview must be shown before this request.
    ///
    /// Two triggers, both from `07` §4.5: the tier is `Full`, always; or the
    /// tier is higher than anything this session has sent at.
    #[must_use]
    pub fn preview_forced(&self, tier: PrivacyTier) -> bool {
        if tier == PrivacyTier::Full {
            return true;
        }
        match self.highest_tier_sent {
            None => tier > PrivacyTier::Off,
            Some(high) => tier > high,
        }
    }

    /// Record a completed request.
    pub fn record(&mut self, tier: PrivacyTier, usage: TokenUsage, price: ModelPrice) {
        self.cost.record(usage, price);
        self.highest_tier_sent = Some(self.highest_tier_sent.map_or(tier, |high| high.max(tier)));
    }

    /// Fold the response cache's counters in, for the footer's hit rate.
    pub fn set_cache_counters(&mut self, hits: u64, misses: u64) {
        self.cost.cache_hits = hits;
        self.cost.cache_misses = misses;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_request_on_a_surface_cancels_the_one_it_replaces() {
        let c = SurfaceCancellation::default();
        let first = c.issue(Surface::GhostCompletion);
        let second = c.issue(Surface::GhostCompletion);
        assert!(
            first.is_cancelled(),
            "the superseded request must be cancelled"
        );
        assert!(!second.is_cancelled());
        assert_eq!(c.superseded(), 1);
    }

    #[test]
    fn typing_fast_leaves_exactly_one_live_ghost_request() {
        // The counter. Ten keystrokes, ten requests issued, nine cancelled, one
        // alive — which is the whole of "single-flight" as an assertion.
        let c = SurfaceCancellation::default();
        let mut last = c.issue(Surface::GhostCompletion);
        for _ in 0..9 {
            last = c.issue(Surface::GhostCompletion);
        }
        assert_eq!(c.superseded(), 9);
        assert!(!last.is_cancelled());
        assert!(c.is_live(Surface::GhostCompletion));
    }

    #[test]
    fn surfaces_do_not_cancel_each_other() {
        let c = SurfaceCancellation::default();
        let chat = c.issue(Surface::Chat);
        let _ghost = c.issue(Surface::GhostCompletion);
        assert!(
            !chat.is_cancelled(),
            "a ghost request must not kill the panel's answer"
        );
        assert_eq!(c.superseded(), 0);
    }

    #[test]
    fn context_invalidation_cancels_everything_at_once() {
        let c = SurfaceCancellation::default();
        let a = c.issue(Surface::Chat);
        let b = c.issue(Surface::ResultExplain);
        c.cancel_all();
        assert!(a.is_cancelled() && b.is_cancelled());
    }

    #[tokio::test]
    async fn a_file_scope_batch_cannot_starve_an_interactive_surface() {
        // 07 §2.7's reserved permit, as the scenario it exists for: a file-scope
        // auto-comment plus a repro draft take the whole general pool, and the
        // quick-fix the user is staring at still runs.
        let c = Concurrency::default();
        let _batch1 = c.acquire(Surface::AutoComment).await;
        let _batch2 = c.acquire(Surface::ReproExplain).await;
        assert_eq!(c.batch_available(), 0);

        let interactive = c.acquire(Surface::QuickFix).await;
        assert!(
            interactive.reserved(),
            "the quick-fix took the reserve, not a general permit"
        );
    }

    #[tokio::test]
    async fn batch_work_can_never_take_the_reserve() {
        let c = Concurrency::default();
        let a = c.acquire(Surface::AutoComment).await;
        let b = c.acquire(Surface::Chat).await;
        assert!(!a.reserved() && !b.reserved());
        assert_eq!(c.interactive_available(), INTERACTIVE_PERMITS);
    }

    #[tokio::test]
    async fn dropping_a_slot_returns_the_permit() {
        let c = Concurrency::default();
        {
            let _a = c.acquire(Surface::Chat).await;
            assert_eq!(c.batch_available(), 1);
        }
        assert_eq!(c.batch_available(), 2);
    }

    #[test]
    fn the_preview_is_forced_at_full_and_on_any_increase() {
        let mut s = SessionCounters::default();
        assert!(
            s.preview_forced(PrivacyTier::SchemaOnly),
            "the first send of a session"
        );
        s.highest_tier_sent = Some(PrivacyTier::SchemaOnly);
        assert!(!s.preview_forced(PrivacyTier::SchemaOnly));
        assert!(s.preview_forced(PrivacyTier::SchemaAndStats), "an increase");
        assert!(s.preview_forced(PrivacyTier::Full), "full is always forced");
        s.highest_tier_sent = Some(PrivacyTier::SchemaAndStats);
        assert!(
            !s.preview_forced(PrivacyTier::SchemaOnly),
            "going down is not a surprise"
        );
    }

    #[test]
    fn tier_off_never_forces_a_preview_because_nothing_is_sent() {
        let s = SessionCounters::default();
        assert!(!s.preview_forced(PrivacyTier::Off));
    }

    #[test]
    fn the_high_water_mark_means_a_user_is_asked_once_not_every_time() {
        let mut s = SessionCounters::default();
        s.record(
            PrivacyTier::SchemaAndStats,
            TokenUsage::default(),
            ModelPrice::default(),
        );
        s.record(
            PrivacyTier::SchemaOnly,
            TokenUsage::default(),
            ModelPrice::default(),
        );
        assert_eq!(s.highest_tier_sent, Some(PrivacyTier::SchemaAndStats));
        assert!(!s.preview_forced(PrivacyTier::SchemaAndStats));
        assert_eq!(s.cost.requests, 2);
    }
}
