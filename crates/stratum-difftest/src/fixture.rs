//! The `auto.dta` fixture the regenerated side is computed over.
//!
//! This is the same 74 observations of the same twelve variables, in the same
//! storage types, that Stata saw when the corpus was captured — read from
//! `crates/stratum-stats/tests/golden/auto.json` (W05's committed fixture; we
//! only ever read it). Storage types are load-bearing: `price` is `int`,
//! `gear_ratio` is `float`, and every derived variable is created as `float`
//! because that is what an untyped `generate` does — visibly so in the
//! goldens, where float rounding appears in the sixth digit of a residual
//! mean.
//!
//! Duplicated from `stratum-stats/tests/common` rather than imported because
//! a crate's `tests/` tree is not a library: the loader here is ~100 lines
//! against public APIs, and W23 owning its own copy keeps R0 clean. When
//! W09's engine edge lands and `stratum run --capture` replaces the direct
//! stats calls, this module is deleted with `corpus::regen`.

use std::sync::OnceLock;

use camino::Utf8PathBuf;
use stratum_core::missing::{BYTE_MISS, INT_MISS, LONG_MISS, SYSMISS, SYSMISS_F32};
use stratum_data::column::{Column, NumCol};
use stratum_data::labels::ValueLabel;
use stratum_data::sample::Sample;
use stratum_proto::StorageType;
use stratum_stats::VarRef;

/// One variable of the fixture: storage plus the metadata `VarRef` carries.
pub struct Var {
    /// Variable name.
    pub name: String,
    /// Variable label.
    pub label: String,
    /// Display format.
    pub format: String,
    /// The column data.
    pub col: Column,
    /// Attached value label, when any.
    pub value_label: Option<ValueLabel>,
}

/// The loaded fixture.
pub struct Auto {
    /// Observation count (74).
    pub nobs: u64,
    /// The twelve variables, in dataset order.
    pub vars: Vec<Var>,
}

impl Auto {
    /// Borrow a variable by name. Panics on a typo: this is harness-internal
    /// plumbing and a `Result` would put a `?` on every line of the matrix.
    ///
    /// # Panics
    /// When no variable has that name.
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

/// The fixture, loaded once per process.
///
/// # Panics
/// When the committed fixture is missing or malformed — an environment error
/// the caller cannot mend.
pub fn auto() -> &'static Auto {
    static AUTO: OnceLock<Auto> = OnceLock::new();
    AUTO.get_or_init(load)
}

/// `crates/stratum-stats/tests/golden/auto.json`, relative to the repo root.
fn fixture_path() -> Utf8PathBuf {
    crate::repo_root().join("crates/stratum-stats/tests/golden/auto.json")
}

#[allow(clippy::cast_possible_truncation)] // storage narrowing mirrors the .dta types
fn load() -> Auto {
    let path = fixture_path();
    let raw = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"));
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
                    .map(|x| x.as_i64().map_or(LONG_MISS, |n| n as i32))
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
                    .map(|x| x.as_f64().unwrap_or(SYSMISS))
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

/// A `strN` column, packed into the row-major shape `Column::from_row_major`
/// expects.
fn str_column(data: &[serde_json::Value], width: u16, nobs: u64) -> Column {
    let w = width as usize;
    let mut buf = vec![0u8; w * usize::try_from(nobs).expect("nobs fits usize")];
    for (i, cell) in data.iter().enumerate() {
        let s = cell.as_str().unwrap_or("").as_bytes();
        let n = s.len().min(w);
        buf[i * w..i * w + n].copy_from_slice(&s[..n]);
    }
    Column::from_row_major(StorageType::Str { width }, &buf, w, 0, nobs)
}

/// A `float` variable built from a closure over an existing one, the way an
/// untyped `generate` does: `float` storage, missing propagates.
#[must_use]
pub fn gen_float(src: &[f64], f: impl Fn(f64) -> f64) -> Column {
    Column::Float(NumCol::from_slice(
        &src.iter()
            .map(|&x| {
                if stratum_core::is_missing(x) {
                    SYSMISS_F32
                } else {
                    #[allow(clippy::cast_possible_truncation)] // float storage IS the semantics
                    {
                        f(x) as f32
                    }
                }
            })
            .collect::<Vec<f32>>(),
    ))
}

/// A `float` variable from an already-computed `f64` vector — `predict`'s
/// output, stored as `float` unless `double` was asked for.
#[must_use]
pub fn float_col(vals: &[f64]) -> Column {
    Column::Float(NumCol::from_slice(
        &vals
            .iter()
            .map(|&x| {
                if stratum_core::is_missing(x) {
                    SYSMISS_F32
                } else {
                    #[allow(clippy::cast_possible_truncation)]
                    {
                        x as f32
                    }
                }
            })
            .collect::<Vec<f32>>(),
    ))
}

/// A borrow of a locally built column with the `%9.0g` format an untyped
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
