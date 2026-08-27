//! 07 §2.6 — retries, and the four rules that matter more than the backoff curve.
//!
//! 1. Retry only transient classes. A 400/401/403/404/413/422 is our bug or the
//!    user's config; retrying burns the deadline and changes nothing.
//! 2. **Never retry once a `TextDelta` has been yielded downstream.** A restart
//!    would either duplicate visible text or silently produce a second, different
//!    answer. Past that point a mid-stream failure is a partial result with a
//!    `[Retry]` button the user presses.
//! 3. Honour `Retry-After`; otherwise `min(cap, base·factorⁿ)` with **full**
//!    jitter — `rand_range(0, computed)`, not equal jitter, because a file-scope
//!    auto-comment batch can fail as one and must not thunder.
//! 4. Abort the whole chain when the surface's total budget is exhausted, even
//!    mid-sleep.

use std::time::Duration;

use super::error::ProviderError;

/// Retry configuration. Defaults are 07 §2.6 verbatim; `total_budget` comes
/// from the per-surface table in 07 §5.2.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct RetryPolicy {
    /// Including the first try. `3` means at most two retries.
    pub max_attempts: u32,
    /// First backoff ceiling.
    pub base: Duration,
    /// Multiplier per attempt.
    pub factor: f64,
    /// Backoff ceiling cap.
    pub cap: Duration,
    /// Wall-clock budget for the whole chain, sleeps included.
    pub total_budget: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            base: Duration::from_millis(500),
            factor: 2.0,
            cap: Duration::from_secs(8),
            total_budget: Duration::from_secs(15),
        }
    }
}

impl RetryPolicy {
    /// A policy that never retries. `GhostCompletion`'s row in 07 §5.2 has a
    /// retry budget of zero: an 800 ms deadline leaves no room, and a suggestion
    /// that arrives after the user typed the next character is discarded anyway.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            max_attempts: 1,
            base: Duration::ZERO,
            factor: 1.0,
            cap: Duration::ZERO,
            total_budget: Duration::ZERO,
        }
    }

    /// `min(cap, base · factorⁿ)`, computed by repeated multiplication.
    ///
    /// `powf`/`powi` are on `clippy.toml`'s `disallowed-methods` list
    /// (ARCHITECTURE §8.11) — a global determinism rule for the numeric kernels
    /// that costs nothing to honour here.
    #[must_use]
    pub fn backoff_ceiling(&self, attempt: u32) -> Duration {
        let mut secs = self.base.as_secs_f64();
        for _ in 0..attempt {
            secs *= self.factor;
        }
        let capped = secs.min(self.cap.as_secs_f64());
        Duration::from_secs_f64(capped.max(0.0))
    }
}

/// What the caller should do next.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RetryDecision {
    /// Sleep this long, then try again.
    After(Duration),
    /// Give up and surface the error.
    Give,
}

/// The inputs a retry decision is made from. A struct rather than six positional
/// arguments because two of them are booleans and a transposed pair would be a
/// silent behaviour change.
#[derive(Clone, Copy, Debug)]
pub struct Attempt<'a> {
    /// 0 for the first failure.
    pub index: u32,
    /// How much of `total_budget` is already spent.
    pub elapsed: Duration,
    /// The failure.
    pub error: &'a ProviderError,
    /// Whether any `TextDelta` has already reached the caller. Rule 2.
    pub streamed_text: bool,
    /// A `Retry-After` header, already parsed.
    pub retry_after: Option<Duration>,
    /// A uniform sample in `[0, 1)` for full jitter.
    pub jitter_unit: f64,
}

/// Decide whether to retry.
#[must_use]
pub fn decide(policy: &RetryPolicy, attempt: &Attempt<'_>) -> RetryDecision {
    // Rule 2 first: it overrides everything, including a Retry-After header.
    if attempt.streamed_text {
        return RetryDecision::Give;
    }
    if !attempt.error.retryable() {
        return RetryDecision::Give;
    }
    if attempt.index + 1 >= policy.max_attempts {
        return RetryDecision::Give;
    }
    if attempt.elapsed >= policy.total_budget {
        return RetryDecision::Give;
    }

    let wait = match attempt.retry_after {
        Some(d) => d,
        None => {
            let ceiling = policy.backoff_ceiling(attempt.index);
            full_jitter(ceiling, attempt.jitter_unit)
        }
    };

    // Rule 4: a sleep that would outlast the budget is not a sleep, it is a
    // give-up with extra latency.
    let remaining = policy.total_budget.saturating_sub(attempt.elapsed);
    if wait >= remaining {
        return RetryDecision::Give;
    }
    RetryDecision::After(wait)
}

/// Full jitter: uniform on `[0, ceiling)`.
#[must_use]
pub fn full_jitter(ceiling: Duration, unit: f64) -> Duration {
    let u = unit.clamp(0.0, 1.0);
    Duration::from_secs_f64(ceiling.as_secs_f64() * u)
}

/// Parse a `Retry-After` header value.
///
/// Only the delta-seconds form is honoured. The HTTP-date form is legal and is
/// deliberately ignored: parsing it needs a date library, `deny.toml` keeps
/// `time` and `chrono` out of this crate (A2), and every provider we target
/// sends seconds. A date we cannot parse falls back to the jittered backoff,
/// which is a slightly worse wait, not a wrong one.
#[must_use]
pub fn parse_retry_after(value: &str) -> Option<Duration> {
    let secs: u64 = value.trim().parse().ok()?;
    // A provider asking us to wait an hour is asking us to give up.
    (secs <= 300).then(|| Duration::from_secs(secs))
}

/// A process-local source of jitter.
///
/// `rand` is not in the workspace dependency table and a whole RNG crate for one
/// uniform sample per failed request is not worth its tree. `RandomState`'s
/// per-process random seed hashed with a monotonically increasing counter gives
/// a value that differs between processes and between calls, which is the
/// entire requirement: full jitter exists so several clients do not retry in
/// lockstep, not so anyone can bet on the outcome.
#[derive(Debug)]
pub struct Jitter {
    state: std::sync::atomic::AtomicU64,
    seed: u64,
}

impl Default for Jitter {
    fn default() -> Self {
        use std::hash::{BuildHasher as _, Hasher as _};
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(0x9E37_79B9_7F4A_7C15);
        Self {
            state: std::sync::atomic::AtomicU64::new(0),
            seed: h.finish(),
        }
    }
}

impl Jitter {
    /// The next uniform sample in `[0, 1)`.
    #[must_use]
    pub fn next_unit(&self) -> f64 {
        let n = self
            .state
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // SplitMix64. Small, well-distributed, and no dependency.
        let mut z = self
            .seed
            .wrapping_add(n.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        // 53 bits of mantissa; the standard [0,1) construction.
        ((z >> 11) as f64) / ((1u64 << 53) as f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attempt<'a>(index: u32, error: &'a ProviderError) -> Attempt<'a> {
        Attempt {
            index,
            elapsed: Duration::ZERO,
            error,
            streamed_text: false,
            retry_after: None,
            jitter_unit: 1.0,
        }
    }

    #[test]
    fn a_401_is_never_retried_however_early() {
        let e = ProviderError::Unauthorized;
        assert_eq!(
            decide(&RetryPolicy::default(), &attempt(0, &e)),
            RetryDecision::Give
        );
    }

    #[test]
    fn a_429_is_retried_until_max_attempts() {
        let policy = RetryPolicy::default();
        let e = ProviderError::RateLimited(None);
        assert!(matches!(
            decide(&policy, &attempt(0, &e)),
            RetryDecision::After(_)
        ));
        assert!(matches!(
            decide(&policy, &attempt(1, &e)),
            RetryDecision::After(_)
        ));
        // max_attempts = 3, so index 2 is the third and last try.
        assert_eq!(decide(&policy, &attempt(2, &e)), RetryDecision::Give);
    }

    #[test]
    fn once_text_has_been_streamed_nothing_is_retried() {
        // The rule that stops the product from either duplicating visible text
        // or silently producing a second, different answer.
        let policy = RetryPolicy::default();
        let e = ProviderError::Overloaded;
        let mut a = attempt(0, &e);
        a.streamed_text = true;
        assert_eq!(decide(&policy, &a), RetryDecision::Give);

        // ... and a Retry-After header does not override it.
        a.retry_after = Some(Duration::from_secs(1));
        assert_eq!(decide(&policy, &a), RetryDecision::Give);
    }

    #[test]
    fn the_total_budget_aborts_the_chain_even_when_attempts_remain() {
        let policy = RetryPolicy {
            total_budget: Duration::from_secs(1),
            ..Default::default()
        };
        let e = ProviderError::Overloaded;
        let mut a = attempt(0, &e);
        a.elapsed = Duration::from_millis(1200);
        assert_eq!(decide(&policy, &a), RetryDecision::Give);
    }

    #[test]
    fn a_sleep_that_would_outlast_the_budget_is_a_give_up() {
        let policy = RetryPolicy {
            total_budget: Duration::from_millis(400),
            ..Default::default()
        };
        let e = ProviderError::Overloaded;
        // jitter_unit 1.0 makes the wait the full 500 ms ceiling.
        assert_eq!(decide(&policy, &attempt(0, &e)), RetryDecision::Give);
    }

    #[test]
    fn backoff_is_capped() {
        let p = RetryPolicy::default();
        assert_eq!(p.backoff_ceiling(0), Duration::from_millis(500));
        assert_eq!(p.backoff_ceiling(1), Duration::from_millis(1000));
        assert_eq!(p.backoff_ceiling(2), Duration::from_millis(2000));
        // 500ms · 2^6 = 32s, capped at 8s.
        assert_eq!(p.backoff_ceiling(6), Duration::from_secs(8));
    }

    #[test]
    fn full_jitter_spans_zero_to_the_ceiling() {
        let c = Duration::from_millis(800);
        assert_eq!(full_jitter(c, 0.0), Duration::ZERO);
        assert_eq!(full_jitter(c, 1.0), c);
        assert_eq!(full_jitter(c, 0.5), Duration::from_millis(400));
        // Out-of-range input is clamped rather than producing a negative sleep.
        assert_eq!(full_jitter(c, -3.0), Duration::ZERO);
    }

    #[test]
    fn retry_after_seconds_is_honoured_and_an_http_date_is_not_pretended_to_parse() {
        assert_eq!(parse_retry_after("7"), Some(Duration::from_secs(7)));
        assert_eq!(parse_retry_after(" 12 "), Some(Duration::from_secs(12)));
        assert_eq!(parse_retry_after("Wed, 21 Oct 2026 07:28:00 GMT"), None);
        // An unreasonable wait is a give-up, not a five-hour sleep.
        assert_eq!(parse_retry_after("18000"), None);
    }

    #[test]
    fn the_never_retry_policy_never_retries() {
        let e = ProviderError::Overloaded;
        assert_eq!(
            decide(&RetryPolicy::none(), &attempt(0, &e)),
            RetryDecision::Give
        );
    }

    #[test]
    fn jitter_samples_stay_in_range_and_are_not_all_equal() {
        let j = Jitter::default();
        let samples: Vec<f64> = (0..64).map(|_| j.next_unit()).collect();
        assert!(samples.iter().all(|u| (0.0..1.0).contains(u)));
        let first = samples[0];
        assert!(
            samples.iter().any(|u| (*u - first).abs() > 1e-9),
            "a constant jitter source is not jitter"
        );
    }
}
