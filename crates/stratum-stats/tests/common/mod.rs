//! The `auto.dta` fixture every golden test in this crate is computed against,
//! plus the derived variables the golden do-files create.
//!
//! # Why a JSON fixture and not the `.dta`
//!
//! `tests/fixtures/dta/` belongs to W03 and reading it needs W03's reader,
//! which this crate does not depend on and must not (ARCHITECTURE §5 puts the
//! `.dta` codec above us, not below). `tests/golden/auto.json` is the same 74
//! observations of the same twelve variables in the same storage types, so the
//! doubles the estimators see are bit-identical to the ones Stata saw.
//!
//! # Storage types are load-bearing, not decoration
//!
//! `price` is `int`, `gear_ratio` is `float`, `foreign` is `byte`. Every
//! derived variable below is created as `float`, because that is what Stata's
//! `generate` and `predict` do when no type is named — and it is visible in the
//! goldens: `summarize xb_hat res stdp_hat` reports a residual mean of
//! `-6.82e-06`, which is float rounding on values of order 3000, not the ~1e-12
//! a double residual would give. Storing the predictions as `double` fails that
//! golden by six digits.

#![allow(dead_code)] // each test binary uses a different subset of this module.

use std::sync::OnceLock;

use stratum_core::missing::{BYTE_MISS, INT_MISS, SYSMISS_F32};
use stratum_data::column::{Column, NumCol};
use stratum_data::labels::ValueLabel;
use stratum_data::sample::Sample;
use stratum_proto::StorageType;
use stratum_stats::VarRef;

/// One variable of the fixture: storage plus the metadata `VarRef` carries.
pub struct Var {
    pub name: String,
    pub label: String,
    pub format: String,
    pub col: Column,
    pub value_label: Option<ValueLabel>,
}

/// The loaded fixture.
pub struct Auto {
    pub nobs: u64,
    pub vars: Vec<Var>,
}

impl Auto {
    /// Borrow a variable by name. Panics: a typo in a test name is a test bug,
    /// and a `Result` here would put a `?` on every line of every golden test.
    #[must_use]
    pub fn var(&self, name: &str) -> VarRef<'_> {
        let v = self
            .vars
            .iter()
            .find(|v| v.name == name)
            .unwrap_or_else(|| panic!("fixture has no variable `{name}`"));
        VarRef {
            name: &v.name,
            label: &v.label,
            format: &v.format,
            col: &v.col,
            value_label: v.value_label.as_ref(),
        }
    }

    /// The `f64` values of a variable in dataset order, missing values in
    /// Stata's sentinel encoding.
    #[must_use]
    pub fn values(&self, name: &str) -> Vec<f64> {
        let mut out = Vec::new();
        self.var(name).col.gather_f64(&self.all(), &mut out);
        out
    }

    /// `Sample::all` over the fixture.
    #[must_use]
    pub fn all(&self) -> Sample {
        Sample::all(self.nobs)
    }
}

/// The fixture, loaded once per test binary.
pub fn auto() -> &'static Auto {
    static AUTO: OnceLock<Auto> = OnceLock::new();
    AUTO.get_or_init(load)
}

fn load() -> Auto {
    let raw = std::fs::read_to_string(golden_path("auto.json")).expect("read auto.json");
    let j: serde_json::Value = serde_json::from_str(&raw).expect("parse auto.json");
    let nobs = j["n"].as_u64().expect("n");

    let mut vars = Vec::new();
    for v in j["vars"].as_array().expect("vars") {
        let name = v["name"].as_str().expect("name").to_owned();
        let ty = v["ty"].as_str().expect("ty");
        let data = v["data"].as_array().expect("data");
        assert_eq!(data.len() as u64, nobs, "{name}: wrong row count");
        let col = match ty {
            "byte" => Column::Byte(NumCol::from_slice(
                &data
                    .iter()
                    .map(|x| x.as_i64().map_or(BYTE_MISS, |n| n as i8))
                    .collect::<Vec<i8>>(),
            )),
            "int" => Column::Int(NumCol::from_slice(
                &data
                    .iter()
                    .map(|x| x.as_i64().map_or(INT_MISS, |n| n as i16))
                    .collect::<Vec<i16>>(),
            )),
            "long" => Column::Long(NumCol::from_slice(
                &data
                    .iter()
                    .map(|x| {
                        x.as_i64()
                            .map_or(stratum_core::missing::LONG_MISS, |n| n as i32)
                    })
                    .collect::<Vec<i32>>(),
            )),
            "float" => Column::Float(NumCol::from_slice(
                &data
                    .iter()
                    .map(|x| x.as_f64().map_or(SYSMISS_F32, |n| n as f32))
                    .collect::<Vec<f32>>(),
            )),
            "double" => Column::Double(NumCol::from_slice(
                &data
                    .iter()
                    .map(|x| x.as_f64().unwrap_or(stratum_core::missing::SYSMISS))
                    .collect::<Vec<f64>>(),
            )),
            s if s.starts_with("str") => {
                let w: u16 = s[3..].parse().expect("str width");
                str_column(data, w, nobs)
            }
            other => panic!("{name}: unsupported storage type `{other}`"),
        };
        let vl = v["value_label"].as_str().map(|tab| {
            let mut t = ValueLabel::new();
            for (k, text) in j["value_labels"][tab]
                .as_object()
                .unwrap_or_else(|| panic!("no value-label table `{tab}`"))
            {
                t.insert(
                    k.parse().expect("value-label key"),
                    text.as_str().expect("text").to_owned(),
                );
            }
            t
        });
        vars.push(Var {
            name,
            label: v["label"].as_str().unwrap_or_default().to_owned(),
            format: v["format"].as_str().unwrap_or_default().to_owned(),
            col,
            value_label: vl,
        });
    }
    Auto { nobs, vars }
}

/// A `strN` column. `Column::from_row_major` is the only public builder for
/// fixed-width text, so the fixture is packed into the row-major shape it
/// expects rather than a second, test-only encoder being invented.
fn str_column(data: &[serde_json::Value], width: u16, nobs: u64) -> Column {
    let w = width as usize;
    let mut buf = vec![0u8; w * nobs as usize];
    for (i, cell) in data.iter().enumerate() {
        let s = cell.as_str().unwrap_or("").as_bytes();
        let n = s.len().min(w);
        buf[i * w..i * w + n].copy_from_slice(&s[..n]);
    }
    Column::from_row_major(StorageType::Str { width }, &buf, w, 0, nobs)
}

// ---------------------------------------------------------------------------
// Derived variables — exactly what the golden do-files generate
// ---------------------------------------------------------------------------

/// A `float` variable built from a closure over the fixture, the way
/// `generate` does: no type named, so `float`, and missing propagates.
#[must_use]
pub fn gen_float(src: &[f64], f: impl Fn(f64) -> f64) -> Column {
    Column::Float(NumCol::from_slice(
        &src.iter()
            .map(|&x| {
                if stratum_core::is_missing(x) {
                    SYSMISS_F32
                } else {
                    f(x) as f32
                }
            })
            .collect::<Vec<f32>>(),
    ))
}

/// A `float` variable from an already-computed `f64` vector — `predict`'s
/// output, which is stored as `float` unless `double` was asked for.
#[must_use]
pub fn float_col(vals: &[f64]) -> Column {
    Column::Float(NumCol::from_slice(
        &vals
            .iter()
            .map(|&x| {
                if stratum_core::is_missing(x) {
                    SYSMISS_F32
                } else {
                    x as f32
                }
            })
            .collect::<Vec<f32>>(),
    ))
}

/// A borrow of a locally built column, with no label and the `%9.0g` format
/// `generate` gives a new float variable.
#[must_use]
pub fn local<'a>(name: &'a str, col: &'a Column) -> VarRef<'a> {
    VarRef {
        name,
        label: "",
        format: "%9.0g",
        col,
        value_label: None,
    }
}

// ---------------------------------------------------------------------------
// Golden files
// ---------------------------------------------------------------------------

/// Absolute path of a file under `tests/golden/`.
#[must_use]
pub fn golden_path(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name)
}

/// The committed golden text for `name` (e.g. `"regress_ols.txt"`).
#[must_use]
pub fn golden(name: &str) -> String {
    std::fs::read_to_string(golden_path(name))
        .unwrap_or_else(|e| panic!("read tests/golden/{name}: {e}"))
}

/// Write a golden. Only ever called under `STRATUM_BLESS=1`; see
/// `styled_runs.rs` and `determinism.rs` for what that does and does not mean.
pub fn bless(name: &str, body: &str) {
    std::fs::write(golden_path(name), body)
        .unwrap_or_else(|e| panic!("write tests/golden/{name}: {e}"));
}

/// True when the run is regenerating goldens rather than checking them.
#[must_use]
pub fn blessing() -> bool {
    std::env::var_os("STRATUM_BLESS").is_some_and(|v| v == "1")
}

// ---------------------------------------------------------------------------
// The golden matrix
// ---------------------------------------------------------------------------

use stratum_proto::result::{ResultPayload, StyledRun};
use stratum_stats::stored::{ResultKind, ResultSet};
use stratum_stats::{
    correlate, pwcorr, regress, summarize, tabulate_oneway, tabulate_twoway, ttest, CorrOptions,
    PredictKind, RegressSpec, StatResult, SummarizeSpec, TTestSpec, TabOptions, VceSpec, LINESIZE,
};

/// One command of the golden matrix, reduced to the three views `StatResult`
/// exposes. Every test binary in this crate walks the same list, so a layout
/// can never be byte-checked but style-unchecked, or hashed but never printed.
pub struct Case {
    /// Basename under `tests/golden/`, without an extension.
    pub name: &'static str,
    /// The command as the golden do-file wrote it. Provenance, and the string
    /// `e(cmdline)` carries for the `regress` cases.
    pub cmdline: &'static str,
    pub runs: Vec<StyledRun>,
    pub payload: ResultPayload,
    pub kind: ResultKind,
    pub results: ResultSet,
}

fn case(name: &'static str, cmdline: &'static str, r: &impl StatResult) -> Case {
    let (kind, results) = r.results();
    Case {
        name,
        cmdline,
        runs: r.classic_text(LINESIZE),
        payload: r.payload(),
        kind,
        results,
    }
}

/// Every layout the goldens cover, in the order the capture do-files run them.
///
/// Built eagerly into owned values: the estimators borrow columns that only
/// live inside this function (the derived `mpg2`, `exact` and `predict`
/// outputs), so returning the results themselves would need a self-referential
/// struct. The three views are all any test needs.
#[must_use]
#[allow(clippy::too_many_lines)] // one statement per golden; splitting it hides the matrix.
pub fn cases() -> Vec<Case> {
    let a = auto();
    let all = a.all();
    let mut out = Vec::new();

    // --- summarize ---------------------------------------------------------
    let every: Vec<_> = a.vars.iter().map(|v| a.var(&v.name)).collect();
    out.push(case(
        "summarize_all",
        "summarize",
        &summarize(&every, &all, &SummarizeSpec::default()),
    ));
    out.push(case(
        "summarize_varlist",
        "summarize price mpg weight",
        &summarize(
            &[a.var("price"), a.var("mpg"), a.var("weight")],
            &all,
            &SummarizeSpec::default(),
        ),
    ));
    out.push(case(
        "summarize_mpg",
        "summarize mpg",
        &summarize(&[a.var("mpg")], &all, &SummarizeSpec::default()),
    ));
    out.push(case(
        "summarize_rep78",
        "summarize rep78",
        &summarize(&[a.var("rep78")], &all, &SummarizeSpec::default()),
    ));
    out.push(case(
        "summarize_detail_price",
        "summarize price, detail",
        &summarize(
            &[a.var("price")],
            &all,
            &SummarizeSpec {
                detail: true,
                meanonly: false,
            },
        ),
    ));

    // --- tabulate ----------------------------------------------------------
    out.push(case(
        "tab1_rep78",
        "tabulate rep78",
        &tabulate_oneway(&a.var("rep78"), &all, &TabOptions::default()).expect("tab1 rep78"),
    ));
    // `gen rep_missing = missing(rep78)` — float, like every untyped generate.
    let rep78 = a.values("rep78");
    let rep_missing = Column::Float(NumCol::from_slice(
        &rep78
            .iter()
            .map(|&x| f32::from(u8::from(stratum_core::is_missing(x))))
            .collect::<Vec<f32>>(),
    ));
    out.push(case(
        "tab1_rep_missing",
        "tabulate rep_missing",
        &tabulate_oneway(
            &local("rep_missing", &rep_missing),
            &all,
            &TabOptions::default(),
        )
        .expect("tab1 rep_missing"),
    ));
    out.push(case(
        "tab2_chi2",
        "tabulate foreign rep78, chi2",
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
    ));
    out.push(case(
        "tab2_rowcol",
        "tabulate foreign rep78, row col",
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
    ));

    // --- regress -----------------------------------------------------------
    out.push(case(
        "regress_ols",
        "regress price mpg weight foreign",
        &regress(
            &RegressSpec::new(
                "regress price mpg weight foreign",
                a.var("price"),
                vec![a.var("mpg"), a.var("weight"), a.var("foreign")],
            ),
            &all,
        )
        .expect("regress ols"),
    ));
    out.push(case(
        "regress_single",
        "regress price mpg",
        &regress(
            &RegressSpec::new("regress price mpg", a.var("price"), vec![a.var("mpg")]),
            &all,
        )
        .expect("regress single"),
    ));
    out.push(case(
        "regress_robust",
        "regress price mpg weight, robust",
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
    ));
    out.push(case(
        "regress_cluster",
        "regress price mpg weight, vce(cluster rep78)",
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
    ));
    out.push(case(
        "regress_noconstant",
        "regress price mpg weight, noconstant",
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
    ));
    out.push(case(
        "regress_level90",
        "regress price mpg weight, level(90)",
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
    ));
    out.push(case(
        "regress_beta",
        "regress price mpg weight, beta",
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
    ));
    // `gen mpg2 = mpg` — an exact duplicate column, which must be omitted.
    let mpg = a.values("mpg");
    let mpg2 = gen_float(&mpg, |x| x);
    out.push(case(
        "regress_collinear",
        "regress price mpg mpg2 weight",
        &regress(
            &RegressSpec::new(
                "regress price mpg mpg2 weight",
                a.var("price"),
                vec![a.var("mpg"), local("mpg2", &mpg2), a.var("weight")],
            ),
            &all,
        )
        .expect("regress collinear"),
    ));
    // `gen exact = 2*mpg + 3` — a perfect fit, RSS exactly zero.
    let exact = gen_float(&mpg, |x| 2.0 * x + 3.0);
    out.push(case(
        "regress_perfectfit",
        "regress exact mpg",
        &regress(
            &RegressSpec::new(
                "regress exact mpg",
                local("exact", &exact),
                vec![a.var("mpg")],
            ),
            &all,
        )
        .expect("regress perfectfit"),
    ));

    // --- predict -----------------------------------------------------------
    // `quietly regress price mpg weight` then the three predict variants, each
    // stored as a float variable, then summarized.
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
    let xb = stratum_stats::predict(Some(&fit), &xs, None, PredictKind::Xb, a.nobs).expect("xb");
    let res = stratum_stats::predict(
        Some(&fit),
        &xs,
        Some(&price),
        PredictKind::Residuals,
        a.nobs,
    )
    .expect("residuals");
    let stdp =
        stratum_stats::predict(Some(&fit), &xs, None, PredictKind::Stdp, a.nobs).expect("stdp");
    let (xb_c, res_c, stdp_c) = (float_col(&xb), float_col(&res), float_col(&stdp));
    out.push(case(
        "summarize_predict",
        "summarize xb_hat res stdp_hat",
        &summarize(
            &[
                local("xb_hat", &xb_c),
                local("res", &res_c),
                local("stdp_hat", &stdp_c),
            ],
            &all,
            &SummarizeSpec::default(),
        ),
    ));

    // --- correlate ---------------------------------------------------------
    let corr_vars = [a.var("price"), a.var("mpg"), a.var("weight")];
    out.push(case(
        "correlate",
        "correlate price mpg weight",
        &correlate(&corr_vars, &all, &CorrOptions::default()).expect("correlate"),
    ));
    out.push(case(
        "correlate_cov",
        "correlate price mpg weight, covariance",
        &correlate(
            &corr_vars,
            &all,
            &CorrOptions {
                covariance: true,
                ..CorrOptions::default()
            },
        )
        .expect("correlate cov"),
    ));
    out.push(case(
        "pwcorr_sig",
        "pwcorr price mpg rep78, sig",
        &pwcorr(
            &[a.var("price"), a.var("mpg"), a.var("rep78")],
            &all,
            &CorrOptions {
                sig: true,
                ..CorrOptions::default()
            },
        )
        .expect("pwcorr sig"),
    ));

    // --- ttest -------------------------------------------------------------
    out.push(case(
        "ttest_by",
        "ttest mpg, by(foreign)",
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
    ));
    out.push(case(
        "ttest_unequal",
        "ttest mpg, by(foreign) unequal",
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
    ));
    out.push(case(
        "ttest_onesample",
        "ttest mpg == 20",
        &ttest(
            &TTestSpec::OneSample {
                var: a.var("mpg"),
                mu0: 20.0,
            },
            &all,
            95.0,
        )
        .expect("ttest onesample"),
    ));
    // `gen mpg2 = mpg + 2` — the paired case's second variable.
    let mpg_plus2 = gen_float(&mpg, |x| x + 2.0);
    out.push(case(
        "ttest_paired",
        "ttest mpg == mpg2",
        &ttest(
            &TTestSpec::Paired {
                x: a.var("mpg"),
                y: local("mpg2", &mpg_plus2),
            },
            &all,
            95.0,
        )
        .expect("ttest paired"),
    ));

    out
}
