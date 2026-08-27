//! String similarity — the arithmetic behind "Did you mean `income`?".
//!
//! Design 07 §0 is the whole argument for this file existing: the spec's
//! headline "AI" feature is edit distance over a live 74-element string vector.
//! It returns in microseconds, offline, free, with no failure mode, and it is
//! *correct* rather than *plausible*.
//!
//! # Why these are written out rather than depended on
//!
//! Design 07 §1.1 lists `strsim` and `nucleo-matcher`. Neither is in the
//! workspace dependency table (W00's file, not ours), and more importantly the
//! *numbers* here are contract, not implementation: §6.1 accepts a candidate at
//! `jaro_winkler >= 0.86` and promotes a single suggestion to
//! `Confidence::Exact` only when `top - second >= 0.08`. A silent change of
//! scoring function — `strsim` uncaps Winkler's prefix bonus and clamps at 1.0,
//! where Winkler's own definition caps the prefix at four characters — moves
//! every one of those decision boundaries. Written here, the definition is
//! pinned and unit-tested against the canonical worked examples.
//!
//! # Determinism
//!
//! Every function is pure, allocation-bounded by the length of its inputs, and
//! free of floating-point reassociation: the Jaro accumulators are integer
//! counts divided exactly once at the end, so two machines agree bit for bit.

// Every index here is either a loop counter into a `Vec` the same function just
// sized, or a byte offset guarded by the enclosing `while i < n`. The
// dynamic-programming tables run per candidate per keystroke over the whole
// varlist, so `.get()` in the innermost cell would buy an `Option` the code
// immediately discards — and it would move the bounds argument out of the loop
// header, where it is checkable, into a per-access apology. The crate keeps the
// lint on elsewhere for the cases where the bound is not visible.
#![allow(clippy::indexing_slicing)]

use core::cmp::Ordering;

/// Winkler's prefix-bonus weight. `p = 0.1` is the value in the original paper
/// and the one every implementation uses.
const WINKLER_P: f64 = 0.1;

/// Maximum prefix length that earns the bonus. Winkler caps it at four so that
/// `p * l <= 0.4` and the adjusted score cannot exceed 1.
const WINKLER_MAX_PREFIX: usize = 4;

/// Design 07 §6.1's acceptance threshold for a did-you-mean candidate.
pub const DID_YOU_MEAN_JW: f64 = 0.86;

/// Design 07 §6.1's margin for promoting the top candidate to a single
/// high-confidence fix instead of offering three.
pub const DID_YOU_MEAN_MARGIN: f64 = 0.08;

/// Jaro similarity in `[0, 1]`. `1.0` iff the two strings are equal.
///
/// Two characters match when they are equal and no further apart than
/// `max(len) / 2 - 1` positions; a transposition is a matched pair whose order
/// differs between the two strings.
#[must_use]
pub fn jaro(a: &str, b: &str) -> f64 {
    let x: Vec<char> = a.chars().collect();
    let y: Vec<char> = b.chars().collect();
    jaro_chars(&x, &y)
}

fn jaro_chars(x: &[char], y: &[char]) -> f64 {
    if x.is_empty() && y.is_empty() {
        return 1.0;
    }
    if x.is_empty() || y.is_empty() {
        return 0.0;
    }
    // `max/2 - 1`, saturating: for one-character strings the window is 0, which
    // is the definition's intent (only the same position can match).
    let window = (x.len().max(y.len()) / 2).saturating_sub(1);

    let mut x_hit = vec![false; x.len()];
    let mut y_hit = vec![false; y.len()];
    let mut matches = 0usize;

    for (i, xc) in x.iter().enumerate() {
        let lo = i.saturating_sub(window);
        let hi = (i + window + 1).min(y.len());
        for j in lo..hi {
            // `y_hit[j]` and `y[j]`: `j < y.len()` by the bound above.
            if !y_hit[j] && y[j] == *xc {
                x_hit[i] = true;
                y_hit[j] = true;
                matches += 1;
                break;
            }
        }
    }

    if matches == 0 {
        return 0.0;
    }

    // Transpositions: walk the two match subsequences in parallel.
    let mut transpositions = 0usize;
    let mut k = 0usize;
    for (i, hit) in x_hit.iter().enumerate() {
        if !*hit {
            continue;
        }
        while k < y.len() && !y_hit[k] {
            k += 1;
        }
        if k < y.len() && x[i] != y[k] {
            transpositions += 1;
        }
        k += 1;
    }
    let half_transpositions = transpositions / 2;

    let m = matches as f64;
    (m / x.len() as f64 + m / y.len() as f64 + (m - half_transpositions as f64) / m) / 3.0
}

/// Jaro–Winkler similarity in `[0, 1]`.
///
/// `jaro + l * p * (1 - jaro)` with `p = 0.1` and `l` the common prefix length
/// **capped at four**, which is Winkler's definition. The cap is what keeps the
/// result inside `[0, 1]` without a clamp, and it is why a long shared prefix
/// does not swamp the body of the name — `investment_income` against
/// `investment_incme` must not outrank `income` against `incme`.
#[must_use]
pub fn jaro_winkler(a: &str, b: &str) -> f64 {
    let x: Vec<char> = a.chars().collect();
    let y: Vec<char> = b.chars().collect();
    let j = jaro_chars(&x, &y);
    let prefix = x
        .iter()
        .zip(y.iter())
        .take(WINKLER_MAX_PREFIX)
        .take_while(|(p, q)| p == q)
        .count();
    j + prefix as f64 * WINKLER_P * (1.0 - j)
}

/// Unrestricted Damerau–Levenshtein distance: insertions, deletions,
/// substitutions and transpositions of two characters that need not be
/// adjacent in the *original* strings.
///
/// The restricted variant (optimal string alignment) is cheaper but is not a
/// metric — it reports `distance("ca", "abc") == 3` where the true edit script
/// is 2 — and the r(111) rule accepts on `distance <= ceil(len / 4)`, so an
/// over-reported distance silently drops a correct suggestion.
#[must_use]
pub fn damerau_levenshtein(a: &str, b: &str) -> usize {
    let x: Vec<char> = a.chars().collect();
    let y: Vec<char> = b.chars().collect();
    if x.is_empty() {
        return y.len();
    }
    if y.is_empty() {
        return x.len();
    }

    let n = x.len();
    let m = y.len();
    let inf = n + m;
    // (n+2) x (m+2), row 0 / column 0 holding the sentinel `inf` that makes the
    // transposition term fall away when there is no earlier occurrence.
    let w = m + 2;
    let mut d = vec![0usize; (n + 2) * w];
    let at = |i: usize, j: usize| i * w + j;

    d[at(0, 0)] = inf;
    for j in 0..=m {
        d[at(0, j + 1)] = inf;
        d[at(1, j + 1)] = j;
    }
    for i in 0..=n {
        d[at(i + 1, 0)] = inf;
        d[at(i + 1, 1)] = i;
    }

    // Last row in which each character was seen. A small association list
    // rather than a map: Stata names are short, and a linear scan over at most
    // `n` entries beats hashing on every cell.
    let mut last_row: Vec<(char, usize)> = Vec::with_capacity(n);

    for i in 1..=n {
        let mut last_match_col = 0usize;
        for j in 1..=m {
            let xi = x[i - 1];
            let yj = y[j - 1];
            let i1 = last_row
                .iter()
                .find(|(c, _)| *c == yj)
                .map_or(0, |(_, r)| *r);
            let j1 = last_match_col;
            let cost = usize::from(xi != yj);
            if cost == 0 {
                last_match_col = j;
            }
            let sub = d[at(i, j)] + cost;
            let ins = d[at(i + 1, j)] + 1;
            let del = d[at(i, j + 1)] + 1;
            let trans = d[at(i1, j1)]
                .saturating_add(i.saturating_sub(i1).saturating_sub(1))
                .saturating_add(1)
                .saturating_add(j.saturating_sub(j1).saturating_sub(1));
            d[at(i + 1, j + 1)] = sub.min(ins).min(del).min(trans);
        }
        match last_row.iter_mut().find(|(c, _)| *c == x[i - 1]) {
            Some(slot) => slot.1 = i,
            None => last_row.push((x[i - 1], i)),
        }
    }
    d[at(n + 1, m + 1)]
}

/// Design 07 §6.1's acceptance test for one candidate.
///
/// `jaro_winkler >= 0.86` **or** `damerau_levenshtein <= ceil(len / 4)`, where
/// `len` is the length of the token the user actually typed. The disjunction is
/// deliberate: Jaro–Winkler is good at the transposition-and-typo case and bad
/// at the truncation case (`incom` for `income`), and the distance bound is the
/// other way round.
#[must_use]
pub fn accepts(typed: &str, candidate: &str) -> bool {
    if typed.is_empty() {
        return false;
    }
    if jaro_winkler(typed, candidate) >= DID_YOU_MEAN_JW {
        return true;
    }
    let budget = typed.chars().count().div_ceil(4);
    damerau_levenshtein(typed, candidate) <= budget
}

/// A scored candidate. Ordered by score descending, then by name, so a ranked
/// list is a total order and two runs produce the same popup.
#[derive(Clone, PartialEq, Debug)]
pub struct Scored<T> {
    /// Whatever the caller is ranking.
    pub item: T,
    /// Jaro–Winkler similarity against the typed token.
    pub score: f64,
}

/// Rank `candidates` against `typed`, keeping only those that clear
/// [`accepts`], best first.
///
/// The output is capped at `limit` because the caller renders at most three
/// suggestions and a longer list is only work to throw away.
pub fn rank<'a, I>(typed: &str, candidates: I, limit: usize) -> Vec<Scored<&'a str>>
where
    I: IntoIterator<Item = &'a str>,
{
    let mut hits: Vec<Scored<&'a str>> = candidates
        .into_iter()
        .filter(|c| *c != typed && accepts(typed, c))
        .map(|c| Scored {
            item: c,
            score: jaro_winkler(typed, c),
        })
        .collect();
    // Total order: score descending, then name ascending. `partial_cmp` cannot
    // be `None` here — neither operand is NaN — but the fallback is written out
    // rather than unwrapped so a future NaN degrades to a stable order instead
    // of a panic in the editor's wasm module.
    hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(Ordering::Equal)
            .then_with(|| a.item.cmp(b.item))
    });
    hits.truncate(limit);
    hits
}

/// Whether the ranked list justifies a single high-confidence suggestion.
///
/// Design 07 §6.1: emit one `Confidence::Exact` fix when `top - second >= 0.08`,
/// otherwise up to three `Probable` ones. A sole candidate is unambiguous by
/// construction.
#[must_use]
pub fn is_decisive<T>(ranked: &[Scored<T>]) -> bool {
    match ranked {
        [] => false,
        [_] => true,
        [a, b, ..] => a.score - b.score >= DID_YOU_MEAN_MARGIN,
    }
}

// ---------------------------------------------------------------------------
// Subsequence scoring — the completion popup's third tier
// ---------------------------------------------------------------------------

/// Score of `needle` as a subsequence of `haystack`, or `None` when it is not
/// one. Higher is better.
///
/// A single greedy left-to-right pass, deliberately not fzf's backtracking
/// optimum: this runs over every offered candidate on the keystroke path, and
/// the ranking tiers above it (exact prefix, case-insensitive prefix) already
/// decide the cases where the difference would be visible. Bonuses follow the
/// same intuition as `nucleo-matcher`: a match at a word boundary is worth more
/// than one in the middle of a word, and a run of consecutive matches is worth
/// more than the same characters scattered.
#[must_use]
pub fn subsequence_score(needle: &str, haystack: &str) -> Option<i32> {
    if needle.is_empty() {
        return Some(0);
    }
    const MATCH: i32 = 16;
    const BOUNDARY_BONUS: i32 = 8;
    const CONSECUTIVE_BONUS: i32 = 8;
    const LEADING_PENALTY: i32 = 1;

    let hay: Vec<char> = haystack.chars().collect();
    let mut score = 0i32;
    let mut j = 0usize;
    let mut prev_matched = false;
    let mut first = true;

    for nc in needle.chars() {
        let target = nc.to_ascii_lowercase();
        let mut found = None;
        while j < hay.len() {
            let hc = hay[j];
            if hc.to_ascii_lowercase() == target {
                found = Some(j);
                break;
            }
            j += 1;
        }
        let pos = found?;
        score += MATCH;
        if first {
            // Distance from the start is a mild penalty, bounded so a long
            // name is not ruled out by its length alone.
            score -= (pos as i32).min(16) * LEADING_PENALTY;
            first = false;
        }
        if prev_matched && pos > 0 && hay.get(pos.wrapping_sub(1)).is_some() && pos == j {
            score += CONSECUTIVE_BONUS;
        }
        let boundary = pos == 0
            || hay
                .get(pos - 1)
                .is_some_and(|p| *p == '_' || *p == '.' || p.is_ascii_digit())
            || (hay[pos].is_ascii_uppercase()
                && hay.get(pos - 1).is_some_and(char::is_ascii_lowercase));
        if boundary {
            score += BOUNDARY_BONUS;
        }
        prev_matched = true;
        j = pos + 1;
    }
    Some(score)
}

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

/// How a candidate path relates to the one the user wrote.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PathMatch {
    /// Same bytes except for ASCII case. Design 07 §6.1's case-sensitivity
    /// trap: a do-file authored on macOS, where the filesystem folded the case,
    /// failing on Linux. Reported as its own kind because the fix is exact and
    /// the explanation is specific.
    CaseOnly,
    /// The basenames agree exactly; the directory differs.
    SameBasename,
    /// The basenames are similar enough to suggest.
    FuzzyBasename,
}

/// Classify `candidate` against the path the user wrote, or `None` when it is
/// not worth offering.
///
/// Only the basename is fuzzed. A directory typo produces an unusable
/// suggestion far more often than a helpful one, and the r(601) card already
/// says which directory was searched.
#[must_use]
pub fn path_fuzz(written: &str, candidate: &str) -> Option<PathMatch> {
    let wb = basename(written);
    let cb = basename(candidate);
    if wb.is_empty() || cb.is_empty() {
        return None;
    }
    if written != candidate && written.eq_ignore_ascii_case(candidate) {
        return Some(PathMatch::CaseOnly);
    }
    if wb == cb {
        return Some(PathMatch::SameBasename);
    }
    if wb.eq_ignore_ascii_case(cb) {
        return Some(PathMatch::CaseOnly);
    }
    if accepts(wb, cb) {
        return Some(PathMatch::FuzzyBasename);
    }
    None
}

/// The final `/`- or `\`-separated component. Both separators, because a
/// do-file written on Windows is read on macOS.
#[must_use]
pub fn basename(path: &str) -> &str {
    match path.rfind(['/', '\\']) {
        Some(i) => path.get(i + 1..).unwrap_or(""),
        None => path,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;

    /// Winkler's own worked examples, to four decimal places. If this test
    /// moves, every §6.1 threshold has moved with it.
    #[test]
    fn jaro_winkler_matches_the_published_worked_examples() {
        let close = |a: f64, b: f64| (a - b).abs() < 1e-4;
        assert!(
            close(jaro("MARTHA", "MARHTA"), 0.944_444),
            "{}",
            jaro("MARTHA", "MARHTA")
        );
        assert!(close(jaro_winkler("MARTHA", "MARHTA"), 0.961_111));
        assert!(close(jaro("DWAYNE", "DUANE"), 0.822_222));
        assert!(close(jaro_winkler("DWAYNE", "DUANE"), 0.840_000));
        assert!(close(jaro("DIXON", "DICKSONX"), 0.766_666));
        assert!(close(jaro_winkler("DIXON", "DICKSONX"), 0.813_333));
    }

    #[test]
    fn jaro_winkler_is_reflexive_and_bounded() {
        for s in ["", "a", "income", "log_wage_2019"] {
            assert!((jaro_winkler(s, s) - 1.0).abs() < 1e-12, "{s}");
        }
        assert_eq!(jaro_winkler("abc", ""), 0.0);
        assert!(jaro_winkler("investment_income", "aaaaaaaaaaaaaaaaa") <= 1.0);
    }

    #[test]
    fn damerau_levenshtein_counts_a_non_adjacent_transposition() {
        // The case that separates the unrestricted metric from optimal string
        // alignment: OSA says 3, the true edit script is 2.
        assert_eq!(damerau_levenshtein("ca", "abc"), 2);
        assert_eq!(damerau_levenshtein("", "abc"), 3);
        assert_eq!(damerau_levenshtein("abc", "abc"), 0);
        assert_eq!(damerau_levenshtein("income", "incmoe"), 1);
        assert_eq!(damerau_levenshtein("income", "incom"), 1);
    }

    #[test]
    fn the_spec_headline_case_is_accepted_and_decisive() {
        // spec §21's own example, and the golden's own typo:
        // `summarize incom` -> r(111) "variable incom not found".
        let varlist = [
            "make",
            "price",
            "mpg",
            "rep78",
            "headroom",
            "trunk",
            "weight",
            "length",
            "turn",
            "displacement",
            "gear_ratio",
            "foreign",
            "income",
        ];
        let ranked = rank("incom", varlist, 3);
        assert_eq!(ranked.first().map(|s| s.item), Some("income"));
        assert!(is_decisive(&ranked), "{ranked:?}");
    }

    #[test]
    fn nothing_is_suggested_for_a_name_with_no_neighbour() {
        let varlist = ["make", "price", "mpg", "foreign"];
        assert!(rank("nosuchvar", varlist, 3).is_empty());
    }

    #[test]
    fn a_candidate_never_suggests_itself() {
        assert!(rank("price", ["price", "prices"], 3)
            .iter()
            .all(|s| s.item != "price"));
    }

    #[test]
    fn subsequence_prefers_word_boundaries() {
        let tight = subsequence_score("lw", "log_wage");
        let loose = subsequence_score("lw", "flowchart");
        assert!(tight > loose, "{tight:?} vs {loose:?}");
        assert_eq!(subsequence_score("zq", "log_wage"), None);
        assert_eq!(subsequence_score("", "anything"), Some(0));
    }

    #[test]
    fn path_fuzz_separates_the_case_trap_from_a_typo() {
        assert_eq!(
            path_fuzz("data/Wave2020.dta", "data/wave2020.dta"),
            Some(PathMatch::CaseOnly)
        );
        assert_eq!(
            path_fuzz("wave2020.dta", "raw/wave2020.dta"),
            Some(PathMatch::SameBasename)
        );
        assert_eq!(
            path_fuzz("data/wave202.dta", "data/wave2020.dta"),
            Some(PathMatch::FuzzyBasename)
        );
        assert_eq!(path_fuzz("data/a.dta", "data/zzzzzzzz.dta"), None);
        assert_eq!(basename("C:\\proj\\a.dta"), "a.dta");
        assert_eq!(basename("a.dta"), "a.dta");
    }

    #[test]
    fn ranking_is_a_total_order_under_ties() {
        // Two candidates equidistant from the typed token must come back in
        // lexicographic order, every time, or the popup moves under the finger.
        let a = rank("xy", ["xa", "xb", "xc"], 3);
        let b = rank("xy", ["xc", "xb", "xa"], 3);
        assert_eq!(
            a.iter().map(|s| s.item).collect::<Vec<_>>(),
            b.iter().map(|s| s.item).collect::<Vec<_>>()
        );
    }
}
