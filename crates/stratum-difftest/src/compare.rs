//! The comparator: one for both halves of the harness, corpus and live.
//!
//! Two channels feed it. Classic text is compared **byte for byte** — a
//! formatting bug and a numerics bug both fail it, which is the point (05
//! §17.3 row 1). Captured results are keyed records: numbers parse from their
//! `%21.17g` strings at this moment and no earlier, missing values match by
//! code before any tolerance is consulted, and everything numeric goes
//! through its [`crate::tolerance::Class`].
//!
//! Everything counted here is a counter in the ADR-017 sense: work done, not
//! time taken. The integration tests pin the counters so a case that stops
//! being compared turns the suite red instead of shrinking it silently.

use std::collections::BTreeMap;
use std::fmt;

use stratum_proto::capture::CaptureRecord;

use crate::capture::{key, Value};
use crate::tolerance::Class;

/// What the run did — the ADR-017 counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counters {
    /// Corpus/live cases compared end to end.
    pub cases: u32,
    /// Classic-text blocks compared byte-exact.
    pub text_blocks: u32,
    /// Bytes of classic text compared.
    pub text_bytes: u64,
    /// `return list` / `ereturn list` listings compared.
    pub listings: u32,
    /// Scalar results compared.
    pub scalars: u32,
    /// Macro results compared.
    pub macros: u32,
    /// Matrix shapes compared.
    pub matrices: u32,
    /// Function (`e(sample)`) names compared.
    pub functions: u32,
    /// Capture records compared (live channel).
    pub records: u32,
}

/// One difference found. The harness never stops at the first: a differential
/// that reports one mismatch per run is a differential someone re-runs
/// twenty times.
#[derive(Clone, Debug)]
pub struct Mismatch {
    /// The case that produced it.
    pub case: String,
    /// Which channel: `text`, `scalar`, `macro`, `matrix`, `function`,
    /// `record`, `rc`.
    pub channel: &'static str,
    /// Human-readable detail with both sides.
    pub detail: String,
}

impl fmt::Display for Mismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} [{}]: {}", self.case, self.channel, self.detail)
    }
}

/// A comparison in progress: counters plus every mismatch found so far.
#[derive(Debug, Default)]
pub struct Report {
    /// The ADR-017 counters.
    pub counters: Counters,
    /// Every difference found, in discovery order.
    pub mismatches: Vec<Mismatch>,
}

impl Report {
    /// Green?
    #[must_use]
    pub fn ok(&self) -> bool {
        self.mismatches.is_empty()
    }

    fn push(&mut self, case: &str, channel: &'static str, detail: String) {
        self.mismatches.push(Mismatch {
            case: case.to_owned(),
            channel,
            detail,
        });
    }

    /// Compare a classic-text block byte for byte.
    pub fn text(&mut self, case: &str, stata: &str, ours: &str) {
        self.counters.text_blocks += 1;
        self.counters.text_bytes += stata.len() as u64;
        if stata == ours {
            return;
        }
        let detail = first_divergence(stata, ours);
        self.push(case, "text", detail);
    }

    /// Compare one scalar: Stata's printed string against our f64, under the
    /// class the name selects. Missing matches by code only.
    pub fn scalar(&mut self, case: &str, name: &str, stata_printed: &str, ours: f64) {
        self.counters.scalars += 1;
        let want = Value::parse(stata_printed);
        let got = Value::parse(&crate::capture::g17(ours));
        if !values_match(name, &want, &got) {
            self.push(
                case,
                "scalar",
                format!(
                    "{name}: Stata `{stata_printed}`, ours `{}`",
                    crate::capture::g17(ours)
                ),
            );
        }
    }

    /// Compare one macro exactly.
    pub fn macro_(&mut self, case: &str, name: &str, stata: &str, ours: Option<&str>) {
        self.counters.macros += 1;
        if ours != Some(stata) {
            self.push(
                case,
                "macro",
                format!("{name}: Stata `{stata}`, ours {ours:?}"),
            );
        }
    }

    /// Compare one matrix shape.
    pub fn matrix_shape(
        &mut self,
        case: &str,
        name: &str,
        stata: (usize, usize),
        ours: Option<(usize, usize)>,
    ) {
        self.counters.matrices += 1;
        if ours != Some(stata) {
            self.push(
                case,
                "matrix",
                format!("{name}: Stata {} x {}, ours {ours:?}", stata.0, stata.1),
            );
        }
    }

    /// Compare the function-name lists (`e(sample)`).
    pub fn functions(&mut self, case: &str, stata: &[String], ours: &[&str]) {
        self.counters.functions += u32::try_from(stata.len()).unwrap_or(u32::MAX);
        if stata.iter().map(String::as_str).ne(ours.iter().copied()) {
            self.push(
                case,
                "function",
                format!("functions: Stata {stata:?}, ours {ours:?}"),
            );
        }
    }

    /// Compare two full capture-record streams (the live channel).
    ///
    /// Records are keyed by [`key`]; a key present on one side only is a
    /// mismatch, and matched keys compare their values under the class the
    /// name selects. Order is deliberately NOT compared here — committed
    /// captures are canonically sorted, so order carries no information.
    pub fn records(&mut self, case: &str, stata: &[CaptureRecord], ours: &[CaptureRecord]) {
        let stata_by: BTreeMap<String, &CaptureRecord> =
            stata.iter().map(|r| (key(r), r)).collect();
        let ours_by: BTreeMap<String, &CaptureRecord> = ours.iter().map(|r| (key(r), r)).collect();
        for (k, s) in &stata_by {
            self.counters.records += 1;
            match ours_by.get(k) {
                None => self.push(case, "record", format!("{k}: missing on our side")),
                Some(o) => self.record_pair(case, k, s, o),
            }
        }
        for k in ours_by.keys() {
            if !stata_by.contains_key(k) {
                self.push(case, "record", format!("{k}: extra on our side"));
            }
        }
    }

    fn record_pair(&mut self, case: &str, k: &str, stata: &CaptureRecord, ours: &CaptureRecord) {
        use CaptureRecord as R;
        match (stata, ours) {
            (R::Scalar { name, value: sv }, R::Scalar { value: ov, .. })
            | (R::Cell { name, value: sv }, R::Cell { value: ov, .. })
            | (R::Coef { name, value: sv }, R::Coef { value: ov, .. })
            | (
                R::Obs {
                    var: name,
                    value: sv,
                    ..
                },
                R::Obs { value: ov, .. },
            ) => {
                let plain = plain_of(name);
                if !values_match(plain, &Value::parse(sv), &Value::parse(ov)) {
                    self.push(case, "record", format!("{k}: Stata `{sv}`, ours `{ov}`"));
                }
            }
            (R::Macro { value: sv, .. }, R::Macro { value: ov, .. }) => {
                if sv != ov {
                    self.push(case, "record", format!("{k}: Stata `{sv}`, ours `{ov}`"));
                }
            }
            (
                R::Matrix {
                    rows: sr,
                    cols: sc,
                    rownames: srn,
                    colnames: scn,
                    ..
                },
                R::Matrix {
                    rows: or,
                    cols: oc,
                    rownames: orn,
                    colnames: ocn,
                    ..
                },
            ) => {
                if (sr, sc) != (or, oc) {
                    self.push(
                        case,
                        "record",
                        format!("{k}: shape Stata {sr} x {sc}, ours {or} x {oc}"),
                    );
                } else if (!srn.is_empty() && !orn.is_empty() && srn != orn)
                    || (!scn.is_empty() && !ocn.is_empty() && scn != ocn)
                {
                    self.push(case, "record", format!("{k}: row/col names differ"));
                }
            }
            (
                R::Var {
                    stype: ss,
                    format: sf,
                    vlabel: sl,
                    ..
                },
                R::Var {
                    stype: os,
                    format: of,
                    vlabel: ol,
                    ..
                },
            ) => {
                if (ss, sf, sl) != (os, of, ol) {
                    self.push(case, "record", format!("{k}: metadata differs"));
                }
            }
            _ => self.push(case, "record", format!("{k}: record kinds differ")),
        }
    }
}

/// The one comparison rule: missing by code first, tolerance only for
/// number-vs-number, text exactly.
fn values_match(plain_name: &str, want: &Value, got: &Value) -> bool {
    match (want, got) {
        // BY CODE, NEVER BY TOLERANCE. `.a` vs `.b` must fail even though the
        // sentinels are adjacent doubles.
        (Value::Missing(a), Value::Missing(b)) => a == b,
        (Value::Num(w), Value::Num(g)) => Class::of_name(plain_name).matches(*g, *w),
        (Value::Text(a), Value::Text(b)) => a == b,
        _ => false,
    }
}

/// `e(V)[mpg,weight]` → `V`; `r(mean)` → `mean`; anything else unchanged.
fn plain_of(name: &str) -> &str {
    let inner = name
        .strip_prefix("e(")
        .or_else(|| name.strip_prefix("r("))
        .or_else(|| name.strip_prefix("s("))
        .unwrap_or(name);
    match inner.find(')') {
        Some(i) => &inner[..i],
        None => inner,
    }
}

/// Locate the first differing line for the text channel's report.
fn first_divergence(stata: &str, ours: &str) -> String {
    for (i, (s, o)) in stata.lines().zip(ours.lines()).enumerate() {
        if s != o {
            return format!(
                "first divergence at line {}:\n  stata: {s:?}\n  ours:  {o:?}",
                i + 1
            );
        }
    }
    format!(
        "line counts differ: stata {}, ours {}",
        stata.lines().count(),
        ours.lines().count()
    )
}

#[cfg(test)]
#[allow(clippy::excessive_precision, clippy::inconsistent_digit_grouping)] // the
                                                                           // float literals below transcribe %21.17g strings digit for digit, on purpose.
mod tests {
    use super::*;

    #[test]
    fn missing_codes_never_meet_a_tolerance() {
        // `.a` and `.b` are one sentinel step apart; a tolerance comparison
        // would call them equal. The comparator must not.
        assert!(values_match("x", &Value::Missing(1), &Value::Missing(1)));
        assert!(!values_match("x", &Value::Missing(1), &Value::Missing(2)));
        // A missing never equals a number, however large the number.
        assert!(!values_match("x", &Value::Missing(0), &Value::Num(8.0e307)));
    }

    #[test]
    fn a_perturbed_scalar_is_caught() {
        let mut r = Report::default();
        r.scalar("t", "mean", "21.297297297297297", 21.297_297_297_297_297);
        assert!(r.ok());
        r.scalar(
            "t",
            "mean",
            "21.297297297297297",
            21.297_297_297_297_297 * (1.0 + 1e-9),
        );
        assert_eq!(r.mismatches.len(), 1, "the perturbation must be caught");
        assert_eq!(r.counters.scalars, 2);
    }

    #[test]
    fn a_perturbed_text_byte_is_caught() {
        let mut r = Report::default();
        r.text("t", "  mpg |  74\n", "  mpg |  74\n");
        assert!(r.ok());
        r.text("t", "  mpg |  74\n", "  mpg |  75\n");
        assert_eq!(r.mismatches.len(), 1);
        assert_eq!(r.counters.text_blocks, 2);
    }

    #[test]
    fn record_streams_report_missing_and_extra_keys() {
        use stratum_proto::capture::CaptureRecord as R;
        let stata = vec![
            R::Scalar {
                name: "e(N)".into(),
                value: "74".into(),
            },
            R::Scalar {
                name: "e(r2)".into(),
                value: ".4995593889723035".into(),
            },
        ];
        let ours = vec![
            R::Scalar {
                name: "e(N)".into(),
                value: "74".into(),
            },
            R::Scalar {
                name: "e(extra)".into(),
                value: "1".into(),
            },
        ];
        let mut r = Report::default();
        r.records("t", &stata, &ours);
        assert_eq!(r.mismatches.len(), 2, "{:?}", r.mismatches);
        assert_eq!(r.counters.records, 2, "stata-side records counted");
    }
}
