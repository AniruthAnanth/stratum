//! Capture files: reading, writing, canonical form, and the parse-at-compare
//! rule for `%21.17g` values.
//!
//! Both sides of the differential emit the **same** schema —
//! [`stratum_proto::capture::CaptureRecord`] — as NDJSON, one record per
//! line. Stata's side is written by `tests/difftest/ado/stratum_capture.ado`
//! with `string(value, "%21.17g")`; our side is written here with the same
//! format through `stratum_core::fmt` (C12: one formatter, both sides).
//! Numbers travel as strings and are parsed to f64 **only at compare time**,
//! because JSON numbers would reintroduce exactly the precision loss the
//! format exists to prevent.
//!
//! # Canonical form
//!
//! A committed `stata.jsonl` must be in canonical form so a re-capture diffs
//! minimally: LF line endings, every line a valid record, lines sorted
//! byte-lexicographically, no duplicate lines, trailing newline. The
//! comparator is order-insensitive (records are keyed), so canonical order
//! costs nothing and buys stable diffs. [`canonicalize`] produces it and
//! `lint` enforces it.

use std::fmt::Write as _;

use stratum_core::fmt::StataFormat;
use stratum_core::missing;
use stratum_proto::capture::CaptureRecord;
use stratum_stats::{MatrixValue, ResultKind, ResultSet};

/// A capture value at compare time: parsed, classified, never a raw string.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// A real number.
    Num(f64),
    /// A Stata missing value, by code: 0 is `.`, 1..=26 are `.a`..`.z`.
    /// Missing values compare **by code, never by tolerance** — `.a` and `.b`
    /// are adjacent bit patterns and any tolerance would call them equal.
    Missing(u8),
    /// Anything that is not a number: macro text, labels.
    Text(String),
}

impl Value {
    /// Parse a capture string. `%21.17g` output arrives space-padded, so the
    /// string is trimmed first.
    #[must_use]
    pub fn parse(s: &str) -> Value {
        let t = s.trim();
        if let Some(tag) = parse_missing(t) {
            return Value::Missing(tag);
        }
        // `f64::from_str` rejects `.` and `.a` (handled above) but accepts
        // every finite decimal and scientific form `%21.17g` can produce.
        match t.parse::<f64>() {
            Ok(v) if v.is_finite() => Value::Num(v),
            _ => Value::Text(t.to_owned()),
        }
    }
}

/// `.` → 0, `.a`..`.z` → 1..=26, anything else → None.
fn parse_missing(t: &str) -> Option<u8> {
    let rest = t.strip_prefix('.')?;
    match rest.as_bytes() {
        [] => Some(0),
        [c @ b'a'..=b'z'] => Some(c - b'a' + 1),
        _ => None,
    }
}

/// Render an f64 the way both sides of the differential do: missing values as
/// their code (`.`, `.a`…), finite numbers via the one `%21.17g` the product
/// has (`stratum_core::fmt`, C12), unpadded.
#[must_use]
pub fn g17(v: f64) -> String {
    if let Some(tag) = missing::tag_of(v) {
        return missing::TAG_NAME[tag as usize].to_owned();
    }
    StataFormat::general(21, 17).format_f64(v).trim().to_owned()
}

/// The identity of a record — what the comparator keys on. Two records with
/// the same key are "the same result" and their values are compared; a key on
/// one side only is itself a mismatch.
#[must_use]
pub fn key(r: &CaptureRecord) -> String {
    match r {
        CaptureRecord::Scalar { name, .. } => format!("scalar {name}"),
        CaptureRecord::Macro { name, .. } => format!("macro {name}"),
        CaptureRecord::Matrix { name, .. } => format!("matrix {name}"),
        CaptureRecord::Cell { name, .. } => format!("cell {name}"),
        CaptureRecord::Coef { name, .. } => format!("coef {name}"),
        CaptureRecord::Var { name, .. } => format!("var {name}"),
        CaptureRecord::Obs { var, i, .. } => format!("obs {var}[{i}]"),
    }
}

/// Serialize records to canonical NDJSON: one JSON object per line, lines
/// sorted byte-lexicographically, deduplicated, LF, trailing newline.
///
/// # Errors
///
/// Serialization failure (never expected for these types).
pub fn canonicalize(records: &[CaptureRecord]) -> Result<String, serde_json::Error> {
    let mut lines: Vec<String> = records
        .iter()
        .map(serde_json::to_string)
        .collect::<Result<_, _>>()?;
    lines.sort_unstable();
    lines.dedup();
    let mut out = String::new();
    for l in &lines {
        let _ = writeln!(out, "{l}");
    }
    Ok(out)
}

/// Is this NDJSON text already in canonical form? Returns the first problem,
/// or `None` when clean.
#[must_use]
pub fn canonical_problem(text: &str) -> Option<String> {
    if text.contains('\r') {
        return Some("contains CR; canonical form is LF only".to_owned());
    }
    if !text.is_empty() && !text.ends_with('\n') {
        return Some("missing trailing newline".to_owned());
    }
    let mut prev: Option<&str> = None;
    for (i, line) in text.lines().enumerate() {
        if let Err(e) = serde_json::from_str::<CaptureRecord>(line) {
            return Some(format!("line {}: not a CaptureRecord: {e}", i + 1));
        }
        if let Some(p) = prev {
            if p >= line {
                return Some(format!(
                    "line {}: not in strictly ascending byte order",
                    i + 1
                ));
            }
        }
        prev = Some(line);
    }
    None
}

/// Parse a capture file (NDJSON, one record per line, blank lines ignored).
///
/// # Errors
///
/// The 1-based line number and cause for the first malformed line.
pub fn read(text: &str) -> Result<Vec<CaptureRecord>, String> {
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<CaptureRecord>(line) {
            Ok(r) => out.push(r),
            Err(e) => return Err(format!("line {}: {e}", i + 1)),
        }
    }
    Ok(out)
}

/// Flatten a regenerated [`ResultSet`] into the capture schema, exactly as
/// `stratum_capture.ado` does on the Stata side: scalars, macros, matrix
/// shapes, then every matrix entry — `e(b)` columns as [`CaptureRecord::Coef`]
/// keyed by column name, every other matrix cell as [`CaptureRecord::Cell`]
/// keyed `e(M)[row,col]`.
#[must_use]
pub fn records_of(kind: ResultKind, rs: &ResultSet) -> Vec<CaptureRecord> {
    let p = match kind {
        ResultKind::RClass => "r",
        ResultKind::EClass => "e",
        ResultKind::SClass => "s",
    };
    let mut out = Vec::new();
    for (name, v) in rs.scalars() {
        out.push(CaptureRecord::Scalar {
            name: format!("{p}({name})"),
            value: g17(*v),
        });
    }
    for (name, v) in rs.macros() {
        out.push(CaptureRecord::Macro {
            name: format!("{p}({name})"),
            value: v.clone(),
        });
    }
    for name in rs.matrix_names() {
        let m = rs.matrix(name).expect("name came from matrix_names");
        out.push(CaptureRecord::Matrix {
            name: format!("{p}({name})"),
            rows: u32::try_from(m.rows).unwrap_or(u32::MAX),
            cols: u32::try_from(m.cols).unwrap_or(u32::MAX),
            rownames: m.rownames.clone(),
            colnames: m.colnames.clone(),
        });
        out.extend(matrix_cells(p, name, m));
    }
    out
}

fn matrix_cells(p: &str, name: &str, m: &MatrixValue) -> Vec<CaptureRecord> {
    let mut out = Vec::new();
    for i in 0..m.rows {
        for j in 0..m.cols {
            let value = g17(m.get(i, j));
            if name == "b" && m.rows == 1 {
                out.push(CaptureRecord::Coef {
                    name: m.colnames.get(j).cloned().unwrap_or_else(|| j.to_string()),
                    value,
                });
            } else {
                let r = m.rownames.get(i).cloned().unwrap_or_else(|| i.to_string());
                let c = m.colnames.get(j).cloned().unwrap_or_else(|| j.to_string());
                out.push(CaptureRecord::Cell {
                    name: format!("{p}({name})[{r},{c}]"),
                    value,
                });
            }
        }
    }
    out
}

#[cfg(test)]
#[allow(clippy::excessive_precision, clippy::inconsistent_digit_grouping)] // the
                                                                           // float literals below transcribe %21.17g strings digit for digit, on purpose.
mod tests {
    use super::*;

    #[test]
    fn values_parse_and_missing_is_by_code() {
        assert_eq!(Value::parse(" 74"), Value::Num(74.0));
        assert_eq!(
            Value::parse("6165.2567567567568"),
            Value::Num(6165.256_756_756_756_8)
        );
        assert_eq!(Value::parse("."), Value::Missing(0));
        assert_eq!(Value::parse(".a"), Value::Missing(1));
        assert_eq!(Value::parse(".z"), Value::Missing(26));
        assert_eq!(Value::parse(".ab"), Value::Text(".ab".to_owned()));
        assert_eq!(Value::parse("ols"), Value::Text("ols".to_owned()));
    }

    #[test]
    fn g17_round_trips_every_double_it_meets() {
        for v in [
            21.297_297_297_297_297,
            0.499_559_388_972_303_5,
            -670.099_014_076_324_8,
            3.0e-300,
            1.0,
            0.0,
        ] {
            let s = g17(v);
            match Value::parse(&s) {
                Value::Num(back) => assert_eq!(back.to_bits(), v.to_bits(), "{s}"),
                other => panic!("{s} parsed to {other:?}"),
            }
        }
    }

    #[test]
    fn g17_spells_missing_by_code() {
        assert_eq!(g17(missing::SYSMISS), ".");
        assert_eq!(g17(missing::missing_f64(1)), ".a");
        assert_eq!(g17(missing::missing_f64(26)), ".z");
    }

    #[test]
    fn canonical_form_is_sorted_and_detected() {
        let recs = vec![
            CaptureRecord::Scalar {
                name: "r(mean)".to_owned(),
                value: "21.297297297297297".to_owned(),
            },
            CaptureRecord::Scalar {
                name: "r(N)".to_owned(),
                value: "74".to_owned(),
            },
        ];
        let canon = canonicalize(&recs).expect("serialize");
        assert!(canonical_problem(&canon).is_none(), "{canon}");
        // The same lines reversed are flagged.
        let mut lines: Vec<&str> = canon.lines().collect();
        lines.reverse();
        let bad = format!("{}\n", lines.join("\n"));
        assert!(canonical_problem(&bad).is_some());
        // Round trip.
        let back = read(&canon).expect("read");
        assert_eq!(back.len(), 2);
    }
}
