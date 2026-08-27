//! The ranker — design 07 §7.1.
//!
//! > Ranking is deterministic and stable: exact prefix → case-insensitive prefix
//! > → subsequence score → edit distance. Ties break on (a) used in this
//! > session, most recent first, (b) frequency in this file, (c) dataset order,
//! > (d) lexicographic. **No AI reranking, ever.** A completion list that
//! > reorders between keystrokes is the single most user-hostile behaviour in
//! > this space.
//!
//! Two properties matter as much as the order itself.
//!
//! **It is a total order.** Every comparison chain ends in the label, which is
//! unique within a source, so two runs over the same environment produce
//! byte-identical lists. A popup whose order depends on hash iteration is a
//! popup that moves under the user's finger.
//!
//! **It does not allocate per candidate.** Matching works on `&str` borrowed
//! from the environment and the static tables; only the surviving
//! [`super::MAX_ITEMS`] rows are ever turned into `String`s. At design 07 A11's
//! measurement cap — 2 048 variables and 512 of everything else — an `Expr`
//! completion with an empty prefix matches every one of ~7 000 names, and
//! cloning them all to throw all but 256 away would be most of the 2 ms budget.

use core::cmp::Ordering;

use crate::similarity::{damerau_levenshtein, subsequence_score};

use super::{CompletionItem, CompletionKind};

/// Which match tier a candidate landed in. Lower is better, and the tier
/// dominates every other criterion.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Tier {
    /// The typed text is a prefix, byte for byte.
    ExactPrefix,
    /// The typed text is a prefix, ignoring ASCII case.
    CasePrefix,
    /// The typed text is a subsequence.
    Subsequence,
    /// Within edit distance `ceil(len / 4)`.
    Near,
}

/// A candidate under consideration. Borrowed; nothing here owns a `String`.
#[derive(Clone, Copy, Debug)]
pub struct Candidate<'a> {
    /// The text shown and inserted.
    pub label: &'a str,
    /// What it is.
    pub kind: CompletionKind,
    /// Right-aligned annotation.
    pub detail: Option<&'a str>,
    /// Text to insert when it differs from the label (`"strpos("`).
    pub insert: Option<&'a str>,
    /// Source group, applied after the tier: variables before functions before
    /// keywords, so an `Expr` completion does not interleave them.
    pub group: u8,
    /// Position within its source. Design 07 §7.1's tie-break (c), "dataset
    /// order": the varlist arrives in storage order and keeps it.
    pub source_pos: u32,
    tier: Tier,
    score: i32,
    recency: u32,
    frequency: u32,
}

/// Accumulates candidates and produces the final ordered list.
pub struct Ranker<'a> {
    typed: &'a str,
    lower: String,
    budget: usize,
    hits: Vec<Candidate<'a>>,
    /// Every candidate examined, whether or not it matched. The counter ADR-017
    /// asks for: it is what proves the popup does one pass over a bounded
    /// universe rather than anything that grows with the data.
    pub scanned: u32,
}

impl<'a> Ranker<'a> {
    /// A ranker for the token being typed.
    #[must_use]
    pub fn new(typed: &'a str) -> Self {
        Ranker {
            typed,
            lower: typed.to_ascii_lowercase(),
            budget: typed.chars().count().div_ceil(4),
            hits: Vec::with_capacity(super::MAX_ITEMS),
            scanned: 0,
        }
    }

    /// Offer one candidate. Returns whether it matched.
    pub fn offer(
        &mut self,
        label: &'a str,
        kind: CompletionKind,
        group: u8,
        source_pos: u32,
        detail: Option<&'a str>,
        insert: Option<&'a str>,
        recency: u32,
        frequency: u32,
    ) -> bool {
        self.scanned += 1;
        let Some((tier, score)) = self.classify(label) else {
            return false;
        };
        self.hits.push(Candidate {
            label,
            kind,
            detail,
            insert,
            group,
            source_pos,
            tier,
            score,
            recency,
            frequency,
        });
        true
    }

    /// Offer a whole list in source order, with no session or file statistics.
    pub fn offer_all<I: Iterator<Item = &'a str>>(
        &mut self,
        names: I,
        kind: CompletionKind,
        group: u8,
    ) {
        for (i, name) in names.enumerate() {
            self.offer(name, kind, group, i as u32, None, None, u32::MAX, 0);
        }
    }

    fn classify(&self, label: &str) -> Option<(Tier, i32)> {
        if self.typed.is_empty() {
            return Some((Tier::ExactPrefix, 0));
        }
        if label.starts_with(self.typed) {
            // Shorter completions first inside the tier: `pri` should offer
            // `price` above `printed_matter`.
            return Some((Tier::ExactPrefix, -(label.len() as i32)));
        }
        if label.len() >= self.lower.len()
            && label
                .get(..self.lower.len())
                .is_some_and(|h| h.eq_ignore_ascii_case(&self.lower))
        {
            return Some((Tier::CasePrefix, -(label.len() as i32)));
        }
        if let Some(s) = subsequence_score(self.typed, label) {
            return Some((Tier::Subsequence, s));
        }
        let d = damerau_levenshtein(self.typed, label);
        (d <= self.budget).then_some((Tier::Near, -(d as i32)))
    }

    /// The ordered list, capped at [`super::MAX_ITEMS`].
    ///
    /// Returns `(items, offered, total)` where `total` is how many candidates
    /// matched and `offered` is how many survived the cap.
    #[must_use]
    pub fn finish(mut self) -> (Vec<CompletionItem>, u32, u32) {
        self.hits.sort_by(Self::order);
        let total = self.hits.len() as u32;
        self.hits.truncate(super::MAX_ITEMS);
        let offered = self.hits.len() as u32;
        let items = self
            .hits
            .iter()
            .enumerate()
            .map(|(rank, c)| CompletionItem {
                label: c.label.to_owned(),
                kind: c.kind,
                detail: c.detail.map(str::to_owned),
                insert: c.insert.map(str::to_owned),
                rank: rank as i32,
            })
            .collect();
        (items, offered, total)
    }

    /// The total order, written out so the tie-break chain is readable.
    fn order(a: &Candidate<'_>, b: &Candidate<'_>) -> Ordering {
        a.tier
            .cmp(&b.tier)
            .then_with(|| b.score.cmp(&a.score))
            .then_with(|| a.group.cmp(&b.group))
            // (a) used in this session, most recent first.
            .then_with(|| a.recency.cmp(&b.recency))
            // (b) frequency in this file, most first.
            .then_with(|| b.frequency.cmp(&a.frequency))
            // (c) dataset order.
            .then_with(|| a.source_pos.cmp(&b.source_pos))
            // (d) lexicographic — the tie-break that makes this a TOTAL order.
            .then_with(|| a.label.cmp(b.label))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;

    fn labels(typed: &str, names: &[&str]) -> Vec<String> {
        let mut r = Ranker::new(typed);
        r.offer_all(names.iter().copied(), CompletionKind::Variable, 0);
        r.finish().0.into_iter().map(|i| i.label).collect()
    }

    #[test]
    fn the_tiers_are_in_the_documented_order() {
        // `pri` : exact prefix `price`; case prefix `PRIvate`; subsequence
        // `p_r_i`; near `pri_ce` is also a prefix... so use a clean set.
        let got = labels("pri", &["price", "PRIvate", "pension_risk_index", "pig"]);
        assert_eq!(got[0], "price", "{got:?}");
        assert_eq!(got[1], "PRIvate", "{got:?}");
        assert_eq!(got[2], "pension_risk_index", "{got:?}");
    }

    #[test]
    fn a_shorter_completion_wins_inside_a_tier() {
        assert_eq!(labels("pri", &["printed_matter", "price"])[0], "price");
    }

    /// The chain ends in the label, so candidates that tie on every documented
    /// criterion come out in one fixed order however the environment happened to
    /// offer them — that is what "total order" buys, and it is what stops a
    /// stable-sort artefact from leaking into the popup.
    ///
    /// Note what this deliberately does NOT assert: that permuting the *source*
    /// leaves the list alone. It must not. `source_pos` is §7.1's tie-break (c),
    /// "dataset order", so a differently ordered varlist is a different
    /// environment and is *meant* to rank differently — see
    /// `an_empty_prefix_offers_everything_in_source_order`. Every candidate here
    /// is therefore offered at the same `source_pos`, which is what leaves the
    /// label as the only thing that can separate them.
    #[test]
    fn ranking_is_a_total_order_under_permutation() {
        fn run(names: &[&str]) -> Vec<String> {
            let mut r = Ranker::new("x");
            for n in names {
                r.offer(n, CompletionKind::Variable, 0, 0, None, None, u32::MAX, 0);
            }
            r.finish().0.into_iter().map(|i| i.label).collect()
        }
        // `yx` is only a subsequence match, so the tier keeps it last regardless.
        let expect = vec!["xa", "xb", "xc", "yx"];
        assert_eq!(run(&["xa", "xb", "xc", "yx"]), expect);
        assert_eq!(run(&["yx", "xc", "xb", "xa"]), expect);
        assert_eq!(run(&["xb", "yx", "xa", "xc"]), expect);
    }

    #[test]
    fn an_empty_prefix_offers_everything_in_source_order() {
        assert_eq!(labels("", &["b", "a", "c"]), vec!["b", "a", "c"]);
    }

    #[test]
    fn session_recency_then_file_frequency_break_a_tie() {
        let mut r = Ranker::new("v");
        r.offer(
            "va",
            CompletionKind::Variable,
            0,
            0,
            None,
            None,
            u32::MAX,
            0,
        );
        r.offer("vb", CompletionKind::Variable, 0, 1, None, None, 0, 0);
        assert_eq!(r.finish().0[0].label, "vb", "recent wins");

        let mut r = Ranker::new("v");
        r.offer(
            "va",
            CompletionKind::Variable,
            0,
            0,
            None,
            None,
            u32::MAX,
            1,
        );
        r.offer(
            "vb",
            CompletionKind::Variable,
            0,
            1,
            None,
            None,
            u32::MAX,
            9,
        );
        assert_eq!(r.finish().0[0].label, "vb", "frequent wins");
    }

    #[test]
    fn nothing_is_allocated_for_a_candidate_that_loses() {
        let names: Vec<String> = (0..5_000).map(|i| format!("v{i:05}")).collect();
        let mut r = Ranker::new("v");
        r.offer_all(
            names.iter().map(String::as_str),
            CompletionKind::Variable,
            0,
        );
        let (items, offered, total) = r.finish();
        assert_eq!(total, 5_000);
        assert_eq!(offered, super::super::MAX_ITEMS as u32);
        assert_eq!(items.len(), super::super::MAX_ITEMS);
    }
}
