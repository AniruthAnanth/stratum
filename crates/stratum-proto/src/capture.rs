//! CONTRACTS.md §9 — the §32 differential-testing contract.
//!
//! Emitted identically by `stratum run --capture` and by
//! `tests/difftest/ado/stratum_capture.ado` running INSIDE Stata. That symmetry
//! is the entire point: the comparison is between two files in one format, not
//! between a format and a screen-scrape.
//!
//! Deliberately not a `specta::Type`: capture records never reach the frontend.

use serde::{Deserialize, Serialize};

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CaptureRecord {
    /// Numerics travel as STRINGS (`%21.17g` from Stata) and are parsed to f64
    /// only at comparison time. JSON numbers would reintroduce exactly the
    /// precision loss the format exists to prevent.
    Scalar {
        name: String,
        value: String,
    },
    Macro {
        name: String,
        value: String,
    },
    Matrix {
        name: String,
        rows: u32,
        cols: u32,
        rownames: Vec<String>,
        colnames: Vec<String>,
    },
    /// "e(V)[mpg,weight]"
    Cell {
        name: String,
        value: String,
    },
    Coef {
        name: String,
        value: String,
    },
    Var {
        name: String,
        stype: String,
        format: String,
        vlabel: Option<String>,
    },
    Obs {
        var: String,
        i: u64,
        value: String,
    },
}
