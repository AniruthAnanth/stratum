//! Byte-exactness of the classic tables, at `linesize 80`.
//!
//! Every `.txt` under `tests/golden/` was cut out of
//! `tests/golden/stata18/{core,extended}_surface.log` — the capture from the
//! licensed StataMP 18.5, which can no longer be regenerated. A failure here
//! means this crate is wrong; the golden is not negotiable.
//!
//! The assertions are deliberately layered:
//!
//! 1. the whole block, byte for byte;
//! 2. F13's whitespace law, which is the property a hand-tuned layout breaks
//!    first and which a whole-file compare states only implicitly;
//! 3. A6 — every `display*` string in the card payload is a substring of the
//!    classic text, so the card and the Classic pane cannot disagree about a
//!    digit.

mod common;

use common::{auto, cases, golden, local, Case};
use pretty_assertions::assert_eq;
use stratum_proto::result::{Cell, ResultPayload};
use stratum_proto::styled::to_plain;
use stratum_stats::{
    classic_plain, regress, RegressSpec, StatsError, SummarizeSpec, VarRef, LINESIZE,
};

/// The whole golden matrix, byte for byte.
#[test]
fn classic_text_is_byte_exact() {
    let mut checked = 0usize;
    for c in cases() {
        let got = to_plain(&c.runs);
        assert_eq!(
            got,
            golden(&format!("{}.txt", c.name)),
            "`{}` ({}) does not reproduce tests/golden/{}.txt",
            c.cmdline,
            c.name,
            c.name
        );
        checked += 1;
    }
    // A dropped case would otherwise turn this file green by doing nothing.
    assert_eq!(checked, 26, "the golden matrix lost or gained a case");
}

/// F13, as a law rather than as a side effect of 25 file compares.
///
/// Every classic table is emitted with no trailing whitespace and a fixed
/// width — except two-way `tabulate` data rows and `pwcorr`'s correlation
/// rows, which carry exactly one trailing space. Those bytes are the single
/// most easily "cleaned up" thing in the crate, so the exception is asserted
/// from both directions: nowhere else may have one, and neither may have two.
#[test]
fn trailing_whitespace_law() {
    for c in cases() {
        let text = to_plain(&c.runs);
        // The two exceptions `05` records: two-way `tabulate` data rows (F13)
        // and `pwcorr`'s correlation rows (§11, "note that `pwcorr`'s
        // correlation rows carry one trailing space while `correlate`'s do
        // not"). Both are visible in the committed goldens.
        let exempt = c.name.starts_with("tab2_") || c.name.starts_with("pwcorr");
        for (i, line) in text.lines().enumerate() {
            if line.ends_with(' ') {
                assert!(
                    exempt,
                    "{}: line {} has trailing whitespace: {line:?}",
                    c.name,
                    i + 1
                );
                assert!(
                    !line.ends_with("  "),
                    "{}: line {} has more than one trailing space: {line:?}",
                    c.name,
                    i + 1
                );
            }
        }
    }
}

/// The measured widths of `05` F13, at `linesize 80`.
///
/// Every case in the matrix is listed, so a layout cannot be added without a
/// measured width and the byte-exact compare being the only thing that saw it.
#[test]
fn table_widths() {
    // (case, the width of the widest line of that block).
    //
    // `regress` is 78 in every variant, `summarize` 71, `summarize, detail`
    // 61, one-way `tabulate` 48, `ttest` 78. Two-way `tabulate` is 79 rather
    // than 78 *because* its data rows carry one trailing space (F13), and
    // `pwcorr` is one wider than `correlate` for the same reason.
    // `correlate` is 14 + 9k: a 14-column stub and one 9-column value column
    // per variable, which for the three-variable goldens is 41.
    let k = 3usize;
    let expect: &[(&str, usize)] = &[
        ("summarize_all", 71),
        ("summarize_varlist", 71),
        ("summarize_mpg", 71),
        ("summarize_rep78", 71),
        ("summarize_predict", 71),
        ("summarize_detail_price", 61),
        ("tab1_rep78", 48),
        ("tab1_rep_missing", 48),
        ("tab2_chi2", 79),
        ("tab2_rowcol", 79),
        ("regress_ols", 78),
        ("regress_robust", 78),
        ("regress_cluster", 78),
        ("regress_collinear", 78),
        ("regress_noconstant", 78),
        ("regress_level90", 78),
        ("regress_beta", 78),
        ("regress_single", 78),
        ("regress_perfectfit", 78),
        ("correlate", 14 + 9 * k),
        ("correlate_cov", 14 + 9 * k),
        ("pwcorr_sig", 14 + 9 * k + 1),
        ("ttest_by", 78),
        ("ttest_unequal", 78),
        ("ttest_onesample", 78),
        ("ttest_paired", 78),
    ];
    let all = cases();
    assert_eq!(expect.len(), all.len(), "every case needs a measured width");
    for c in &all {
        let (_, w) = expect
            .iter()
            .find(|(n, _)| *n == c.name)
            .unwrap_or_else(|| panic!("no measured width for `{}`", c.name));
        let text = to_plain(&c.runs);
        let widest = text.lines().map(|l| l.chars().count()).max().unwrap_or(0);
        assert_eq!(
            widest, *w,
            "{}: widest line is {widest}, expected {w}",
            c.name
        );
        assert!(
            widest <= usize::from(LINESIZE),
            "{}: {widest} columns overflows linesize {LINESIZE}",
            c.name
        );
    }
}

/// A6: every pre-formatted `display` string in a payload appears verbatim in
/// the classic text of the same result.
///
/// This is the test that makes "the card and the classic text cannot disagree"
/// mechanical. A renderer that reformatted a number — `{:.4}` instead of
/// `fmt_g` — would still pass the byte-exact compare if the payload were built
/// from a second formatter; it cannot pass this.
#[test]
fn payload_display_strings_are_substrings_of_classic_text() {
    let mut asserted = 0usize;
    for c in cases() {
        let text = to_plain(&c.runs);
        for (what, s) in display_strings(&c) {
            let t = s.trim();
            if t.is_empty() {
                continue;
            }
            assert!(
                text.contains(t),
                "{}: payload {what} = {t:?} does not appear in the classic text",
                c.name
            );
            asserted += 1;
        }
    }
    // Guards against `display_strings` silently returning nothing.
    assert!(
        asserted > 400,
        "only {asserted} display strings were checked; the extractor is broken"
    );
}

/// Every pre-formatted string a payload carries, labelled for the failure
/// message.
fn display_strings(c: &Case) -> Vec<(String, String)> {
    let mut out = Vec::new();
    match &c.payload {
        ResultPayload::Summarize(s) => {
            for r in &s.rows {
                let d = &r.display;
                out.push((format!("{}.obs", r.var), d.obs.clone()));
                if r.obs == 0 {
                    // Stata prints the count alone for an all-missing variable.
                    continue;
                }
                out.push((format!("{}.mean", r.var), d.mean.clone()));
                out.push((format!("{}.sd", r.var), d.sd.clone()));
                out.push((format!("{}.min", r.var), d.min.clone()));
                out.push((format!("{}.max", r.var), d.max.clone()));
                if let Some(det) = &r.detail {
                    for (i, p) in det.display_percentiles.iter().enumerate() {
                        out.push((format!("{}.p{i}", r.var), p.clone()));
                    }
                    for (i, s) in det.display_stats.iter().enumerate() {
                        // [skewness, kurtosis, variance].
                        out.push((format!("{}.stat[{i}]", r.var), s.clone()));
                    }
                    for (i, s) in det.display_smallest4.iter().enumerate() {
                        out.push((format!("{}.smallest[{i}]", r.var), s.clone()));
                    }
                    for (i, s) in det.display_largest4.iter().enumerate() {
                        out.push((format!("{}.largest[{i}]", r.var), s.clone()));
                    }
                }
            }
        }
        ResultPayload::Estimation(e) => {
            // `regress, beta` replaces the two confidence-interval columns
            // with a single `Beta` column, so `display_num[4]` and `[5]` are
            // values the payload legitimately carries and the classic text
            // legitimately does not print. Every other column must appear.
            let ci_printed = !c.cmdline.contains("beta");
            for t in &e.terms {
                for (i, s) in t.display_num.iter().enumerate() {
                    if i >= 4 && !ci_printed {
                        continue;
                    }
                    // A perfect fit prints `.` in every inference cell; a bare
                    // dot would match almost any table.
                    if s.trim() == "." {
                        continue;
                    }
                    out.push((format!("{}.display_num[{i}]", t.name), s.clone()));
                }
            }
            if let Some(a) = &e.anova {
                for (i, s) in a.display.iter().enumerate() {
                    out.push((format!("anova.display[{i}]"), s.clone()));
                }
            }
        }
        ResultPayload::Table(t) => {
            for (i, cell) in t.cells.iter().enumerate() {
                if let Some(Cell::Num { display, .. }) = cell {
                    out.push((format!("cell[{i}]"), display.clone()));
                }
            }
        }
        ResultPayload::Tabulate(_) => {
            // `TabulatePayload` carries counts, not display strings: the
            // percentages are derived by the renderer from integers, so there
            // is nothing here that could drift from the classic text.
        }
        other => panic!("{}: unexpected payload {other:?}", c.name),
    }
    out
}

// ---------------------------------------------------------------------------
// Behaviour the goldens pin indirectly
// ---------------------------------------------------------------------------

/// The capitalisation of the standard-error column header is not a typo in the
/// goldens: StataMP 18.5 prints `Std. err.` under OLS and `std. err.` under
/// robust and cluster, because the robust table stacks a `Robust` banner above
/// it and the header becomes the second line of a two-line label. It is the
/// single most likely thing for a well-meaning cleanup to "fix", so it is
/// asserted by name as well as by the byte compare.
#[test]
fn the_std_err_header_case_follows_the_vce() {
    let by_name = |n: &str| {
        let c = cases()
            .into_iter()
            .find(|c| c.name == n)
            .unwrap_or_else(|| panic!("no case `{n}`"));
        to_plain(&c.runs)
    };
    for ols in ["regress_ols", "regress_collinear", "regress_single"] {
        let t = by_name(ols);
        assert!(t.contains("Coefficient  Std. err."), "{ols}: OLS header");
        assert!(!t.contains("std. err."), "{ols}: lowercase under OLS");
        assert!(!t.contains("Robust"), "{ols}: no Robust banner under OLS");
    }
    for rob in ["regress_robust", "regress_cluster"] {
        let t = by_name(rob);
        assert!(t.contains("Coefficient  std. err."), "{rob}: robust header");
        assert!(
            t.contains("               Robust\n"),
            "{rob}: Robust banner"
        );
    }
    // …and the cluster banner keeps the capital, because there it opens a
    // sentence rather than continuing a two-line column label.
    assert!(by_name("regress_cluster").contains("(Std. err. adjusted for 5 clusters in rep78)"));
}

/// `summarize` of a string variable is a row with `Obs 0`, not an error.
///
/// This is the `make` row of `summarize_all`, isolated so a regression names
/// itself instead of showing up as a one-line diff in a twelve-row table.
#[test]
fn string_variable_summarizes_as_zero_obs() {
    let a = auto();
    let r = stratum_stats::summarize(&[a.var("make")], &a.all(), &SummarizeSpec::default());
    assert_eq!(r.vars[0].n, 0);
    let text = classic_plain(&r, LINESIZE);
    assert!(
        text.contains("        make |          0\n"),
        "unexpected string-variable row:\n{text}"
    );
}

/// …but an estimation command refuses one, with `r(109)`.
#[test]
fn string_variable_in_a_model_is_r109() {
    let a = auto();
    let err = regress(
        &RegressSpec::new("regress make mpg", a.var("make"), vec![a.var("mpg")]),
        &a.all(),
    )
    .expect_err("a string depvar must not estimate");
    assert_eq!(err, StatsError::StringVariable("make".to_owned()));
    assert_eq!(err.rc(), 109);
}

/// An empty sample is `r(2000)`, not a table of zeros.
#[test]
fn empty_sample_is_r2000() {
    let a = auto();
    let empty = stratum_data::sample::Sample::range(a.nobs, 0, 0);
    let err = regress(
        &RegressSpec::new("regress price mpg", a.var("price"), vec![a.var("mpg")]),
        &empty,
    )
    .expect_err("an empty sample must not estimate");
    assert_eq!(err.rc(), 2000);
}

/// The collinearity note is emitted once per omitted column, in **varlist**
/// order rather than detection order.
///
/// The construction separates the two orders. `regress price mpg mpg2 weight
/// w2` with `mpg2 = mpg/2` and `w2 = weight/2`: F5/F6's dynamic
/// max-current-diagonal rule keeps the larger-magnitude column of each pair,
/// so `mpg` and `weight` survive. An omitted column's *residual* diagonal is
/// order eps times its raw one, so `w2` — whose raw scale is a thousand times
/// `mpg2`'s — is discovered first. Varlist order is the opposite, and that is
/// what the notes must print.
#[test]
fn collinearity_notes_follow_varlist_order() {
    let a = auto();
    let mpg2 = common::gen_float(&a.values("mpg"), |x| x / 2.0);
    let w2 = common::gen_float(&a.values("weight"), |x| x / 2.0);
    let r = regress(
        &RegressSpec::new(
            "regress price mpg mpg2 weight w2",
            a.var("price"),
            vec![
                a.var("mpg"),
                local("mpg2", &mpg2),
                a.var("weight"),
                local("w2", &w2),
            ],
        ),
        &a.all(),
    )
    .expect("regress");
    assert_eq!(r.omitted_names, vec!["mpg2".to_owned(), "w2".to_owned()]);
    let text = classic_plain(&r, LINESIZE);
    let notes: Vec<&str> = text.lines().filter(|l| l.starts_with("note:")).collect();
    assert_eq!(
        notes,
        vec![
            "note: mpg2 omitted because of collinearity.",
            "note: w2 omitted because of collinearity.",
        ]
    );
    assert_eq!(r.rank, 3, "mpg, weight and _cons survive");
}

/// F5: the omitted **set** does not depend on the order the regressors are
/// written in. `regress y a b` and `regress y b a` with `b = 3a` both drop `a`.
#[test]
fn collinearity_is_order_independent() {
    let a = auto();
    let triple = common::gen_float(&a.values("mpg"), |x| 3.0 * x);
    let fwd = regress(
        &RegressSpec::new(
            "regress price mpg mpg3",
            a.var("price"),
            vec![a.var("mpg"), local("mpg3", &triple)],
        ),
        &a.all(),
    )
    .expect("regress");
    let rev = regress(
        &RegressSpec::new(
            "regress price mpg3 mpg",
            a.var("price"),
            vec![local("mpg3", &triple), a.var("mpg")],
        ),
        &a.all(),
    )
    .expect("regress");
    assert_eq!(fwd.omitted_names, vec!["mpg".to_owned()]);
    assert_eq!(rev.omitted_names, vec!["mpg".to_owned()]);
    assert_eq!(fwd.rank, 2);
    assert_eq!(rev.rank, 2);
    // The surviving fit is the same model either way.
    assert!((fwd.rss - rev.rss).abs() <= 1e-12 * fwd.rss.abs());
}

/// F8: `_cons` is swept first and is never the omitted column, even when a
/// user variable is itself constant.
#[test]
fn a_constant_regressor_is_omitted_and_cons_is_not() {
    let a = auto();
    let mpg = a.values("mpg");
    let c1 = common::gen_float(&mpg, |_| 1.0);
    let r = regress(
        &RegressSpec::new(
            "regress price mpg c1",
            a.var("price"),
            vec![a.var("mpg"), local("c1", &c1)],
        ),
        &a.all(),
    )
    .expect("regress");
    assert_eq!(r.omitted_names, vec!["c1".to_owned()]);
    let cons = r.coefs.last().expect("_cons");
    assert_eq!(cons.name, "_cons");
    assert!(!cons.omitted, "_cons must never be the omitted column");
}

/// F15: the cluster variable participates in casewise deletion, so
/// `vce(cluster rep78)` estimates on 69 observations and not 74.
#[test]
fn cluster_variable_is_cased_out() {
    let a = auto();
    let plain: VarRef<'_> = a.var("price");
    let r = regress(
        &RegressSpec {
            vce: stratum_stats::VceSpec::Cluster(a.var("rep78")),
            ..RegressSpec::new(
                "regress price mpg weight, vce(cluster rep78)",
                plain,
                vec![a.var("mpg"), a.var("weight")],
            )
        },
        &a.all(),
    )
    .expect("regress");
    assert_eq!(r.n, 69);
    assert_eq!(r.sample.count(), 69);
    assert_eq!(r.df_r, 4.0, "df_r is G-1");
    assert_eq!(r.vce.n_clust, Some(5));
}

/// F3, with the exact-integrality rule the design demands: `j == j.floor()`,
/// no epsilon.
#[test]
fn percentile_rule_is_the_averaged_order_statistic() {
    let x10: Vec<f64> = (1..=10).map(f64::from).collect();
    assert_eq!(stratum_stats::summarize::percentile(&x10, 10), 1.5);
    assert_eq!(stratum_stats::summarize::percentile(&x10, 25), 3.0);
    assert_eq!(stratum_stats::summarize::percentile(&x10, 50), 5.5);
    assert_eq!(stratum_stats::summarize::percentile(&x10, 75), 8.0);
    assert_eq!(stratum_stats::summarize::percentile(&x10, 90), 9.5);

    let x5: Vec<f64> = (1..=5).map(f64::from).collect();
    assert_eq!(stratum_stats::summarize::percentile(&x5, 25), 2.0);
    assert_eq!(stratum_stats::summarize::percentile(&x5, 50), 3.0);
}
