//! A12: `classic_text` returns `Vec<StyledRun>`, and a style regression must be
//! exactly as loud as a spacing regression.
//!
//! Three assertions, in increasing strength:
//!
//! 1. **`to_plain(runs)` is byte-identical to the `.txt` golden.** Styling can
//!    therefore never move a byte — that is the whole reason the byte-exactness
//!    contract survived the change from `String` to runs.
//! 2. **The run boundaries match the committed `.runs.json`.** These files pin
//!    the boundaries themselves, so widening `Result` to swallow a label, or
//!    losing it entirely, fails here.
//! 3. **Structural laws the goldens cannot state.** `.runs.json` is generated
//!    from this crate under `STRATUM_BLESS=1`, so on its own it only proves the
//!    renderer is stable, not that it is right. The laws below are what make it
//!    a real check: a `Result` run holds a formatted number and nothing else —
//!    no padding, no `|`, no rule, no header word — and the count of `Result`
//!    runs per layout equals the number of computed values that layout prints,
//!    counted by hand from the table shape.

mod common;

use common::{bless, blessing, cases, golden, golden_path};
use pretty_assertions::assert_eq;
use stratum_proto::result::{StyleId, StyledRun};
use stratum_proto::styled::to_plain;

/// The serialized form of a run boundary. `StyledRun` itself would serialize
/// the whole text again, doubling every golden and making a whitespace-only
/// diff unreadable; the boundary is `(style, length in characters)` and the
/// text is already pinned by the `.txt` golden.
#[derive(serde::Serialize, serde::Deserialize, PartialEq, Eq, Debug)]
struct Boundary {
    style: String,
    /// Characters, not bytes: the tables are ASCII today, and a column count is
    /// what a reader checks against a ruler.
    chars: usize,
}

fn boundaries(runs: &[StyledRun]) -> Vec<Boundary> {
    runs.iter()
        .map(|r| Boundary {
            style: match r.style {
                StyleId::Text => "text",
                StyleId::Result => "result",
                other => panic!("a classic table emitted {other:?}; only text and result are used"),
            }
            .to_owned(),
            chars: r.text.chars().count(),
        })
        .collect()
}

/// (1) and (2), over every layout.
#[test]
fn runs_flatten_to_the_golden_and_match_the_committed_boundaries() {
    for c in cases() {
        let txt = format!("{}.txt", c.name);
        assert_eq!(
            to_plain(&c.runs),
            golden(&txt),
            "{}: to_plain(runs) is not the committed classic text",
            c.name
        );

        let got = boundaries(&c.runs);
        let name = format!("{}.runs.json", c.name);
        if blessing() {
            bless(
                &name,
                &(serde_json::to_string_pretty(&got).expect("serialize boundaries") + "\n"),
            );
            continue;
        }
        let raw = std::fs::read_to_string(golden_path(&name))
            .unwrap_or_else(|e| panic!("read tests/golden/{name}: {e} (run with STRATUM_BLESS=1)"));
        let want: Vec<Boundary> = serde_json::from_str(&raw).expect("parse boundaries");
        assert_eq!(got, want, "{}: run boundaries moved", c.name);
    }
}

/// (3a) A `Result` run is a number and nothing else.
///
/// `Runs::res_r` pads with `Text` and then writes the formatted value, so a
/// `Result` run can never contain a space. Everything a Stata numeric format
/// can produce is here: digits, sign, point, comma group separator, the `e`
/// of scientific notation, and the lone `.` of a missing value.
#[test]
fn result_runs_hold_only_formatted_numbers() {
    for c in cases() {
        for r in &c.runs {
            if r.style != StyleId::Result {
                continue;
            }
            assert!(
                r.text
                    .chars()
                    .all(|ch| ch.is_ascii_digit() || matches!(ch, '.' | '-' | '+' | ',' | 'e')),
                "{}: Result run {:?} is not a formatted number",
                c.name,
                r.text
            );
            assert!(
                !r.text.is_empty(),
                "{}: an empty Result run was emitted",
                c.name
            );
        }
    }
}

/// (3b) The number of `Result` runs per layout, counted by hand off the table.
///
/// This is what stops the `.runs.json` goldens from being a tautology: if the
/// renderer stopped marking result values entirely, (1) and (2) could both be
/// re-blessed green, and this could not.
#[test]
fn result_run_counts_match_the_table_shapes() {
    // Each entry: (case, expected count, the arithmetic).
    let expect: &[(&str, usize, &str)] = &[
        // 12 rows; `make` prints Obs alone, the other 11 print Obs/Mean/SD/Min/Max.
        ("summarize_all", 1 + 11 * 5, "1 + 11*5"),
        ("summarize_varlist", 3 * 5, "3 vars * 5 stats"),
        ("summarize_mpg", 5, "1 var * 5 stats"),
        ("summarize_rep78", 5, "1 var * 5 stats"),
        ("summarize_predict", 3 * 5, "3 vars * 5 stats"),
        // 9 percentiles + 4 smallest + 4 largest + Obs + Sum of wgt. + Mean +
        // Std. dev. + Variance + Skewness + Kurtosis.
        ("summarize_detail_price", 9 + 4 + 4 + 7, "9+4+4+7"),
        // 5 levels * (Freq, Percent, Cum.) + Total's (Freq, Percent).
        ("tab1_rep78", 5 * 3 + 2, "5*3 + 2"),
        ("tab1_rep_missing", 2 * 3 + 2, "2*3 + 2"),
        // 2 rows * (5 cells + row total) + total row (5 cells + grand total)
        // + chi2 statistic + its p.
        ("tab2_chi2", 2 * 6 + 6 + 2, "2*6 + 6 + 2"),
        // 3 stats * (2 rows * 6 + 1 total row * 6) = 3 * 18.
        ("tab2_rowcol", 3 * 18, "3 stats * 18 cells"),
        // ANOVA 9 + header 6 (N, F, Prob>F, R2, adjR2, RootMSE)
        // + 4 coefficient rows * 6 columns.
        ("regress_ols", 9 + 6 + 4 * 6, "9 + 6 + 4*6"),
        ("regress_single", 9 + 6 + 2 * 6, "9 + 6 + 2*6"),
        // Perfect fit: F and Prob>F print as `.`, and each coefficient row has
        // b plus five `.` cells. The dots are still results.
        ("regress_perfectfit", 9 + 6 + 2 * 6, "9 + 6 + 2*6"),
        // Robust: no ANOVA block, 5 header rows, 3 coefficient rows.
        ("regress_robust", 5 + 3 * 6, "5 + 3*6"),
        // Cluster: the same shape. The `5` inside "(Std. err. adjusted for 5
        // clusters in rep78)" is deliberately NOT a result run — Stata prints
        // that whole banner in `{txt}`, and inking one digit inside a sentence
        // would be our invention rather than Stata's ink.
        ("regress_cluster", 5 + 3 * 6, "5 + 3*6"),
        // Collinear: the omitted row prints `0` and then `(omitted)`, so one
        // result cell instead of six.
        ("regress_collinear", 9 + 6 + (3 * 6 + 1), "9 + 6 + 3*6 + 1"),
        ("regress_noconstant", 9 + 6 + 2 * 6, "9 + 6 + 2*6"),
        ("regress_level90", 9 + 6 + 3 * 6, "9 + 6 + 3*6"),
        // `beta` replaces the two CI columns with one Beta column, and `_cons`
        // has no standardized coefficient, so its Beta cell is `.`.
        ("regress_beta", 9 + 6 + 3 * 5, "9 + 6 + 3*5"),
        // (obs=74) + the lower triangle of a 3x3.
        ("correlate", 1 + 6, "1 + 6"),
        ("correlate_cov", 1 + 6, "1 + 6"),
        // Lower triangle of a 3x3 (6 r's) + a p under each off-diagonal (3).
        ("pwcorr_sig", 6 + 3, "6 + 3"),
        // 3 rows * 6 + diff row (mean, se, ci_lo, ci_hi) + t + df + 3 p's.
        ("ttest_by", 3 * 6 + 4 + 2 + 3, "3*6 + 4 + 2 + 3"),
        ("ttest_unequal", 3 * 6 + 4 + 2 + 3, "3*6 + 4 + 2 + 3"),
        // 1 row * 6 + t + df + 3 p's; `ttest x == #` prints no diff row, and
        // the `20` in "H0: mean = 20" is the user's hypothesis, not a computed
        // value, so it stays `Text`.
        ("ttest_onesample", 6 + 2 + 3, "6 + 2 + 3"),
        // 2 variable rows * 6 + a full diff row (6) + t + df + 3 p's.
        ("ttest_paired", 2 * 6 + 6 + 2 + 3, "2*6 + 6 + 2 + 3"),
    ];
    let all = cases();
    assert_eq!(expect.len(), all.len(), "every case needs a hand count");
    for c in &all {
        let (_, want, how) = expect
            .iter()
            .find(|(n, _, _)| *n == c.name)
            .unwrap_or_else(|| panic!("no hand count for `{}`", c.name));
        let got = c.runs.iter().filter(|r| r.style == StyleId::Result).count();
        assert_eq!(got, *want, "{}: expected {how} Result runs", c.name);
    }
}

/// Adjacent runs of the same style are merged.
///
/// Without merging the goldens would encode the renderer's call sequence
/// rather than its visible structure, and a refactor that split one `txt` call
/// into two would show up as a style regression.
#[test]
fn adjacent_runs_never_share_a_style() {
    for c in cases() {
        for w in c.runs.windows(2) {
            assert_ne!(
                w[0].style, w[1].style,
                "{}: unmerged adjacent runs {:?} / {:?}",
                c.name, w[0].text, w[1].text
            );
        }
    }
}
