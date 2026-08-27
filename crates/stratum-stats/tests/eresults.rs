//! `e()` and `r()` — the names, the **insertion order**, and the values.
//!
//! `ereturn list` and `return list` print in insertion order, and both listings
//! are part of the byte-exact output surface: `core_surface.log` and
//! `extended_surface.log` contain them verbatim. So the order here is output,
//! not bookkeeping, and this file asserts it against those captures rather than
//! against a transcription of them — the logs are the authority and cannot be
//! regenerated, so the test reads them directly.
//!
//! Tolerances follow `05` §17.3. Counts and degrees of freedom are compared
//! exactly; everything else gets the relative tolerance its row of that table
//! prescribes. Nothing here is compared with `==` on a computed double.

mod common;

use common::{auto, cases};
use stratum_stats::stored::{ResultKind, ResultSet};
use stratum_stats::{regress, RegressSpec, StatResult, SummarizeSpec, VceSpec};

// ---------------------------------------------------------------------------
// Reading the Stata capture
// ---------------------------------------------------------------------------

/// One `ereturn list` / `return list` block, in printed order.
#[derive(Default, Debug)]
struct Listing {
    scalars: Vec<(String, f64)>,
    macros: Vec<(String, String)>,
    /// `(name, rows, cols)`.
    matrices: Vec<(String, usize, usize)>,
    functions: Vec<String>,
}

fn log(name: &str) -> String {
    // `tests/golden/stata18/` is W23's; this only ever reads it.
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/golden/stata18")
        .join(name);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// The `n`-th (0-based) `ereturn list` or `return list` block of `text`.
fn listing(text: &str, cmd: &str, n: usize) -> Listing {
    let want = format!(". {cmd}");
    let mut seen = 0usize;
    let mut lines = text.lines();
    while let Some(l) = lines.next() {
        if l.trim_end() != want {
            continue;
        }
        if seen < n {
            seen += 1;
            continue;
        }
        let mut out = Listing::default();
        let mut section = "";
        for l in lines.by_ref() {
            if l.starts_with(". ") {
                break;
            }
            let t = l.trim();
            if t.is_empty() {
                continue;
            }
            if let Some(s) = t.strip_suffix(':') {
                section = match s {
                    "scalars" => "s",
                    "macros" => "m",
                    "matrices" => "x",
                    "functions" => "f",
                    other => panic!("unknown {cmd} section `{other}`"),
                };
                continue;
            }
            // `e(N) =  74` / `e(cmdline) : "…"` / `e(b) :  1 x 4` / `e(sample)`.
            let (key, rest) = match (t.find('='), t.find(':')) {
                (Some(i), None) => (&t[..i], t[i + 1..].trim()),
                (None, Some(i)) => (&t[..i], t[i + 1..].trim()),
                (Some(i), Some(j)) if i < j => (&t[..i], t[i + 1..].trim()),
                (_, Some(j)) => (&t[..j], t[j + 1..].trim()),
                (None, None) => (t, ""),
            };
            let name = key
                .trim()
                .trim_start_matches(['e', 'r'])
                .trim_start_matches('(')
                .trim_end_matches(')')
                .to_owned();
            match section {
                "s" => out.scalars.push((
                    name,
                    rest.parse().unwrap_or_else(|_| panic!("scalar {rest:?}")),
                )),
                "m" => out.macros.push((name, rest.trim_matches('"').to_owned())),
                "x" => {
                    let (r, c) = rest.split_once('x').expect("`R x C`");
                    out.matrices.push((
                        name,
                        r.trim().parse().expect("rows"),
                        c.trim().parse().expect("cols"),
                    ));
                }
                "f" => out.functions.push(name),
                _ => panic!("value outside a section: {t:?}"),
            }
        }
        return out;
    }
    panic!("no `{cmd}` listing #{n} in the capture");
}

/// `05` §17.3's tolerance table, keyed by result name.
fn tolerance(name: &str) -> f64 {
    match name {
        // Counts, degrees of freedom and ranks are integers: exact.
        "N" | "N_clust" | "N_1" | "N_2" | "df_m" | "df_r" | "rank" | "sum_w" | "r" | "c"
        | "level" => 0.0,
        // Moments.
        "mean" | "Var" | "sd" | "sum" | "min" | "max" | "skewness" | "kurtosis" => 1e-13,
        // p-values and tail probabilities.
        "p" | "p_l" | "p_u" => 1e-14,
        // Everything derived from the sweep.
        _ => 1e-12,
    }
}

fn close(name: &str, got: f64, want: f64) -> bool {
    let tol = tolerance(name);
    if tol == 0.0 {
        return got == want;
    }
    if got == want {
        return true;
    }
    let scale = want.abs().max(1.0);
    (got - want).abs() <= tol * scale
}

fn check_scalars(what: &str, got: &ResultSet, want: &[(String, f64)]) {
    let names: Vec<&str> = got.scalar_names();
    let expect: Vec<&str> = want.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, expect, "{what}: scalar insertion order");
    for (n, w) in want {
        let g = got.scalar(n).unwrap_or_else(|| panic!("{what}: no {n}"));
        assert!(
            close(n, g, *w),
            "{what}: {n} = {g:.17e}, Stata printed {w:.17e}"
        );
    }
}

// ---------------------------------------------------------------------------
// regress
// ---------------------------------------------------------------------------

/// `ereturn list` after `regress price mpg weight foreign`, against
/// `core_surface.log` line 300.
#[test]
fn ereturn_ols_matches_the_capture() {
    let a = auto();
    let r = regress(
        &RegressSpec::new(
            "regress price mpg weight foreign",
            a.var("price"),
            vec![a.var("mpg"), a.var("weight"), a.var("foreign")],
        ),
        &a.all(),
    )
    .expect("regress");
    let (kind, e) = r.results();
    assert_eq!(kind, ResultKind::EClass);

    let want = listing(&log("core_surface.log"), "ereturn list", 0);
    check_scalars("regress ols", &e, &want.scalars);

    let macros: Vec<&str> = e.macro_names();
    let expect: Vec<&str> = want.macros.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(macros, expect, "regress ols: macro insertion order");
    for (n, v) in &want.macros {
        assert_eq!(e.macro_(n), Some(v.as_str()), "regress ols: e({n})");
    }

    let mats: Vec<&str> = e.matrix_names();
    let expect: Vec<&str> = want.matrices.iter().map(|(n, _, _)| n.as_str()).collect();
    assert_eq!(mats, expect, "regress ols: matrix insertion order");
    for (n, rows, cols) in &want.matrices {
        let m = e.matrix(n).unwrap_or_else(|| panic!("no e({n})"));
        assert_eq!((m.rows, m.cols), (*rows, *cols), "e({n}) shape");
    }

    assert_eq!(e.function_names(), want.functions);
    let s = e.function("sample").expect("e(sample)");
    assert_eq!(s.count(), 74, "every observation is in the OLS sample");
    assert_eq!(s.len(), a.nobs);
}

/// `ereturn list` after `regress price mpg weight, robust`, against
/// `extended_surface.log` line 127. The robust listing is the one that proves
/// `e(vcetype)` lands after `e(estat_cmd)` and that `e(V_modelbased)` exists.
#[test]
fn ereturn_robust_matches_the_capture() {
    let a = auto();
    let r = regress(
        &RegressSpec {
            vce: VceSpec::Robust,
            ..RegressSpec::new(
                "regress price mpg weight, robust",
                a.var("price"),
                vec![a.var("mpg"), a.var("weight")],
            )
        },
        &a.all(),
    )
    .expect("regress");
    let (_, e) = r.results();

    let want = listing(&log("extended_surface.log"), "ereturn list", 0);
    check_scalars("regress robust", &e, &want.scalars);

    let macros: Vec<&str> = e.macro_names();
    let expect: Vec<&str> = want.macros.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(macros, expect, "regress robust: macro insertion order");
    for (n, v) in &want.macros {
        assert_eq!(e.macro_(n), Some(v.as_str()), "regress robust: e({n})");
    }

    let mats: Vec<&str> = e.matrix_names();
    let expect: Vec<&str> = want.matrices.iter().map(|(n, _, _)| n.as_str()).collect();
    assert_eq!(mats, expect, "regress robust: matrix insertion order");
    for (n, rows, cols) in &want.matrices {
        let m = e.matrix(n).unwrap_or_else(|| panic!("no e({n})"));
        assert_eq!((m.rows, m.cols), (*rows, *cols), "e({n}) shape");
    }
}

/// `05` §8.7: under `vce(cluster)` `e(N_clust)` is inserted **first**, and
/// `e(beta)` is not posted at all.
#[test]
fn cluster_inserts_n_clust_first_and_drops_beta() {
    let a = auto();
    let r = regress(
        &RegressSpec {
            vce: VceSpec::Cluster(a.var("rep78")),
            ..RegressSpec::new(
                "regress price mpg weight, vce(cluster rep78)",
                a.var("price"),
                vec![a.var("mpg"), a.var("weight")],
            )
        },
        &a.all(),
    )
    .expect("regress");
    let (_, e) = r.results();
    assert_eq!(
        e.scalar_names(),
        vec![
            "N_clust", "N", "df_m", "df_r", "F", "r2", "rmse", "mss", "rss", "r2_a", "ll", "ll_0",
            "rank"
        ]
    );
    assert_eq!(e.scalar("N_clust"), Some(5.0));
    assert_eq!(e.scalar("N"), Some(69.0));
    assert_eq!(e.scalar("df_r"), Some(4.0));
    assert_eq!(e.macro_("vcetype"), Some("Robust"));
    assert_eq!(e.macro_("clustvar"), Some("rep78"));
    assert!(e.matrix("beta").is_none(), "cluster posts no e(beta)");
    assert!(e.matrix("V_modelbased").is_some());
}

/// The `o.` stripe is what tells `_b[mpg2] == 0` apart from a coefficient that
/// genuinely estimated to zero.
#[test]
fn omitted_columns_carry_the_o_stripe() {
    let a = auto();
    let mpg = a.values("mpg");
    let mpg2 = common::gen_float(&mpg, |x| x);
    let r = regress(
        &RegressSpec::new(
            "regress price mpg mpg2 weight",
            a.var("price"),
            vec![a.var("mpg"), common::local("mpg2", &mpg2), a.var("weight")],
        ),
        &a.all(),
    )
    .expect("regress");
    let (_, e) = r.results();
    let b = e.matrix("b").expect("e(b)");
    assert_eq!(b.colnames, vec!["mpg", "mpg2", "weight", "_cons"]);
    assert_eq!(b.colstripe, vec!["", "o.", "", ""]);
    assert_eq!(b.data[1], 0.0, "an omitted coefficient is exactly zero");
    let v = e.matrix("V").expect("e(V)");
    for j in 0..v.cols {
        assert_eq!(v.get(1, j), 0.0, "e(V) row 1 is zeroed");
        assert_eq!(v.get(j, 1), 0.0, "e(V) column 1 is zeroed");
    }
}

// ---------------------------------------------------------------------------
// summarize
// ---------------------------------------------------------------------------

/// `return list` after `summarize mpg`, against `core_surface.log` line 132.
#[test]
fn return_list_after_summarize_matches_the_capture() {
    let a = auto();
    let r = stratum_stats::summarize(&[a.var("mpg")], &a.all(), &SummarizeSpec::default());
    let (kind, got) = r.results();
    assert_eq!(kind, ResultKind::RClass);
    let want = listing(&log("core_surface.log"), "return list", 0);
    check_scalars("summarize mpg", &got, &want.scalars);
    assert!(want.macros.is_empty() && want.matrices.is_empty());
}

/// `05` §7.5: `detail` is a **different** sequence, not an extension — the
/// moments move in front of `sum`, and `min`/`max` move behind it.
#[test]
fn summarize_detail_reorders_r() {
    let a = auto();
    let r = stratum_stats::summarize(
        &[a.var("price")],
        &a.all(),
        &SummarizeSpec {
            detail: true,
            meanonly: false,
        },
    );
    let (_, got) = r.results();
    assert_eq!(
        got.scalar_names(),
        vec![
            "N", "sum_w", "mean", "Var", "sd", "skewness", "kurtosis", "sum", "min", "max", "p1",
            "p5", "p10", "p25", "p50", "p75", "p90", "p95", "p99"
        ]
    );
    // Cross-checked against the classic table: `summarize price, detail` prints
    // Variance 8699526, Skewness 1.653434, Kurtosis 4.819188.
    assert!(close(
        "Var",
        got.scalar("Var").unwrap(),
        8_699_525.974_268_788
    ));
    assert!((got.scalar("skewness").unwrap() - 1.653_434).abs() < 5e-7);
    assert!((got.scalar("kurtosis").unwrap() - 4.819_188).abs() < 5e-7);
    assert_eq!(got.scalar("p50"), Some(5006.5));
}

/// Only the **last** variable's results survive in `r()`.
#[test]
fn summarize_leaves_only_the_last_variable() {
    let a = auto();
    let r = stratum_stats::summarize(
        &[a.var("price"), a.var("mpg"), a.var("weight")],
        &a.all(),
        &SummarizeSpec::default(),
    );
    let (_, got) = r.results();
    assert_eq!(got.scalar("N"), Some(74.0));
    assert!(close(
        "mean",
        got.scalar("mean").unwrap(),
        3_019.459_459_459_459_4
    ));
}

// ---------------------------------------------------------------------------
// tabulate / correlate / ttest
// ---------------------------------------------------------------------------

/// `05` §10.3: `r(N) r(r) r(c) r(chi2) r(p)`, with the verified values.
#[test]
fn twoway_tabulate_posts_the_chi2_set() {
    let a = auto();
    let t = stratum_stats::tabulate_twoway(
        &a.var("foreign"),
        &a.var("rep78"),
        &a.all(),
        &stratum_stats::TabOptions {
            chi2: true,
            ..stratum_stats::TabOptions::default()
        },
    )
    .expect("tabulate");
    let (kind, r) = t.results();
    assert_eq!(kind, ResultKind::RClass);
    assert_eq!(r.scalar_names(), vec!["N", "r", "c", "chi2", "p"]);
    assert_eq!(r.scalar("N"), Some(69.0));
    assert_eq!(r.scalar("r"), Some(2.0));
    assert_eq!(r.scalar("c"), Some(5.0));
    assert!(close(
        "chi2",
        r.scalar("chi2").unwrap(),
        27.263_961_038_961_04
    ));
    assert!(close("p", r.scalar("p").unwrap(), 1.757_960_842_66e-5));
}

/// `05` §11: `correlate` posts `r(N) r(rho)` and the matrix `r(C)`; `pwcorr`
/// posts the scalars and **no** matrix.
#[test]
fn correlate_posts_c_and_pwcorr_does_not() {
    let a = auto();
    let vars = [a.var("price"), a.var("mpg"), a.var("weight")];
    let (_, c) = stratum_stats::correlate(&vars, &a.all(), &stratum_stats::CorrOptions::default())
        .expect("correlate")
        .results();
    assert_eq!(c.scalar_names(), vec!["N", "rho"]);
    assert_eq!(c.matrix_names(), vec!["C"]);
    assert_eq!(c.scalar("N"), Some(74.0));
    // r(rho) is the last off-diagonal pair: corr(weight, mpg) = -0.8072.
    assert!((c.scalar("rho").unwrap() + 0.807_2).abs() < 5e-5);
    let m = c.matrix("C").expect("r(C)");
    assert_eq!((m.rows, m.cols), (3, 3));
    for i in 0..3 {
        assert!(close("C", m.get(i, i), 1.0), "r(C) diagonal");
        for j in 0..3 {
            assert!(close("C", m.get(i, j), m.get(j, i)), "r(C) symmetry");
        }
    }

    let (_, p) = stratum_stats::pwcorr(&vars, &a.all(), &stratum_stats::CorrOptions::default())
        .expect("pwcorr")
        .results();
    assert_eq!(p.scalar_names(), vec!["N", "rho"]);
    assert!(p.matrix_names().is_empty(), "18.5 posts no r(C) for pwcorr");
}

/// `05` §12.4's two printed orders, one-sample and two-sample.
#[test]
fn ttest_r_orders() {
    let a = auto();
    let (_, one) = stratum_stats::ttest(
        &stratum_stats::TTestSpec::OneSample {
            var: a.var("mpg"),
            mu0: 20.0,
        },
        &a.all(),
        95.0,
    )
    .expect("ttest")
    .results();
    assert_eq!(
        one.scalar_names(),
        vec!["level", "sd_1", "se", "p_u", "p_l", "p", "t", "df_t", "mu_1", "N_1"]
    );
    assert_eq!(one.scalar("level"), Some(95.0));
    assert_eq!(one.scalar("N_1"), Some(74.0));
    assert_eq!(one.scalar("df_t"), Some(73.0));

    let (_, two) = stratum_stats::ttest(
        &stratum_stats::TTestSpec::TwoSample {
            var: a.var("mpg"),
            by: a.var("foreign"),
            unequal: false,
        },
        &a.all(),
        95.0,
    )
    .expect("ttest")
    .results();
    assert_eq!(
        two.scalar_names(),
        vec![
            "level", "sd", "sd_2", "sd_1", "se", "p_u", "p_l", "p", "t", "df_t", "mu_2", "N_2",
            "mu_1", "N_1"
        ]
    );
    assert_eq!(two.scalar("N_1"), Some(52.0));
    assert_eq!(two.scalar("N_2"), Some(22.0));
    assert_eq!(two.scalar("df_t"), Some(72.0));
}

/// Every case posts a non-empty result set of the right class, and no name is
/// ever inserted twice.
#[test]
fn every_case_posts_a_well_formed_result_set() {
    for c in cases() {
        let names = c.results.scalar_names();
        assert!(!names.is_empty(), "{}: posts no scalars", c.name);
        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            names.len(),
            "{}: duplicate scalar name",
            c.name
        );
        let expect = if c.name.starts_with("regress_") {
            ResultKind::EClass
        } else {
            ResultKind::RClass
        };
        assert_eq!(c.kind, expect, "{}: wrong result class", c.name);
    }
}
