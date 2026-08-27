//! The completion environment, on the READ side of the boundary.
//!
//! `CompletionEnv` is bounded by A11 and `CompletionEnv::enforce_bounds` is the
//! producer's obligation: the engine caps before the value leaves it. This
//! module is the consumer's half of the same rule, and it exists because the two
//! obligations are not the same obligation.
//!
//! `Engine::set_completion_env` decodes whatever msgpack arrives. If a producer
//! forgets to cap — a future engine version, a replayed session, a test harness,
//! anything that is not today's `stratum-exec` — an uncapped environment lands
//! in the engine and every subsequent keystroke pays for it, inside a 2 ms hard
//! contract, on the main thread, inside the CodeMirror transaction cycle. So the
//! completer reads through [`varnames`] and [`other`] rather than through the
//! fields directly, and the budget then holds against any input at all.
//!
//! That is a bound, not a repair: the counts reported to the popup still come
//! from what the ENGINE said it shed ([`offered_of_total`]), because "2 048 of
//! 32 767" is a statement about the dataset, not about this crate's slicing.

use stratum_proto::{CompletionEnv, COMPLETION_ENV_MAX_OTHER, COMPLETION_ENV_MAX_VARS};

/// Variable names, never more than A11's cap.
#[must_use]
pub fn varnames(env: &CompletionEnv) -> &[String] {
    let n = env.varnames.len().min(COMPLETION_ENV_MAX_VARS);
    &env.varnames[..n]
}

/// Any other list, never more than A11's cap.
#[must_use]
pub fn other(list: &[String]) -> &[String] {
    let n = list.len().min(COMPLETION_ENV_MAX_OTHER);
    &list[..n]
}

/// True when the environment arrived over a cap — i.e. the producer did not
/// call `enforce_bounds`. Surfaced by the tests rather than by a diagnostic:
/// the popup still works, it is the producer that is wrong.
#[must_use]
pub fn exceeds_caps(env: &CompletionEnv) -> bool {
    env.varnames.len() > COMPLETION_ENV_MAX_VARS
        || [
            &env.frames,
            &env.locals,
            &env.globals,
            &env.scalars,
            &env.matrices,
            &env.programs,
            &env.e_names,
            &env.r_names,
            &env.value_labels,
            &env.stored_estimates,
        ]
        .iter()
        .any(|l| l.len() > COMPLETION_ENV_MAX_OTHER)
}

/// The pair the popup renders as "2 048 of 32 767".
///
/// `offered` is how many names this environment actually carries, `total` how
/// many exist. They are equal unless the engine shed entries, which is exactly
/// what `CompletionEnv::truncated` means.
#[must_use]
pub fn offered_of_total(env: &CompletionEnv) -> (u32, u32) {
    let offered = varnames(env).len() as u32;
    (offered, env.var_total.max(offered))
}

/// Upper bound on the number of candidate names one `complete()` can examine.
///
/// ADR-017's counter for §14's 2 ms contract: a duration on a developer laptop
/// swung 33 % on an unchanged tree, but "how many names are scanned" is a
/// property of the code and cannot move under load. `benches/resegment.rs`
/// records the duration beside it.
#[must_use]
pub fn scan_budget(env: &CompletionEnv) -> usize {
    varnames(env).len()
        + [
            &env.frames,
            &env.locals,
            &env.globals,
            &env.scalars,
            &env.matrices,
            &env.programs,
            &env.e_names,
            &env.r_names,
            &env.value_labels,
            &env.stored_estimates,
        ]
        .iter()
        .map(|l| other(l).len())
        .sum::<usize>()
}

/// The scan bound that holds for EVERY environment, however malformed.
#[must_use]
pub const fn scan_ceiling() -> usize {
    COMPLETION_ENV_MAX_VARS + 10 * COMPLETION_ENV_MAX_OTHER
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(n: usize, tag: &str) -> Vec<String> {
        (0..n).map(|i| format!("{tag}{i:06}")).collect()
    }

    #[test]
    fn an_uncapped_environment_is_still_read_within_the_cap() {
        let env = CompletionEnv {
            varnames: names(32_767, "v"),
            var_total: 32_767,
            programs: names(5_000, "p"),
            ..CompletionEnv::default()
        };
        assert!(
            exceeds_caps(&env),
            "the fixture is meant to be over the cap"
        );
        assert_eq!(varnames(&env).len(), COMPLETION_ENV_MAX_VARS);
        assert_eq!(other(&env.programs).len(), COMPLETION_ENV_MAX_OTHER);
        assert!(scan_budget(&env) <= scan_ceiling());
    }

    #[test]
    fn a_capped_environment_is_passed_through_untouched() {
        let mut env = CompletionEnv {
            varnames: names(32_767, "v"),
            var_total: 32_767,
            ..CompletionEnv::default()
        };
        env.enforce_bounds();
        assert!(!exceeds_caps(&env));
        assert_eq!(varnames(&env).len(), env.varnames.len());
        assert_eq!(offered_of_total(&env), (2048, 32_767));
        assert!(env.truncated);
    }

    #[test]
    fn total_never_reads_below_offered() {
        // A producer that filled `varnames` and forgot `var_total` would make
        // the popup say "12 of 0". It says "12 of 12" instead.
        let env = CompletionEnv {
            varnames: names(12, "v"),
            var_total: 0,
            ..CompletionEnv::default()
        };
        assert_eq!(offered_of_total(&env), (12, 12));
    }

    #[test]
    fn the_ceiling_is_the_documented_a11_arithmetic() {
        assert_eq!(scan_ceiling(), 2048 + 10 * 512);
    }
}
