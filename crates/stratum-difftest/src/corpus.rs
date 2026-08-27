//! The committed-corpus phase: Stata's side is `tests/golden/stata18/*.log`,
//! ours is regenerated on every run. No Stata required, ever.
//!
//! # Only Stata's output is committed
//!
//! The logs were captured from a licensed StataMP 18.5 whose licence has since
//! expired; they are irreplaceable, their banner licence lines are
//! `[redacted]`, and they are never edited. **Our side is never committed**:
//! [`regen`] recomputes it from `stratum-stats` on every invocation, so a
//! regression cannot be blessed into the repository by regenerating a golden
//! over it.
//!
//! # The seam W09 replaces
//!
//! When the engine edge lands in `stratum-cli`, `stratum run --capture`
//! produces the same classic text and the same capture records from the real
//! interpreter, and [`regen`] becomes a subprocess call instead of direct
//! stats-crate calls. Everything else in this module — the manifest, the
//! extraction, the comparison — is unchanged by that swap; that is why the
//! regeneration is one function.

use anyhow::{Context, Result};
use camino::Utf8Path;
use stratum_data::column::Column;
use stratum_stats::{
    classic_plain, correlate, pwcorr, regress, summarize, tabulate_oneway, tabulate_twoway, ttest,
    CorrOptions, PredictKind, RegressSpec, ResultKind, ResultSet, StatResult, SummarizeSpec,
    TTestSpec, TabOptions, VceSpec, LINESIZE,
};

use crate::compare::Report;
use crate::fixture::{auto, float_col, gen_float, local, Auto};
use crate::log;

/// Which committed log a case lives in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogFile {
    /// `tests/golden/stata18/core_surface.log`.
    Core,
    /// `tests/golden/stata18/extended_surface.log`.
    Extended,
}

impl LogFile {
    /// Path relative to the repo root.
    #[must_use]
    pub fn rel(self) -> &'static str {
        match self {
            LogFile::Core => "tests/golden/stata18/core_surface.log",
            LogFile::Extended => "tests/golden/stata18/extended_surface.log",
        }
    }
}

/// A `return list` / `ereturn list` block to compare against the regenerated
/// `ResultSet`.
#[derive(Clone, Copy, Debug)]
pub struct ListingRef {
    /// Which log the listing is in.
    pub log: LogFile,
    /// The echoed command: `"return list"` or `"ereturn list"`.
    pub echo: &'static str,
    /// 0-based occurrence of that echo within the log.
    pub occurrence: usize,
}

/// One corpus case: a command whose classic output is byte-compared against
/// the committed log, plus (for three anchor cases) the stored-results
/// listing that follows it in the capture.
#[derive(Clone, Copy, Debug)]
pub struct CorpusCase {
    /// Case name — also the directory name under `tests/difftest/cases/`.
    pub name: &'static str,
    /// Which log carries the echo.
    pub log: LogFile,
    /// The exact echoed command line (without the leading `. `).
    pub echo: &'static str,
    /// 0-based occurrence of the echo.
    pub occurrence: usize,
    /// The listing that captures this case's `r()`/`e()`, when one exists.
    pub listing: Option<ListingRef>,
}

const fn case(name: &'static str, log: LogFile, echo: &'static str) -> CorpusCase {
    CorpusCase {
        name,
        log,
        echo,
        occurrence: 0,
        listing: None,
    }
}

/// The corpus manifest: every command in the committed capture that the
/// current runtime surface can regenerate. 26 cases, same matrix as W05's
/// golden tests — asserted by count in the integration tests so a dropped
/// case cannot shrink the net silently.
#[must_use]
pub fn manifest() -> Vec<CorpusCase> {
    use LogFile::{Core, Extended};
    vec![
        case("summarize_all", Core, "summarize"),
        case("summarize_varlist", Core, "summarize price mpg weight"),
        CorpusCase {
            listing: Some(ListingRef {
                log: Core,
                echo: "return list",
                occurrence: 0,
            }),
            ..case("summarize_mpg", Core, "summarize mpg")
        },
        case("summarize_rep78", Core, "summarize rep78"),
        case("summarize_detail_price", Core, "summarize price, detail"),
        case("tab1_rep78", Core, "tabulate rep78"),
        case("tab1_rep_missing", Core, "tabulate rep_missing"),
        case("tab2_chi2", Core, "tabulate foreign rep78, chi2"),
        case("tab2_rowcol", Core, "tabulate foreign rep78, row col"),
        CorpusCase {
            listing: Some(ListingRef {
                log: Core,
                echo: "ereturn list",
                occurrence: 0,
            }),
            ..case("regress_ols", Core, "regress price mpg weight foreign")
        },
        case("regress_single", Extended, "regress price mpg"),
        CorpusCase {
            listing: Some(ListingRef {
                log: Extended,
                echo: "ereturn list",
                occurrence: 0,
            }),
            ..case("regress_robust", Core, "regress price mpg weight, robust")
        },
        case(
            "regress_cluster",
            Extended,
            "regress price mpg weight, vce(cluster rep78)",
        ),
        case(
            "regress_noconstant",
            Extended,
            "regress price mpg weight, noconstant",
        ),
        case(
            "regress_level90",
            Extended,
            "regress price mpg weight, level(90)",
        ),
        case("regress_beta", Extended, "regress price mpg weight, beta"),
        case("regress_collinear", Core, "regress price mpg mpg2 weight"),
        case(
            "regress_perfectfit",
            Extended,
            "capture noisily regress exact mpg",
        ),
        case(
            "summarize_predict",
            Extended,
            "summarize xb_hat res stdp_hat",
        ),
        case("correlate", Core, "correlate price mpg weight"),
        case(
            "correlate_cov",
            Extended,
            "correlate price mpg weight, covariance",
        ),
        case("pwcorr_sig", Core, "pwcorr price mpg rep78, sig"),
        case("ttest_by", Core, "ttest mpg, by(foreign)"),
        case("ttest_unequal", Extended, "ttest mpg, by(foreign) unequal"),
        case("ttest_onesample", Core, "ttest mpg == 20"),
        case("ttest_paired", Extended, "ttest mpg == mpg2"),
    ]
}

/// A regenerated case: the two views the comparator consumes.
pub struct Regen {
    /// Classic text at `linesize 80`, flattened.
    pub classic: String,
    /// Which stored-result class the command posts.
    pub kind: ResultKind,
    /// The regenerated `r()`/`e()`.
    pub results: ResultSet,
}

fn made(r: &impl StatResult) -> Regen {
    let (kind, results) = r.results();
    Regen {
        classic: classic_plain(r, LINESIZE),
        kind,
        results,
    }
}

/// Regenerate our side of `name`, fresh. This is the seam `stratum run
/// --capture` replaces once W09's engine edge lands (module header).
///
/// # Panics
/// On an unknown case name (the manifest and this dispatcher are asserted in
/// lockstep by the tests) or an estimator error on the fixture, which cannot
/// happen for the committed matrix.
#[must_use]
#[allow(clippy::too_many_lines)] // one arm per corpus case; splitting hides the matrix
pub fn regen(a: &Auto, name: &str) -> Regen {
    let all = a.all();
    match name {
        "summarize_all" => {
            let every: Vec<_> = a.vars.iter().map(|v| a.var(&v.name)).collect();
            made(&summarize(&every, &all, &SummarizeSpec::default()))
        }
        "summarize_varlist" => made(&summarize(
            &[a.var("price"), a.var("mpg"), a.var("weight")],
            &all,
            &SummarizeSpec::default(),
        )),
        "summarize_mpg" => made(&summarize(&[a.var("mpg")], &all, &SummarizeSpec::default())),
        "summarize_rep78" => made(&summarize(
            &[a.var("rep78")],
            &all,
            &SummarizeSpec::default(),
        )),
        "summarize_detail_price" => made(&summarize(
            &[a.var("price")],
            &all,
            &SummarizeSpec {
                detail: true,
                meanonly: false,
            },
        )),
        "tab1_rep78" => made(
            &tabulate_oneway(&a.var("rep78"), &all, &TabOptions::default()).expect("tab1 rep78"),
        ),
        "tab1_rep_missing" => {
            let rep78 = a.values("rep78");
            let rep_missing = Column::Float(stratum_data::column::NumCol::from_slice(
                &rep78
                    .iter()
                    .map(|&x| f32::from(u8::from(stratum_core::is_missing(x))))
                    .collect::<Vec<f32>>(),
            ));
            made(
                &tabulate_oneway(
                    &local("rep_missing", &rep_missing),
                    &all,
                    &TabOptions::default(),
                )
                .expect("tab1 rep_missing"),
            )
        }
        "tab2_chi2" => made(
            &tabulate_twoway(
                &a.var("foreign"),
                &a.var("rep78"),
                &all,
                &TabOptions {
                    chi2: true,
                    ..TabOptions::default()
                },
            )
            .expect("tab2 chi2"),
        ),
        "tab2_rowcol" => made(
            &tabulate_twoway(
                &a.var("foreign"),
                &a.var("rep78"),
                &all,
                &TabOptions {
                    row: true,
                    col: true,
                    ..TabOptions::default()
                },
            )
            .expect("tab2 rowcol"),
        ),
        "regress_ols" => made(
            &regress(
                &RegressSpec::new(
                    "regress price mpg weight foreign",
                    a.var("price"),
                    vec![a.var("mpg"), a.var("weight"), a.var("foreign")],
                ),
                &all,
            )
            .expect("regress ols"),
        ),
        "regress_single" => made(
            &regress(
                &RegressSpec::new("regress price mpg", a.var("price"), vec![a.var("mpg")]),
                &all,
            )
            .expect("regress single"),
        ),
        "regress_robust" => made(
            &regress(
                &RegressSpec {
                    vce: VceSpec::Robust,
                    ..RegressSpec::new(
                        "regress price mpg weight, robust",
                        a.var("price"),
                        vec![a.var("mpg"), a.var("weight")],
                    )
                },
                &all,
            )
            .expect("regress robust"),
        ),
        "regress_cluster" => made(
            &regress(
                &RegressSpec {
                    vce: VceSpec::Cluster(a.var("rep78")),
                    ..RegressSpec::new(
                        "regress price mpg weight, vce(cluster rep78)",
                        a.var("price"),
                        vec![a.var("mpg"), a.var("weight")],
                    )
                },
                &all,
            )
            .expect("regress cluster"),
        ),
        "regress_noconstant" => made(
            &regress(
                &RegressSpec {
                    noconstant: true,
                    ..RegressSpec::new(
                        "regress price mpg weight, noconstant",
                        a.var("price"),
                        vec![a.var("mpg"), a.var("weight")],
                    )
                },
                &all,
            )
            .expect("regress noconstant"),
        ),
        "regress_level90" => made(
            &regress(
                &RegressSpec {
                    level: 90.0,
                    ..RegressSpec::new(
                        "regress price mpg weight, level(90)",
                        a.var("price"),
                        vec![a.var("mpg"), a.var("weight")],
                    )
                },
                &all,
            )
            .expect("regress level90"),
        ),
        "regress_beta" => made(
            &regress(
                &RegressSpec {
                    beta: true,
                    ..RegressSpec::new(
                        "regress price mpg weight, beta",
                        a.var("price"),
                        vec![a.var("mpg"), a.var("weight")],
                    )
                },
                &all,
            )
            .expect("regress beta"),
        ),
        "regress_collinear" => {
            let mpg = a.values("mpg");
            let mpg2 = gen_float(&mpg, |x| x);
            made(
                &regress(
                    &RegressSpec::new(
                        "regress price mpg mpg2 weight",
                        a.var("price"),
                        vec![a.var("mpg"), local("mpg2", &mpg2), a.var("weight")],
                    ),
                    &all,
                )
                .expect("regress collinear"),
            )
        }
        "regress_perfectfit" => {
            let mpg = a.values("mpg");
            let exact = gen_float(&mpg, |x| 2.0 * x + 3.0);
            made(
                &regress(
                    &RegressSpec::new(
                        "regress exact mpg",
                        local("exact", &exact),
                        vec![a.var("mpg")],
                    ),
                    &all,
                )
                .expect("regress perfectfit"),
            )
        }
        "summarize_predict" => {
            let fit = regress(
                &RegressSpec::new(
                    "regress price mpg weight",
                    a.var("price"),
                    vec![a.var("mpg"), a.var("weight")],
                ),
                &all,
            )
            .expect("regress for predict");
            let xs = [a.var("mpg"), a.var("weight")];
            let price = a.var("price");
            let xb =
                stratum_stats::predict(Some(&fit), &xs, None, PredictKind::Xb, a.nobs).expect("xb");
            let res = stratum_stats::predict(
                Some(&fit),
                &xs,
                Some(&price),
                PredictKind::Residuals,
                a.nobs,
            )
            .expect("residuals");
            let stdp = stratum_stats::predict(Some(&fit), &xs, None, PredictKind::Stdp, a.nobs)
                .expect("stdp");
            let (xb_c, res_c, stdp_c) = (float_col(&xb), float_col(&res), float_col(&stdp));
            made(&summarize(
                &[
                    local("xb_hat", &xb_c),
                    local("res", &res_c),
                    local("stdp_hat", &stdp_c),
                ],
                &all,
                &SummarizeSpec::default(),
            ))
        }
        "correlate" => made(
            &correlate(
                &[a.var("price"), a.var("mpg"), a.var("weight")],
                &all,
                &CorrOptions::default(),
            )
            .expect("correlate"),
        ),
        "correlate_cov" => made(
            &correlate(
                &[a.var("price"), a.var("mpg"), a.var("weight")],
                &all,
                &CorrOptions {
                    covariance: true,
                    ..CorrOptions::default()
                },
            )
            .expect("correlate cov"),
        ),
        "pwcorr_sig" => made(
            &pwcorr(
                &[a.var("price"), a.var("mpg"), a.var("rep78")],
                &all,
                &CorrOptions {
                    sig: true,
                    ..CorrOptions::default()
                },
            )
            .expect("pwcorr sig"),
        ),
        "ttest_by" => made(
            &ttest(
                &TTestSpec::TwoSample {
                    var: a.var("mpg"),
                    by: a.var("foreign"),
                    unequal: false,
                },
                &all,
                95.0,
            )
            .expect("ttest by"),
        ),
        "ttest_unequal" => made(
            &ttest(
                &TTestSpec::TwoSample {
                    var: a.var("mpg"),
                    by: a.var("foreign"),
                    unequal: true,
                },
                &all,
                95.0,
            )
            .expect("ttest unequal"),
        ),
        "ttest_onesample" => made(
            &ttest(
                &TTestSpec::OneSample {
                    var: a.var("mpg"),
                    mu0: 20.0,
                },
                &all,
                95.0,
            )
            .expect("ttest onesample"),
        ),
        "ttest_paired" => {
            let mpg = a.values("mpg");
            let mpg_plus2 = gen_float(&mpg, |x| x + 2.0);
            made(
                &ttest(
                    &TTestSpec::Paired {
                        x: a.var("mpg"),
                        y: local("mpg2", &mpg_plus2),
                    },
                    &all,
                    95.0,
                )
                .expect("ttest paired"),
            )
        }
        other => panic!("no regeneration for corpus case `{other}`"),
    }
}

/// Deliberate self-sabotage for the negative test: prove the harness CAN
/// fail. `--selftest-perturb` flips one byte of one case's classic text and
/// nudges one scalar just past its tolerance; a green report under
/// perturbation means the comparator is broken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Perturb {
    /// Compare honestly.
    None,
    /// Sabotage `summarize_mpg`: one text byte, one scalar.
    Deliberate,
}

/// Run the corpus phase: extract each case's block from the committed log,
/// regenerate ours, compare byte-exact; for the anchor cases, also compare
/// the stored-results listing under the §17.3 tolerances.
///
/// # Errors
/// Environment problems only — a missing or unreadable log, an echo the
/// manifest names that the log does not contain. Mismatches are NOT errors;
/// they are the [`Report`]'s content.
pub fn run(root: &Utf8Path, perturb: Perturb) -> Result<Report> {
    let a = auto();
    let core = read_log(root, LogFile::Core)?;
    let extended = read_log(root, LogFile::Extended)?;
    let body_of = |f: LogFile| match f {
        LogFile::Core => log::body(&core),
        LogFile::Extended => log::body(&extended),
    };

    let mut report = Report::default();
    for c in manifest() {
        report.counters.cases += 1;
        let stata_block = log::command_output(body_of(c.log), c.echo, c.occurrence)
            .with_context(|| format!("{}: `. {}` not found in {}", c.name, c.echo, c.log.rel()))?;
        let mut ours = regen(a, c.name);

        if perturb == Perturb::Deliberate && c.name == "summarize_mpg" {
            sabotage(&mut ours);
        }

        report.text(c.name, &stata_block, &ours.classic);

        if let Some(l) = c.listing {
            let block =
                log::command_output(body_of(l.log), l.echo, l.occurrence).with_context(|| {
                    format!("{}: `. {}` #{} not found", c.name, l.echo, l.occurrence)
                })?;
            let listing =
                log::parse_listing(&block).map_err(|e| anyhow::anyhow!("{}: {}", c.name, e))?;
            compare_listing(&mut report, c.name, &listing, &ours);
        }
    }
    Ok(report)
}

/// The corpus phase's other half of the negative test: text and value both.
fn sabotage(ours: &mut Regen) {
    // One byte of classic text: turn the first digit found into a different
    // digit. The block is a real Stata table, so a digit always exists.
    let flipped: String = {
        let mut done = false;
        ours.classic
            .chars()
            .map(|ch| {
                if !done && ch.is_ascii_digit() {
                    done = true;
                    if ch == '9' {
                        '8'
                    } else {
                        char::from(ch as u8 + 1)
                    }
                } else {
                    ch
                }
            })
            .collect()
    };
    ours.classic = flipped;
    // One scalar, nudged past every class tolerance but well inside display
    // rounding — the exact bug class the capture channel exists to catch.
    let mean = ours
        .results
        .scalar("mean")
        .expect("summarize posts r(mean)");
    ours.results.push_scalar("mean", mean * (1.0 + 1e-9));
}

/// Compare a parsed `return list` / `ereturn list` against the regenerated
/// `ResultSet`: same names in the same order, values under §17.3, macros
/// exact, matrix shapes exact, function names exact.
fn compare_listing(report: &mut Report, case: &str, listing: &log::Listing, ours: &Regen) {
    report.counters.listings += 1;

    let want_order: Vec<&str> = listing.scalars.iter().map(|(n, _)| n.as_str()).collect();
    let got_order: Vec<&str> = ours.results.scalar_names();
    if want_order != got_order {
        report.mismatches.push(crate::compare::Mismatch {
            case: case.to_owned(),
            channel: "scalar",
            detail: format!("insertion order: Stata {want_order:?}, ours {got_order:?}"),
        });
    }
    for (name, printed) in &listing.scalars {
        match ours.results.scalar(name) {
            Some(v) => report.scalar(case, name, printed, v),
            None => report.mismatches.push(crate::compare::Mismatch {
                case: case.to_owned(),
                channel: "scalar",
                detail: format!("{name}: missing on our side"),
            }),
        }
    }

    for (name, text) in &listing.macros {
        report.macro_(case, name, text, ours.results.macro_(name));
    }
    let want_m: Vec<&str> = listing.macros.iter().map(|(n, _)| n.as_str()).collect();
    let got_m: Vec<&str> = ours.results.macro_names();
    if want_m != got_m {
        report.mismatches.push(crate::compare::Mismatch {
            case: case.to_owned(),
            channel: "macro",
            detail: format!("insertion order: Stata {want_m:?}, ours {got_m:?}"),
        });
    }

    for (name, rows, cols) in &listing.matrices {
        let shape = ours.results.matrix(name).map(|m| (m.rows, m.cols));
        report.matrix_shape(case, name, (*rows, *cols), shape);
    }

    report.functions(case, &listing.functions, &ours.results.function_names());
}

fn read_log(root: &Utf8Path, which: LogFile) -> Result<String> {
    let path = root.join(which.rel());
    std::fs::read_to_string(&path).with_context(|| format!("read {path}"))
}
