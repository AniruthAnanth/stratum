//! `07` §5.6 — the cache-stable system prefix.
//!
//! # Why the bytes are locked
//!
//! Everything up to and including the `cache: true` chunk must be byte-identical
//! across requests or the provider's prompt cache silently misses. "Silently" is
//! the load-bearing word: nothing fails, the answers stay correct, and the cost
//! triples. `07` §5.6 asks for an `insta` snapshot; this locks the `blake3` of
//! each assembled framing against a committed constant instead, which is the
//! same guarantee with no snapshot files to accidentally re-bless — `cargo insta
//! accept` is one keystroke and this is one edit to a constant with a comment
//! saying what it costs.
//!
//! # What the prefix must not contain
//!
//! No timestamps, no paths, no counts, no dataset name — anything that varies
//! per request, per user or per machine belongs in the volatile chunk.
//! [`tests::the_cached_prefix_carries_nothing_that_varies`] checks the ones that
//! are mechanically checkable.
//!
//! # Assembly order
//!
//! `07` §5.6 fixes it: `TASK_FRAMING + STATA_STYLE_RULES + OUTPUT_CONTRACT`. Each
//! task file therefore carries its framing and its contract split by
//! [`CONTRACT_MARKER`], and the shared style rules are spliced between them. The
//! contract is last because it is the part the model is most likely to honour if
//! it is the last thing it read.

use crate::service::surface::Surface;

/// Bumped whenever any prompt's bytes change. It is part of the response-cache
/// key ([`super::cache`]), so a prompt edit invalidates cached answers rather
/// than serving an answer to a question we no longer ask.
pub const PROMPT_VERSION: u32 = 1;

/// Splits a task prompt into its framing and its output contract.
pub const CONTRACT_MARKER: &str = "\n<!-- output-contract -->\n";

const SHARED: &str = include_str!("prompts/_shared.md");

const QUICK_FIX: &str = include_str!("prompts/quick_fix.md");
const RESULT_EXPLAIN: &str = include_str!("prompts/result_explain.md");
const CHECK_MODEL: &str = include_str!("prompts/check_model.md");
const NEXT_STEP: &str = include_str!("prompts/next_step.md");
const GHOST_COMPLETION: &str = include_str!("prompts/ghost_completion.md");
const AUTO_COMMENT: &str = include_str!("prompts/auto_comment.md");
const REPRO_EXPLAIN: &str = include_str!("prompts/repro_explain.md");
const HISTORY_CLEANUP: &str = include_str!("prompts/history_cleanup.md");
const CHAT: &str = include_str!("prompts/chat.md");

/// The raw task prompt for a surface, framing and contract still joined.
#[must_use]
pub const fn source(surface: Surface) -> &'static str {
    match surface {
        Surface::QuickFix => QUICK_FIX,
        Surface::ResultExplain => RESULT_EXPLAIN,
        Surface::CheckModel => CHECK_MODEL,
        Surface::NextStep => NEXT_STEP,
        Surface::GhostCompletion => GHOST_COMPLETION,
        Surface::AutoComment => AUTO_COMMENT,
        Surface::ReproExplain => REPRO_EXPLAIN,
        Surface::HistoryCleanup => HISTORY_CLEANUP,
        Surface::Chat => CHAT,
    }
}

/// The shared Stata style rules.
#[must_use]
pub const fn shared() -> &'static str {
    SHARED
}

/// The assembled, cache-stable system prefix for a surface.
///
/// Deterministic in its argument and nothing else: no clock, no environment, no
/// filesystem. That is the property the cache depends on.
#[must_use]
pub fn framing(surface: Surface) -> String {
    let src = source(surface);
    let (task, contract) = match src.split_once(CONTRACT_MARKER) {
        Some(pair) => pair,
        // Unreachable in a shipped build — `every_prompt_has_exactly_one_contract`
        // fails first. Degrading to "no contract" rather than panicking is still
        // the right behaviour: a missing output contract makes an answer worse,
        // and taking the AI panel down makes the whole product worse.
        None => (src, ""),
    };
    format!(
        "{}\n{}\n{}",
        task.trim_end(),
        SHARED.trim_end(),
        contract.trim_end()
    )
}

/// The `blake3` of a surface's framing, hex, first 32 chars.
///
/// Logged with the cache-health warning of `07` §11.3: when the second request
/// of a session reports `cache_read_input_tokens == 0`, this is what identifies
/// which prefix moved.
#[must_use]
pub fn framing_hash(surface: Surface) -> String {
    blake3::hash(framing(surface).as_bytes()).to_hex()[..32].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::backends::anthropic::estimate_tokens;

    /// The locked bytes. **Changing a prompt invalidates every user's prompt
    /// cache**, which is a real cost paid by real people; this test exists so
    /// that cost is paid deliberately. If you meant it, update the constant in
    /// the same commit as the prompt and say why in the message.
    ///
    /// These are the `blake3` of the shipped `prompts/*.md`, taken the first
    /// time this crate compiled. The rows they replace were never produced by a
    /// compiler — the crate did not build, so this test had never run — and a
    /// lock nothing ever checked is not a lock.
    const LOCKED: [(Surface, &str); 9] = [
        (Surface::QuickFix, "4451c5a854becf717b5f2655a30d6b4f"),
        (Surface::ResultExplain, "3f623255d2c588a4476c006fa0c6936c"),
        (Surface::CheckModel, "927131b9a0fcddd9e8dc06e80bfbef1f"),
        (Surface::NextStep, "276aede9c1fa2f421555b65df3084a6d"),
        (Surface::GhostCompletion, "7098bd2426f231654b9ffeeb64d5d5f0"),
        (Surface::AutoComment, "65a7e5b9c22383a7c390908500ca3431"),
        (Surface::ReproExplain, "f936e34bf1a9f920bb3c78f0e7f90c5c"),
        (Surface::HistoryCleanup, "12396d0750fe3b34a949ac9163e6dde5"),
        (Surface::Chat, "9ac2823376aab6c444f0f342697288c3"),
    ];

    #[test]
    fn every_prompt_has_exactly_one_contract_marker() {
        for s in Surface::ALL {
            assert_eq!(
                source(s).matches(CONTRACT_MARKER).count(),
                1,
                "{s} must split into framing and output contract exactly once"
            );
        }
    }

    #[test]
    fn the_cached_prefix_is_large_enough_to_be_cacheable() {
        // 07 §5.6: chunk 0 must be ≥1024 tokens or the provider will not cache
        // it at all, and the whole breakpoint is decorative.
        for s in Surface::ALL {
            let n = estimate_tokens(&framing(s));
            assert!(
                n >= 1024,
                "{s} framing is {n} tokens; under the cache floor"
            );
        }
    }

    #[test]
    fn the_cached_prefix_carries_nothing_that_varies() {
        // No timestamps, no paths, no counts, no dataset name. A prefix that
        // varies per request is not a prefix, it is a cache miss with extra
        // steps.
        for s in Surface::ALL {
            let f = framing(s);
            for forbidden in ["/Users/", "C:\\", "/home/", "auto.dta", "20260"] {
                assert!(!f.contains(forbidden), "{s} framing contains {forbidden:?}");
            }
        }
    }

    #[test]
    fn the_shared_rules_appear_once_in_every_framing() {
        for s in Surface::ALL {
            let f = framing(s);
            assert!(
                f.contains("Missing is not zero"),
                "{s} lost the style rules"
            );
            assert_eq!(
                f.matches("# Stratum").count(),
                1,
                "{s} spliced the rules twice"
            );
        }
    }

    #[test]
    fn the_output_contract_is_last() {
        // The model honours the last thing it read more reliably than the first.
        for s in Surface::ALL {
            let f = framing(s);
            let contract = f.rfind("## Output").expect("every prompt names its output");
            let rules = f
                .rfind("Missing is not zero")
                .expect("the style rules are in there");
            assert!(
                contract > rules,
                "{s}: the contract must follow the style rules"
            );
        }
    }

    #[test]
    fn every_framing_is_byte_locked() {
        let mut wrong = Vec::new();
        for (s, expect) in LOCKED {
            let got = framing_hash(s);
            if got != expect {
                wrong.push(format!("({s:?}, \"{got}\"),"));
            }
        }
        assert!(
            wrong.is_empty(),
            "a prompt changed, which invalidates every user's prompt cache.\n\
             If that was intended, replace the LOCKED rows with:\n{}",
            wrong.join("\n")
        );
    }

    #[test]
    fn every_surface_has_its_own_prompt() {
        let mut hashes: Vec<String> = Surface::ALL.iter().map(|s| framing_hash(*s)).collect();
        hashes.sort();
        hashes.dedup();
        assert_eq!(hashes.len(), 9, "two surfaces share a framing");
    }
}
