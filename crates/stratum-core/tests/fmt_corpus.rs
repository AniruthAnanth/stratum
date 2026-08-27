//! Q3 / A28 — replay the measured `(value, format) -> string` corpus captured
//! from the licensed Stata 18.5 and assert an EXACT string match.
//!
//! `tests/data/fmt_corpus.jsonl` is committed output, produced once by
//! `scripts/gen-fmt-corpus.do`: 100 000 doubles under 47 numeric formats plus
//! 12 000 date/time values under 10, so 4 820 000 cells. Regenerating it needs
//! a Stata license; running this test does not, which is spec §32 — the build
//! never requires Stata. Two independent runs of the generator produce a
//! byte-identical file, which is the property that makes it a golden at all.
//!
//! The file is JSONL with three record shapes:
//!
//! ```text
//! {"note": ...}                                  provenance, ignored
//! {"group": 0, "formats": ["%1.0g", ...]}         the format list for a group
//! {"g": 0, "x": "+1.19…X+000", "s": [ … ]}        one value, one cell per format
//! ```
//!
//! Values travel as Stata's own `%21x`, which is a lossless hex-float, so the
//! double this test formats is bit-identical to the one Stata formatted. A
//! decimal literal would have been a different number a few ulps away and the
//! whole exercise would have been theatre.
//!
//! A cell is `null` where Stata itself has no stable answer — see
//! `EXPECTED_NULLS`. Every other cell matches exactly: there is no
//! known-disagreement list, and adding one would be the wrong repair.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

use serde::Deserialize;
use stratum_core::fmt::FormatKind;
use stratum_core::missing::SYSMISS;
use stratum_core::StataFormat;

/// Cells the generator refused to capture, because Stata 18.5's `%w.0g` walks
/// off the end of its own digit buffer for `w >= 21` on a double whose integer
/// part needs more than 19 digits.
///
/// `di %24.0g 1e20` answers `1000000000000000000` — 1e18, two decades wrong —
/// and on a different run of the same binary the same call answers
/// `1000000999999999836.` followed by two bytes of uninitialised memory. The
/// generator compares every fixed-branch cell of a `>= 2^53` value against
/// `%40.0f`, which is stable and correct, and writes `null` where they differ;
/// scientific-branch cells at the same magnitudes are unaffected and stay in.
///
/// Pinned so the hole cannot grow silently. This crate prints the correct
/// 21-digit integer for those inputs and is not bug-compatible with a buffer
/// overrun, which is not a thing that can be implemented.
const EXPECTED_NULLS: usize = 7_517;

/// Total cells in the file, `null`s included. Pinned so a truncated or
/// half-regenerated corpus fails loudly instead of passing on a subset.
const EXPECTED_CELLS: usize = 4_820_000;

/// Values above SYSMISS that the engine can never hold — see
/// `violates_invariant_m`. One value, times the 47 group-0 formats.
const EXPECTED_INVARIANT_M_SKIPS: usize = 47;

#[derive(Deserialize)]
struct ValueRec<'a> {
    g: u64,
    #[serde(borrow)]
    x: &'a str,
    /// `None` is JSON `null`; see `EXPECTED_NULLS`.
    #[serde(borrow)]
    s: Vec<Option<&'a str>>,
}

#[derive(Deserialize)]
struct GroupRec {
    group: u64,
    formats: Vec<String>,
}

fn corpus_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/data/fmt_corpus.jsonl")
}

/// Decode Stata's `%21x`: `[+-]<0|1>.<13 hex>X<[+-]hex exponent>`.
///
/// A leading `0` means the biased exponent field is zero — Stata writes every
/// subnormal (and zero itself) that way, with the exponent `-3ff`.
fn from_hex21(s: &str) -> f64 {
    let b = s.as_bytes();
    let neg = b[0] == b'-';
    let lead = b[1] - b'0';
    assert_eq!(b[2], b'.', "malformed %21x: {s}");
    let mantissa = u64::from_str_radix(&s[3..16], 16).expect("13 hex mantissa digits");
    assert_eq!(b[16], b'X', "malformed %21x: {s}");
    let esign = if b[17] == b'-' { -1i32 } else { 1 };
    let exp = esign * i32::from_str_radix(&s[18..], 16).expect("hex exponent");
    let biased: u64 = if lead == 0 {
        0
    } else {
        u64::try_from(exp + 1023).expect("exponent in range")
    };
    let bits = (u64::from(neg) << 63) | (biased << 52) | mantissa;
    f64::from_bits(bits)
}

/// Invariant M (CONTRACTS §13.1). A double outside it cannot exist in the
/// engine: `canon` maps it to `.` at the end of every kernel and the `.dta`
/// reader canonicalises on load. Stata's `display` does not canonicalise, so
/// the corpus contains a handful of such values and they are the ONE
/// documented disagreement class — see the assertion at the bottom.
fn violates_invariant_m(v: f64) -> bool {
    if v <= -SYSMISS {
        return true;
    }
    if v < SYSMISS {
        return false;
    }
    // At or above SYSMISS the only legal doubles are the 27 sentinels. `tag_of`
    // clamps an out-of-range tag to `.`, so the raw bits are what decides.
    let raw = (v.to_bits() - stratum_core::missing::F64_MISS_BITS) >> 40;
    raw > 26
}

/// Plain `%w.0g` cells the corpus renders scientifically. The denominator for
/// [`A28_WRONG_CELLS`].
const A28_SCIENTIFIC_CELLS: usize = 1_552_057;

/// How many of those the audit's literal `esig` formula would get wrong.
///
/// A28 specifies `let esig = (w as i32 - 6).max(1) as usize;`, i.e. `max(w - 7,
/// 0)` decimals whatever the exponent looks like. Stata's measured floor is a
/// seven-character body — `max(w - 1, 7) - 4 - expdigits` decimals — so a
/// two-digit exponent keeps one decimal at every width and a three-digit
/// exponent keeps none. The two rules disagree on **more than half** of the
/// scientific corpus: A28 prints `1.e+04` where StataMP 18.5 prints `1.2e+04`
/// (62 558 cells at `%6.0g` alone) and `1.0e+300` where Stata prints
/// `1.e+300` (26 330 cells at `%8.0g`).
///
/// This crate implements the measured rule, so `esig` is a **deliberate
/// deviation from the letter of A28** and this constant is the evidence for it,
/// recomputed from the oracle on every run rather than asserted in prose. A28's
/// actual purpose — that `%6.0g` must not request −1 digits and panic inside
/// `format!` — is met: `auto_decimals` is `saturating_sub` over a floor of 7 and
/// cannot underflow, which `fmt_golden.rs` pins at every legal width.
const A28_WRONG_CELLS: usize = 829_603;

/// The decimal count of a measured scientific cell — `1.2e+06` is 1, `1.e+300`
/// is 0 — or `None` when the cell is not a plain `%w.0g` rendered
/// scientifically. Deliberately strict: anything it does not recognise is left
/// out of the count rather than guessed at.
fn sci_decimals(f: &StataFormat, cell: &str) -> Option<usize> {
    if f.kind != FormatKind::General || f.prec != 0 || f.commas || f.left || f.zero_pad {
        return None;
    }
    let body = cell.trim();
    let body = body.strip_prefix('-').unwrap_or(body);
    // The first `e` is the exponent marker or nothing is: a cell holds only
    // digits, `.`, a sign and at most one `e`. `.e` — a missing-value tag, not a
    // number — is why this is a length-checked split and not `split_at(1)`.
    let (mantissa, exp) = body.split_once('e')?;
    let (sign, exp_digits) = exp.split_at_checked(1)?;
    if !matches!(sign, "+" | "-")
        || exp_digits.len() < 2
        || !exp_digits.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let (lead, frac) = mantissa.split_once('.')?;
    if lead.len() != 1
        || !lead.bytes().all(|b| b.is_ascii_digit())
        || !frac.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    Some(frac.len())
}

#[test]
fn replays_the_stata_corpus_byte_for_byte() {
    let file = File::open(corpus_path()).expect("tests/data/fmt_corpus.jsonl");
    // 75 MB of JSONL: stream it and borrow each cell out of the line buffer
    // rather than materialising the whole file and a serde_json::Value tree.
    let mut reader = BufReader::with_capacity(1 << 20, file);

    let mut formats: BTreeMap<u64, Vec<StataFormat>> = BTreeMap::new();
    let mut spec_names: BTreeMap<u64, Vec<String>> = BTreeMap::new();
    let mut checked = 0usize;
    let mut cells = 0usize;
    let mut nulls = 0usize;
    let mut skipped = 0usize;
    let mut a28_cells = 0usize;
    let mut a28_wrong = 0usize;
    // format spec -> (count, first three examples)
    let mut failures: BTreeMap<String, (usize, Vec<String>)> = BTreeMap::new();

    let mut line = String::new();
    let mut lineno = 0usize;
    loop {
        line.clear();
        if reader.read_line(&mut line).expect("read corpus") == 0 {
            break;
        }
        lineno += 1;
        let text = line.trim_end();
        if text.is_empty() {
            continue;
        }
        if text.starts_with(r#"{"note"#) {
            continue; // provenance
        }
        if text.starts_with(r#"{"group"#) {
            let rec: GroupRec =
                serde_json::from_str(text).unwrap_or_else(|e| panic!("line {lineno}: {e}"));
            let parsed = rec
                .formats
                .iter()
                .map(|s| StataFormat::parse(s).unwrap_or_else(|e| panic!("cannot parse {s}: {e}")))
                .collect();
            formats.insert(rec.group, parsed);
            spec_names.insert(rec.group, rec.formats);
            continue;
        }

        let rec: ValueRec =
            serde_json::from_str(text).unwrap_or_else(|e| panic!("line {lineno}: {e}"));
        let x = from_hex21(rec.x);
        let fmts = &formats[&rec.g];
        let names = &spec_names[&rec.g];
        assert_eq!(rec.s.len(), fmts.len(), "line {lineno}");
        cells += rec.s.len();

        if violates_invariant_m(x) {
            skipped += fmts.len();
            continue;
        }

        for ((f, name), w) in fmts.iter().zip(names).zip(&rec.s) {
            let Some(expect) = *w else {
                nulls += 1;
                continue;
            };
            // Stata's overrun writes raw memory; if a regeneration ever slips a
            // non-printable byte past the generator's guard, fail here rather
            // than freeze it into the golden.
            assert!(
                expect.bytes().all(|b| (0x20..0x7f).contains(&b)),
                "line {lineno}: {name} cell is not printable ASCII: {expect:?}"
            );
            if let Some(decimals) = sci_decimals(f, expect) {
                a28_cells += 1;
                // A28's rule, evaluated against the oracle rather than against
                // us, so the comparison stands even if this crate is wrong too.
                if usize::from(f.width).saturating_sub(7) != decimals {
                    a28_wrong += 1;
                }
            }
            let got = f.format_f64(x);
            checked += 1;
            if got != expect {
                let e = failures.entry(name.clone()).or_insert((0, Vec::new()));
                e.0 += 1;
                if e.1.len() < 3 {
                    e.1.push(format!(
                        "  {name} of {} ({x:e})\n    want [{expect}]\n    got  [{got}]",
                        rec.x
                    ));
                }
            }
        }
    }

    if !failures.is_empty() {
        let total: usize = failures.values().map(|(n, _)| *n).sum();
        let mut msg = format!("{total} of {checked} corpus cases disagree with Stata\n");
        for (name, (n, ex)) in &failures {
            msg.push_str(&format!("{name}: {n} failures\n"));
            for e in ex {
                msg.push_str(e);
                msg.push('\n');
            }
        }
        panic!("{msg}");
    }

    // The corpus cannot silently shrink, and neither of the two residual
    // disagreement classes can silently grow.
    assert_eq!(cells, EXPECTED_CELLS, "the corpus changed size");
    assert_eq!(
        nulls, EXPECTED_NULLS,
        "the Stata wide-%g defect set changed"
    );
    // INVARIANT M. `di %30.0g 1e308` is `.z_` on Stata: 1e308 is above SYSMISS
    // and Stata's display routine walks off the end of its own tag table. No
    // such double can reach a formatter here — `canon` turns it into `.` at the
    // end of the kernel that produced it — so the two differ only on inputs the
    // engine cannot hold. Exactly one value in the corpus, times 47 formats.
    // (`-1e308` is NOT one of them: Stata canonicalised the negative to `.` on
    // the way in and only kept the positive out of domain, which is an
    // asymmetry of its own.)
    assert_eq!(
        skipped, EXPECTED_INVARIANT_M_SKIPS,
        "the Invariant-M skip set changed"
    );
    assert_eq!(checked, EXPECTED_CELLS - EXPECTED_NULLS - skipped);

    // A28's `esig` formula, scored against the same oracle every other cell in
    // this file is scored against. See [`A28_WRONG_CELLS`].
    assert_eq!(
        a28_cells, A28_SCIENTIFIC_CELLS,
        "the scientific corpus changed"
    );
    assert_eq!(
        a28_wrong, A28_WRONG_CELLS,
        "the A28 `esig` counter-evidence changed; the deviation recorded in \
         fmt/numeric.rs must be re-derived before this number is re-pinned"
    );
    assert!(
        a28_wrong * 2 > a28_cells,
        "A28's esig rule is wrong on {a28_wrong} of {a28_cells} scientific cells"
    );
}
