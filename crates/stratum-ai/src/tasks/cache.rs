//! `07` §11.3 layer 2 — the in-memory response cache.
//!
//! Re-clicking `[Explain]` on the same card is free and instant. That is the
//! whole feature, and it is the one people notice: the second click is the one
//! where they were checking they read it right.
//!
//! # Why this is not `moka`
//!
//! `07` §1.1 chose `moka` for "TTL + size eviction + concurrent access for
//! free", at about ten transitive crates. This is a bounded map behind a
//! `Mutex` with an explicit sweep, and the reason is not size — the standing
//! priority is speed over bytes. It is that the lock-free read path `moka` buys
//! is worth nothing here: the cache is consulted **once per user click**, never
//! per keystroke and never per row, so the contended case does not exist. A
//! `Mutex` held for one hash lookup at human frequency is not a performance
//! consideration in either direction.
//!
//! # The clock is an argument
//!
//! Every method takes `now_unix_ms`. Nothing here reads a clock, so the TTL is
//! testable without sleeping and the whole module is deterministic — which is
//! also what lets `AiService` use one clock for the cache, the audit log and the
//! session counters instead of three that can disagree.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use crate::provider::types::{ModelId, ProviderId, TokenUsage};

/// `07` §11.3: 256 entries.
pub const CAPACITY: usize = 256;

/// `07` §11.3: a 30-minute TTL.
pub const TTL_MS: u64 = 30 * 60 * 1_000;

/// The cache key: `blake3(provider ‖ model ‖ rendered_prompt ‖ prompt_version)`.
///
/// The prompt version is in the key so that editing a prompt invalidates cached
/// answers rather than serving an answer to a question we no longer ask.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct CacheKey([u8; 16]);

impl CacheKey {
    /// Derive a key.
    #[must_use]
    pub fn new(provider: ProviderId, model: &ModelId, prompt: &str, prompt_version: u32) -> Self {
        let mut h = blake3::Hasher::new();
        h.update(provider.as_str().as_bytes());
        h.update(b"\0");
        h.update(model.as_str().as_bytes());
        h.update(b"\0");
        h.update(prompt.as_bytes());
        h.update(b"\0");
        h.update(&prompt_version.to_le_bytes());
        let full = h.finalize();
        let mut key = [0u8; 16];
        key.copy_from_slice(&full.as_bytes()[..16]);
        Self(key)
    }

    /// The raw key, for the persistent store's table.
    #[must_use]
    pub const fn bytes(self) -> [u8; 16] {
        self.0
    }

    /// Hex, for a log line.
    #[must_use]
    pub fn to_hex(self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }
}

/// A cached answer.
#[derive(Clone, PartialEq, Debug)]
pub struct CachedResponse {
    /// The complete text.
    pub text: String,
    /// The usage the original request reported. Replayed so the session's token
    /// total stays truthful about what was actually billed — a cache hit costs
    /// nothing and must not be counted twice.
    pub usage: TokenUsage,
}

struct Entry {
    value: CachedResponse,
    stored_at: u64,
    /// Monotonic within this cache; the eviction victim is the smallest.
    used: u64,
}

/// A bounded, TTL'd response cache.
#[derive(Debug)]
pub struct ResponseCache {
    inner: Mutex<HashMap<CacheKey, Entry>>,
    clock: AtomicU64,
    hits: AtomicU64,
    misses: AtomicU64,
    capacity: usize,
    ttl_ms: u64,
}

impl Default for ResponseCache {
    fn default() -> Self {
        Self::new(CAPACITY, TTL_MS)
    }
}

impl std::fmt::Debug for Entry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Entry")
            .field("stored_at", &self.stored_at)
            .finish_non_exhaustive()
    }
}

impl ResponseCache {
    /// Build with an explicit capacity and TTL.
    #[must_use]
    pub fn new(capacity: usize, ttl_ms: u64) -> Self {
        Self {
            inner: Mutex::new(HashMap::with_capacity(capacity.min(CAPACITY))),
            clock: AtomicU64::new(0),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            capacity: capacity.max(1),
            ttl_ms,
        }
    }

    /// Look one up, counting the hit or the miss.
    #[must_use]
    pub fn get(&self, key: CacheKey, now_unix_ms: u64) -> Option<CachedResponse> {
        let tick = self.clock.fetch_add(1, Ordering::Relaxed);
        let mut map = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let hit = match map.get_mut(&key) {
            Some(e) if now_unix_ms.saturating_sub(e.stored_at) < self.ttl_ms => {
                e.used = tick;
                Some(e.value.clone())
            }
            Some(_) => {
                map.remove(&key);
                None
            }
            None => None,
        };
        if hit.is_some() {
            self.hits.fetch_add(1, Ordering::Relaxed);
        } else {
            self.misses.fetch_add(1, Ordering::Relaxed);
        }
        hit
    }

    /// Store one, sweeping expired entries and evicting the least recently used
    /// if that was not enough.
    pub fn put(&self, key: CacheKey, value: CachedResponse, now_unix_ms: u64) {
        let tick = self.clock.fetch_add(1, Ordering::Relaxed);
        let mut map = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        map.retain(|_, e| now_unix_ms.saturating_sub(e.stored_at) < self.ttl_ms);
        while map.len() >= self.capacity {
            let Some(victim) = map.iter().min_by_key(|(_, e)| e.used).map(|(k, _)| *k) else {
                break;
            };
            map.remove(&victim);
        }
        map.insert(
            key,
            Entry {
                value,
                stored_at: now_unix_ms,
                used: tick,
            },
        );
    }

    /// Hits and misses since construction. The footer's "cache hit rate".
    #[must_use]
    pub fn counters(&self) -> (u64, u64) {
        (
            self.hits.load(Ordering::Relaxed),
            self.misses.load(Ordering::Relaxed),
        )
    }

    /// How many entries are held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    /// Whether anything is held.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// "Delete all AI history" purges this along with the audit log.
    pub fn clear(&self) {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(n: u32) -> CacheKey {
        CacheKey::new(
            ProviderId::Anthropic,
            &ModelId::from("claude-opus-5"),
            "prompt",
            n,
        )
    }

    fn response(text: &str) -> CachedResponse {
        CachedResponse {
            text: text.to_owned(),
            usage: TokenUsage::default(),
        }
    }

    #[test]
    fn re_asking_the_same_question_is_a_hit_and_costs_nothing() {
        let c = ResponseCache::default();
        assert!(c.get(key(1), 0).is_none());
        c.put(key(1), response("because"), 0);
        assert_eq!(c.get(key(1), 1).map(|r| r.text), Some("because".to_owned()));
        assert_eq!(c.counters(), (1, 1));
    }

    #[test]
    fn a_prompt_version_bump_invalidates_every_cached_answer() {
        let c = ResponseCache::default();
        c.put(key(1), response("v1 answer"), 0);
        assert!(
            c.get(key(2), 0).is_none(),
            "a new prompt must not serve an old answer"
        );
    }

    #[test]
    fn an_entry_past_its_ttl_is_a_miss_and_is_dropped() {
        let c = ResponseCache::new(8, 1_000);
        c.put(key(1), response("stale"), 0);
        assert!(c.get(key(1), 1_000).is_none());
        assert_eq!(
            c.len(),
            0,
            "an expired entry is removed on the miss, not left to rot"
        );
    }

    #[test]
    fn the_cache_never_exceeds_its_capacity() {
        // The bound is the whole reason a hand-rolled map is acceptable here.
        let c = ResponseCache::new(4, TTL_MS);
        for i in 0..64 {
            c.put(key(i), response("x"), 0);
        }
        assert!(c.len() <= 4, "{}", c.len());
    }

    #[test]
    fn eviction_takes_the_least_recently_used() {
        let c = ResponseCache::new(2, TTL_MS);
        c.put(key(1), response("one"), 0);
        c.put(key(2), response("two"), 0);
        // Touch 1 so 2 becomes the victim.
        assert!(c.get(key(1), 0).is_some());
        c.put(key(3), response("three"), 0);
        assert!(
            c.get(key(1), 0).is_some(),
            "the recently used entry survived"
        );
        assert!(
            c.get(key(2), 0).is_none(),
            "the least recently used was evicted"
        );
    }

    #[test]
    fn clearing_leaves_nothing_behind() {
        let c = ResponseCache::default();
        c.put(key(1), response("x"), 0);
        c.clear();
        assert!(c.is_empty());
    }

    #[test]
    fn a_different_model_is_a_different_key() {
        let c = ResponseCache::default();
        let opus = CacheKey::new(
            ProviderId::Anthropic,
            &ModelId::from("claude-opus-5"),
            "p",
            1,
        );
        let haiku = CacheKey::new(
            ProviderId::Anthropic,
            &ModelId::from("claude-haiku-4-5"),
            "p",
            1,
        );
        c.put(opus, response("opus"), 0);
        assert!(c.get(haiku, 0).is_none());
    }
}
