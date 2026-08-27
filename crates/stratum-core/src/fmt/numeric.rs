//! The numeric formatting engine: `%g`, `%f`, `%e`, `%x` (`04` §8, `05` §4).
//!
//! Everything here was derived from the licensed Stata 18.5 and is replayed by
//! `tests/fmt_corpus.rs` against 10⁵ captured values under 47 formats — 4.7M
//! `(value, format) -> string` cells, matched exactly, with no
//! known-disagreement list. Where `05` §4's published sketch and the machine
//! disagree, the machine wins, and the difference is written down below rather
//! than smoothed over.
//!
//! # `%w.dg` — the rule, as measured
//!
//! A candidate positional rendering of `|x|` **fits** when
//!
//! ```text
//! len <= w - 1   if it contains a '.'
//! len <= w - 2   if it does not
//! ```
//!
//! One column is always reserved for the sign, and a second is reserved for the
//! decimal point whether or not the number has one. That single rule is what
//! makes `%9.0g 0.63074906` print `.6307491` rather than the `.63074906` that
//! fits nine columns perfectly well, and it subsumes every magnitude window in
//! `05` §4: `%10.0g 317252881` is scientific because the nine digits need
//! `w >= 11`, `%3.0g 1` is scientific because `1.0` needs `w >= 4`, and
//! `%9.0g 12345678` is scientific because eight digits need `w >= 10`.
//!
//! 1. Missing renders as `.`/`.a`..`.z`.
//! 2. Fixed notation is available when `|x| >= 1e-5` and the integer part has at
//!    most [`MAX_INT_DIGITS`] digits. The `1e-5` floor is hard and independent
//!    of width: `9.9999e-6` is scientific at `%24.0g` just as at `%8.0g`.
//! 3. Inside that window, take the LARGEST `p` in `2..=pmax` whose UNSTRIPPED
//!    rendering fits, then strip trailing fraction zeros and a trailing `.`.
//!    `pmax` is [`MAX_SIG`] when `d == 0`, and `max(d, digits before the point)`
//!    otherwise. The fit is judged on the shape the value has BEFORE rounding.
//! 4. Otherwise scientific, `decimals = w - 5 - expdigits`, floored so the body
//!    is never shorter than seven characters — EXCEPT when a `p` that fitted
//!    was then taken away because rounding carried the integer part past the
//!    digit budget, which is its own branch with its own decimal count.
//!
//! Five corrections to `05` §4, all measured:
//!
//! * The fit test runs on the **unstripped** string. `%7.0g 1e-5` is
//!   `1.0e-05`, not `.00001`, because the candidate measured is `.000010`.
//! * `esig` is `w - 6` only above `w = 8`; the floor is a 7-character body, so
//!   two significant digits with a two-digit exponent and one with a
//!   three-digit exponent. A28 clamps `esig` at 1, which is one too low: it
//!   would print `1.e+04` where Stata prints `1.2e+04`, and `1.0e+300` where
//!   Stata prints `1.e+300`. This is the one place the crate departs from the
//!   LETTER of a plan bullet, so it is scored rather than argued —
//!   `tests/fmt_corpus.rs` evaluates A28's formula against the same oracle
//!   every other cell is scored against and pins the result: **wrong on
//!   829 603 of the corpus's 1 552 057 scientific cells**, over half. What A28
//!   was for — `%6.0g` must not request −1 digits and panic — is met by
//!   [`auto_decimals`], whose subtraction saturates under a floor of 7 and
//!   cannot underflow at any legal width.
//! * A tie rounds **half away from zero** in `%g` and `%e` (`%4.0g 0.125` is
//!   `.13`) and **half to even** in `%f` (`%9.2f 0.125` is `0.12`). They are
//!   different code paths inside Stata and they round differently; one shared
//!   rounder fails one of them.
//! * `%w.dg` with `d > 0` caps the significant digits at `max(d, integer
//!   digits)` — `%9.2g 2130.77` is `2131`, not `2100`, because the integer part
//!   is never sacrificed to the precision request.
//! * `%w.dgc` reduces precision to make room for the separators before it gives
//!   them up: `%12.0gc 1234567.891` is `1,234,567.9`, one digit poorer than the
//!   ungrouped `1234567.891` that also fits.
//! * The width test runs on the UNROUNDED shape, and the answer depends on
//!   which of the two ways a positional candidate can fail actually happened.
//!   Too wide to begin with is the ordinary fallback: `%9.2g 99999999.9` is
//!   ` 1.0e+08`, `min(d - 1, auto)`. Wide enough, and then lost to a carry, is
//!   not: `%9.2g 9999999.9` is ` 1.00e+07`, three significant digits where `d`
//!   is two, because seven digits DID fit nine columns before `9999999.9`
//!   rounded to `10000000`. The second branch takes `pmax - 5` decimals — the
//!   width rule applied to the positional field it failed in — which is why
//!   `%12.5g 99999.9` is ` 1.0e+05` and `%12.5g 9999999.9` is `1.00e+07`.
//! * The decimal count is settled against the exponent the VALUE has, and a
//!   carry that lengthens the exponent does not reopen it: `%3.0f 9.99e99` is
//!   ` 1.0e+100`, nine columns for a three-column format. When the carry
//!   SHORTENS the exponent the reservation stays and becomes a pad column:
//!   `%1.0g 9.9e-100` is `  1.e-99`, eight columns for a body of six.
//!
//! # A reproduced Stata bug
//!
//! Stata's positional path emits at most [`MAX_SIG`] digit characters but
//! right-justifies for the full length, so `di %24.0g 1e20` prints
//! `1000000000000000000` — 1e18 — in a 22-column field. [`Rendered::layout`]
//! carries the untruncated length so this reproduces the shape byte for byte.
//! It is a wrong number and we print it anyway, because the product's contract
//! is that classic output matches Stata; it needs `w >= 21` and an integer part
//! longer than [`MAX_SIG`] to trigger, so no default format reaches it.
//!
//! The corpus does NOT pin those cells, and cannot: Stata writes past its own
//! digit buffer there, so `%24.0g 1e20` is `1000000000000000000.` plus two
//! bytes of uninitialised memory on one run and `1000000000000000000` on the
//! next. `scripts/gen-fmt-corpus.do` writes them as JSON `null` and
//! `tests/fmt_corpus.rs` pins the count. What IS pinned is the boundary —
//! [`MAX_INT_DIGITS`] — because Stata's choice of branch is stable even where
//! the positional body is not.

use crate::missing::{tag_of, TAG_NAME};

/// The significant-digit ceiling, measured: `%22.0g (1/3)` stops at
/// `.3333333333333333148` and `%30.0g` of a subnormal stops at
/// `9.999888671826830054e-321`. Both are 19 digits.
pub const MAX_SIG: usize = 19;

/// Integer parts longer than this are shown scientifically however wide the
/// field is. Measured by walking `w` from 19 to 100 over 1e18..1e25: a value
/// needing 24 integer digits turns positional at `w = 26` (`%26.0g 1e24`, whose
/// double is the 24-digit 999999999999999983222784), while 1.1e24 — 25 digits —
/// is still `1.100000000000000008e+24` at `%100.0g`.
pub const MAX_INT_DIGITS: usize = 24;

/// Guard digits taken when only the EXPONENT of a value is wanted. Large
/// enough that a two-digit rendering cannot round 999999999.9 up into a tenth
/// integer digit and hand `%12.5g` a significant digit it must not have.
const GUARD: usize = 25;
/// The rendered magnitude of a number: `p` significant digits and the power of
/// ten of the leading one.
struct Decimal {
    digits: Vec<u8>,
    exp10: i32,
}

/// Round `|x|` to `p` significant decimal digits, HALF AWAY FROM ZERO.
///
/// **Stata rounds twice, and reproducing that is the whole job.** It first
/// produces [`MAX_SIG`] digits from the exact binary value, then rounds THAT
/// decimal string down to `p`. The two-step is visible in the corpus:
/// `%24.0g -222568280284.98953` is `-222568280284.9895325` — nineteen exact
/// digits — and `%20.0g` of the same double is `-222568280284.989533`, where a
/// single correct rounding of the exact value `…9895324707…` would have given
/// `…989532`. The intermediate `…9895325` makes it a tie, and a tie rounds away
/// from zero.
///
/// Rust's `{:.*e}` is correctly rounded but rounds ties to even, so the first
/// step takes guard digits and rounds here. When the guard window looks exactly
/// like a tie the full expansion is taken, because "looks like a tie at 25
/// digits" and "is a tie" are not the same thing: `2.675` is not tied at three
/// digits (the double is `2.67499999999999982`) and must round DOWN, while
/// `0.125` is tied at two and must round UP.
fn decimal(ax: f64, p: usize) -> Decimal {
    // Step one is exactly Rust's `{:.*e}`: correctly rounded from the binary
    // value, ties to even. `%24.0g -241253962596934.53125` is
    // `-241253962596934.5312` — the tie at nineteen digits went to the even 2.
    let (digits, exp10) = raw_digits(ax, MAX_SIG - 1);
    let wide = Decimal { digits, exp10 };
    if p >= MAX_SIG {
        wide
    } else {
        round_digits(wide, p)
    }
}

impl Decimal {
    /// Apply a rounding decision that was made against the guard digits.
    fn bump(mut self, up: bool, n: usize) -> Decimal {
        if !up {
            return self;
        }
        let mut i = n;
        loop {
            if i == 0 {
                self.digits.insert(0, 1);
                self.digits.truncate(n);
                self.exp10 += 1;
                break;
            }
            i -= 1;
            if self.digits[i] == 9 {
                self.digits[i] = 0;
            } else {
                self.digits[i] += 1;
                break;
            }
        }
        self
    }
}

/// Step two: round an existing digit string to `p`, half away from zero. Every
/// tie here is exact, because the string is the number.
fn round_digits(mut d: Decimal, p: usize) -> Decimal {
    let up = d.digits.len() > p && d.digits[p] >= 5;
    d.digits.truncate(p);
    while d.digits.len() < p {
        d.digits.push(0);
    }
    d.bump(up, p)
}

/// `n+1` correctly rounded significant digits of `ax`, plus the exponent.
fn raw_digits(ax: f64, n: usize) -> (Vec<u8>, i32) {
    if ax == 0.0 {
        return (vec![0; n + 1], 0);
    }
    let s = format!("{:.*e}", n, ax);
    let (mant, exp) = s
        .split_once('e')
        .expect("scientific rendering has an exponent");
    let exp10: i32 = exp.parse().expect("exponent is an integer");
    let digits: Vec<u8> = mant
        .bytes()
        .filter(u8::is_ascii_digit)
        .map(|b| b - b'0')
        .collect();
    (digits, exp10)
}

/// A rendered body plus the number of columns Stata justifies it against.
///
/// They differ only where Stata truncates a long integer (see the module note):
/// `text` is what is printed, `layout` is what the padding is computed from.
#[derive(Clone, Debug)]
pub struct Rendered {
    /// The characters to print.
    pub text: String,
    /// Columns the padding calculation believes `text` occupies.
    pub layout: usize,
    /// The field the body is right-justified in, which is not always the
    /// format's own width.
    pub field: usize,
}

impl Rendered {
    /// A body whose printed length is exactly what the padding is computed
    /// from — every case but Stata's long-integer truncation.
    #[must_use]
    pub fn of(text: String, field: usize) -> Self {
        Rendered {
            layout: text.chars().count(),
            text,
            field,
        }
    }
}

/// Positional rendering of a [`Decimal`], with the leading `0` before the point
/// dropped the way Stata drops it. Trailing zeros are KEPT: the caller measures
/// this string and strips afterwards.
fn positional(dec: &Decimal) -> String {
    let p = dec.digits.len();
    let mut out = String::with_capacity(p + 8);
    if dec.exp10 >= 0 {
        let int_len = (dec.exp10 as usize) + 1;
        if int_len >= p {
            for d in &dec.digits {
                out.push((b'0' + d) as char);
            }
            for _ in p..int_len {
                out.push('0');
            }
        } else {
            for d in &dec.digits[..int_len] {
                out.push((b'0' + d) as char);
            }
            out.push('.');
            for d in &dec.digits[int_len..] {
                out.push((b'0' + d) as char);
            }
        }
    } else {
        out.push('.');
        for _ in 0..(-dec.exp10 - 1) {
            out.push('0');
        }
        for d in &dec.digits {
            out.push((b'0' + d) as char);
        }
    }
    out
}

/// Cut a pure-integer body after [`MAX_SIG`] digit characters — Stata's own
/// truncation (see the module note).
///
/// Only integers are affected. A fractional rendering already carries at most
/// `p <= MAX_SIG` significant digits, and its leading `.0000` zeros must not be
/// counted against the ceiling: `%30.0g 1.2345e-5` keeps all of
/// `.00001234499999999999954`.
fn truncate_digits(s: &str) -> String {
    if s.contains('.') || s.chars().filter(char::is_ascii_digit).count() <= MAX_SIG {
        return s.to_owned();
    }
    let mut seen = 0usize;
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_ascii_digit() {
            if seen == MAX_SIG {
                break;
            }
            seen += 1;
        }
        out.push(c);
    }
    out
}

/// Drop trailing zeros in the fraction, then a bare trailing point.
fn strip_fraction(s: &str) -> &str {
    if !s.contains('.') {
        return s;
    }
    let t = s.trim_end_matches('0');
    t.strip_suffix('.').unwrap_or(t)
}

/// Insert thousands separators into the integer part.
fn commafy(s: &str) -> String {
    let (int_part, rest) = match s.find('.') {
        Some(i) => (&s[..i], &s[i..]),
        None => (s, ""),
    };
    let n = int_part.len();
    let mut out = String::with_capacity(n + n / 3 + rest.len());
    for (i, c) in int_part.chars().enumerate() {
        if i > 0 && (n - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out.push_str(rest);
    out
}

/// The measured fit rule: one column for the sign, and one for the decimal
/// point whether the candidate has one or not.
fn fits(candidate: &str, w: usize) -> bool {
    let reserve = if candidate.contains('.') { 1 } else { 2 };
    candidate.len() + reserve <= w
}

/// Number of digits in the decimal exponent, at least two — Stata never prints
/// a one-digit exponent.
fn exp_digits(exp10: i32) -> usize {
    if exp10.unsigned_abs() >= 100 {
        3
    } else {
        2
    }
}

/// The scientific body of `|x|`, e.g. `3.17e+08`, with `decimals` after the
/// point. The point is printed even when `decimals` is 0 (`1.e+300`).
fn sci_body(ax: f64, decimals: usize) -> String {
    let p = (decimals + 1).min(MAX_SIG);
    let dec = decimal(ax, p);
    let mut out = String::with_capacity(decimals + 8);
    out.push((b'0' + dec.digits[0]) as char);
    out.push('.');
    for i in 1..=decimals {
        out.push(if i < dec.digits.len() {
            (b'0' + dec.digits[i]) as char
        } else {
            '0'
        });
    }
    let e = if ax == 0.0 { 0 } else { dec.exp10 };
    out.push('e');
    out.push(if e < 0 { '-' } else { '+' });
    let a = e.unsigned_abs();
    if a >= 100 {
        out.push_str(&a.to_string());
    } else {
        out.push((b'0' + (a / 10) as u8) as char);
        out.push((b'0' + (a % 10) as u8) as char);
    }
    out
}

/// The width-derived decimal count for the scientific branch.
///
/// The 7-character floor on the body is measured: it is why a two-digit
/// exponent keeps one decimal at every width and a three-digit exponent keeps
/// none (`%1.0g 1e15` is `1.0e+15`, `%8.0g 1e300` is `1.e+300`).
fn auto_decimals(w: usize, exp10: i32) -> usize {
    let budget = w.saturating_sub(1).max(7);
    budget.saturating_sub(4 + exp_digits(exp10))
}

/// Scientific rendering of `x` honouring an explicit decimal request.
///
/// Returns `(text, clipped)`. `clipped` is true when [`MAX_SIG`] cut the
/// mantissa short of what the width asked for, which measurably costs the
/// result one column of field (see [`general`]).
fn scientific_text(
    x: f64,
    w: usize,
    requested: Option<usize>,
    carried_budget: Option<usize>,
) -> (String, bool, i32) {
    let ax = x.abs();
    let exp10 = if ax == 0.0 {
        0
    } else {
        raw_digits(ax, GUARD).1
    };
    let want = |e: i32| {
        // A positional attempt abandoned because the carry outgrew the digit
        // budget does NOT come back through the width rule: the width that
        // applies is the positional field it failed in, `pmax + 1` columns.
        // Measured across ten formats and every exponent in the corpus:
        // `%12.5g 99999.9` is ` 1.0e+05` (pmax 5) while `%12.5g 9999999.9` is
        // `1.00e+07` (pmax 7), same width, same `d`, different answer; and
        // `%11.3g 999.9` is `1.0e+03` while `%11.3g 999999999.9` is
        // `1.0000e+09`, same format, different value.
        let auto = auto_decimals(w, e);
        if let Some(pmax) = carried_budget {
            // The explicit `d` does NOT apply on this path — `%9.2g` of
            // 9.9999999e6 is ` 1.00e+07`, three significant digits where `d` is
            // two — but the width still does: `%9.5g 9999999999.9` is
            // ` 1.00e+10` where `%12.5g` of the same double is ` 1.00000e+10`.
            return (pmax + 1)
                .saturating_sub(4 + exp_digits(e))
                .max(1)
                .min(auto);
        }
        requested.map_or(auto, |r| r.min(auto))
    };
    let pick = |e: i32| want(e).min(MAX_SIG - 1);
    let clipped = want(exp10) > MAX_SIG - 1;
    // The decimal count is settled against the exponent the VALUE has, and
    // rounding that carries the exponent into another digit does not reopen it:
    // `%3.0f 9.99e99` is ` 1.0e+100`, nine columns for a three-column format,
    // where re-deriving the budget from the carried `e+100` would have printed
    // ` 1.e+100`. The field, not the body, absorbs the difference — see
    // [`sci_field`].
    let body = sci_body(ax, pick(exp10));
    let text = if x.is_sign_negative() && ax != 0.0 {
        format!("-{body}")
    } else {
        body
    };
    (text, clipped, exp10)
}

fn exp_digits_of_body(body: &str) -> usize {
    body.rsplit('e').next().map_or(2, |t| t.len() - 1)
}

/// The field a scientific body occupies. A non-negative number always gets its
/// sign column, so `%1.0g 0` is eight characters wide in a one-column field.
///
/// `exp10` is the exponent BEFORE rounding. When a carry shrinks it from three
/// digits to two the body loses a column but the field does not: `%1.0g` of
/// 9.9e-100 is `  1.e-99`, eight columns, because the space was reserved for
/// the exponent the value had.
fn sci_field(text: &str, x: f64, w_eff: usize, exp10: i32) -> usize {
    let reserved = exp_digits(exp10).saturating_sub(exp_digits_of_body(text));
    let slot = text.chars().count() + reserved + usize::from(x >= 0.0 || x.is_nan());
    w_eff.max(slot)
}

/// The lower end of the fixed-notation window. Hard, and independent of width:
/// `9.9999e-6` is scientific at `%24.0g` just as it is at `%8.0g`.
const FIXED_MIN: f64 = 1e-5;

/// `%w.dg` — the general format.
#[must_use]
pub fn general(x: f64, w: usize, d: u8, commas: bool) -> Rendered {
    if let Some(t) = tag_of(x) {
        return Rendered::of(TAG_NAME[t as usize].to_owned(), w);
    }

    let ax = x.abs();
    let mut carried_budget: Option<usize> = None;
    if ax >= FIXED_MIN || ax == 0.0 {
        // The UNROUNDED exponent: 999999999.9 has nine integer digits, and
        // reading it off a two-digit rendering would say ten and let `%12.5g`
        // ask for a tenth significant digit it must not have.
        let exp10 = if ax == 0.0 {
            0
        } else {
            raw_digits(ax, GUARD).1
        };
        let int_digits = if exp10 >= 0 { (exp10 + 1) as usize } else { 0 };
        if int_digits <= MAX_INT_DIGITS {
            let pmax = if d == 0 {
                MAX_SIG
            } else {
                usize::from(d).max(int_digits).min(MAX_SIG)
            };
            let pmin = 2usize.min(pmax);
            // Commas first: the candidate is chosen with the separators already
            // in it, so `%12.0gc 1234567.891` gives up a digit to keep the
            // grouping rather than printing the ungrouped form that also fits.
            let found = match commas.then(|| fixed_search(ax, pmin, pmax, w, true)) {
                Some(Ok(hit)) => Ok(hit),
                _ => fixed_search(ax, pmin, pmax, w, false),
            };
            match found {
                Ok((text, layout)) => {
                    let neg = x < 0.0;
                    return Rendered {
                        text: if neg { format!("-{text}") } else { text },
                        layout: layout + usize::from(neg),
                        field: w,
                    };
                }
                Err(NoFixed::CarriedPastBudget) => carried_budget = Some(pmax),
                Err(NoFixed::TooWide) => {}
            }
        }
    }

    let requested = if d == 0 {
        None
    } else {
        Some(usize::from(d) - 1)
    };
    let (text, clipped, exp10) = scientific_text(x, w, requested, carried_budget);
    // A mantissa the significant-digit ceiling cut short is padded as if the
    // exponent had two digits and the mantissa its full MAX_SIG: the pad is
    // `w - 25` however wide the exponent actually is. `%30.0g 9.9999e-6`
    // occupies 29 columns and `%30.0g 1e100` occupies 30.
    let field = if clipped {
        // The pad is `w - 25 - sign`: Stata computes it as though the mantissa
        // still had its full MAX_SIG digits and the exponent two. `%30.0g` is
        // 29 columns for 9.9999e-6, 30 for 1e100 and 29 for -3.31e-7.
        let sign = usize::from(x < 0.0);
        text.chars().count() + w.saturating_sub(MAX_SIG + 6 + sign)
    } else {
        sci_field(&text, x, w, exp10)
    };
    Rendered::of(text, field)
}

/// Why no positional rendering was usable. The distinction is not cosmetic:
/// the two run into DIFFERENT scientific branches (see [`general`]).
#[derive(Clone, Copy, PartialEq, Eq)]
enum NoFixed {
    /// Nothing fitted the width.
    TooWide,
    /// Every precision rounded up into a decade whose integer part is longer
    /// than the digit budget.
    CarriedPastBudget,
}

/// Whether the rendering the UNROUNDED value would have had fits.
///
/// Stata measures the width against the shape the value has BEFORE rounding.
/// It matters in both directions. Downward: a carry drops one of a sub-1
/// value's leading zeros and makes the string a column shorter, so `%4.0g
/// .0999` is ` 1.0e-01`, not the `  .1` the carried `.10` would have allowed —
/// while `%4.0g .999` stays `   1`, `.99` and `1.0` being the same length.
/// Upward: it decides WHICH scientific fallback applies (see [`fixed_search`]).
/// Only the digit COUNT matters here, so the shape is measured with zeros.
fn pre_fits(exp0: i32, p: usize, w: usize, commas: bool) -> bool {
    let shape = Decimal {
        digits: vec![0; p],
        exp10: exp0,
    };
    let raw = positional(&shape);
    let candidate = if commas { commafy(&raw) } else { raw };
    fits(&candidate, w)
}

/// Largest `p` whose unstripped rendering fits, stripped on the way out.
///
/// Returns `(text, layout)`: `layout` is the UNTRUNCATED length, because that is
/// what Stata pads against.
fn fixed_search(
    ax: f64,
    pmin: usize,
    pmax: usize,
    w: usize,
    commas: bool,
) -> Result<(String, usize), NoFixed> {
    let exp0 = raw_digits(ax, GUARD).1;
    let mut carried = false;
    for p in (pmin..=pmax).rev() {
        // The width is judged FIRST, and on the unrounded shape. A candidate
        // that was never going to fit is an ordinary too-wide case however it
        // rounds, and the two exits do not lead to the same place: `%9.2g` of
        // 99999999.9 is ` 1.0e+08` (eight integer digits never fitted nine
        // columns) while `%9.2g 9999999.9` is ` 1.00e+07` — seven digits fit,
        // and only then does the carry take the positional form away.
        if !pre_fits(exp0, p, w, commas) {
            continue;
        }
        let dec = decimal(ax, p);
        // Rounding that carries into a new decade is a candidate only while
        // the carried integer part still fits the digit budget. `%9.2g` of
        // 99.99999999999999 is `1.0e+02` — `100` needs three integer digits
        // and the budget was two — while `%9.2g 9.999999999999998` is `10`,
        // which needs exactly its two.
        if dec.exp10 != exp0 && (dec.exp10 + 1).max(0) as usize > pmax {
            // Only the FULL budget failing this way changes the fallback: that
            // is the value genuinely rounding out of its own decade.
            carried |= p == pmax;
            continue;
        }
        let raw = positional(&dec);
        let candidate = if commas { commafy(&raw) } else { raw };
        if fits(&candidate, w) {
            let stripped = strip_fraction(&candidate);
            return Ok((truncate_digits(stripped), stripped.len()));
        }
    }
    Err(if carried {
        NoFixed::CarriedPastBudget
    } else {
        NoFixed::TooWide
    })
}

/// `%w.df` — fixed notation with exactly `d` decimals, C's rounding.
///
/// Falls back to scientific only when the plain body (sign excluded) overflows
/// AND scientific is strictly shorter: `%20.18f -0.5` keeps
/// `-0.500000000000000000` even though it is 21 characters in a 20-column
/// field, while `%4.2f 1234567.891` becomes `1.2e+06`.
#[must_use]
pub fn fixed(x: f64, w: usize, d: u8, commas: bool) -> Rendered {
    if let Some(t) = tag_of(x) {
        return Rendered::of(TAG_NAME[t as usize].to_owned(), w);
    }
    let ax = x.abs();
    // `{:.*}` is C's rounding — ties to even — which is what `%f` wants and what
    // `%g` must NOT have. The precision is a runtime value, so this is not a
    // literal precision spec (C12's grep).
    let plain = format!("{:.*}", usize::from(d), ax);
    let sign = if x < 0.0 { "-" } else { "" };

    // The width a value NEEDS is the sign, the integer digits, the point and
    // the decimals — and a value below 1 has NO integer digits: its `0.` is
    // decoration. That is why `%20.18f -0.5` keeps `-0.500000000000000000`,
    // twenty-one characters in a twenty-column field, while `%20.18f -1.0`
    // becomes `-1.0000000000000e+00`.
    // Integer digits of the value BEFORE rounding: `%9.0f 999999999.9` stays
    // positional and prints `1000000000`, ten characters in a nine-column
    // field, because the number it was asked to render has nine integer digits.
    let int_digits = if ax >= 1.0 {
        (raw_digits(ax, GUARD).1 + 1).max(0) as usize
    } else {
        0
    };
    let decorative = usize::from(plain.starts_with("0."));
    let needed = sign.len() + int_digits + usize::from(d > 0) + usize::from(d);

    if commas {
        let c = format!("{sign}{}", commafy(&plain));
        if c.chars().count() - decorative <= w {
            return Rendered::of(c, w);
        }
    }
    if needed <= w {
        return Rendered::of(format!("{sign}{plain}"), w);
    }
    // Scientific only when it is strictly narrower than what fixed needs.
    // `%6.1f 678457.9` stays positional at eight characters in a six-column
    // field, because ` 6.8e+05` is eight characters too.
    let requested = if d == 0 { None } else { Some(usize::from(d)) };
    let (sci, _, exp10) = scientific_text(x, w, requested, None);
    let field = sci_field(&sci, x, w, exp10);
    if field.max(sci.chars().count()) < needed {
        Rendered::of(sci, field)
    } else {
        Rendered::of(format!("{sign}{plain}"), w)
    }
}

/// `%w.de` — scientific with exactly `d` decimals (`d == 0` means "as many as
/// the width allows", which is what `%8.0e` prints).
#[must_use]
pub fn exponential(x: f64, w: usize, d: u8) -> Rendered {
    if let Some(t) = tag_of(x) {
        return Rendered::of(TAG_NAME[t as usize].to_owned(), w);
    }
    let requested = if d == 0 { None } else { Some(usize::from(d)) };
    let (text, _, exp10) = scientific_text(x, w, requested, None);
    let field = sci_field(&text, x, w, exp10);
    Rendered::of(text, field)
}

/// The body of `%w.dg` without any field decision, for the `%t*` fallback.
#[must_use]
pub fn general_body(x: f64, w: usize, d: u8, commas: bool) -> String {
    general(x, w, d, commas).text
}

/// `%21x` — Stata's exact hexadecimal-float rendering.
///
/// `+1.199999999999aX+000` is 1.1: sign, the implicit leading digit, the 52
/// mantissa bits as thirteen lowercase hex digits, `X`, and the unbiased binary
/// exponent in signed three-digit hex. Zero and the subnormals carry a leading
/// `0` and the exponent `-3ff`, which is how Stata writes "no implicit one".
///
/// This is a lossless transport for a double, and it is what
/// `tests/data/fmt_corpus.jsonl` carries its values in.
#[must_use]
pub fn hex_body(x: f64) -> String {
    let bits = x.to_bits();
    let sign = if bits >> 63 == 1 { '-' } else { '+' };
    let biased = ((bits >> 52) & 0x7FF) as i32;
    let mantissa = bits & 0x000F_FFFF_FFFF_FFFF;
    let (lead, exp) = if biased == 0 {
        ('0', -1023)
    } else {
        ('1', biased - 1023)
    };
    let esign = if exp < 0 { '-' } else { '+' };
    format!(
        "{sign}{lead}.{mantissa:013x}X{esign}{:03x}",
        exp.unsigned_abs()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_published_goldens() {
        // 04 §8 and 05 §4, byte for byte.
        assert_eq!(general_body(1234567.891, 9, 0, false), "1234568");
        assert_eq!(general_body(0.000012345, 9, 0, false), ".0000123");
        assert_eq!(general_body(123456789.0, 9, 0, false), "1.23e+08");
        assert_eq!(general_body(12345678901.0, 9, 0, false), "1.23e+10");
        assert_eq!(general_body(1.5, 10, 0, false), "1.5");
        assert_eq!(general_body(317252881.2439711, 9, 0, false), "3.17e+08");
        assert_eq!(general_body(317252881.2439711, 10, 0, false), "3.173e+08");
        assert_eq!(general_body(317252881.2439711, 11, 0, false), "317252881");
        assert_eq!(general_body(317252881.2439711, 12, 0, false), "317252881.2");
        assert_eq!(general_body(4540178.784, 9, 0, false), "4540179");
        assert_eq!(general_body(8699525.974, 9, 0, false), "8699526");
        assert_eq!(general_body(2130.769528589715, 9, 0, false), "2130.77");
        assert_eq!(general_body(0.63074906, 9, 0, false), ".6307491");
        assert_eq!(general_body(-5853.6957, 9, 0, false), "-5853.696");
        assert_eq!(general_body(0.158902485820707, 9, 0, false), ".1589025");
    }

    #[test]
    fn rounding_differs_between_g_and_f() {
        // %g rounds an exact tie away from zero, %f rounds it to even.
        assert_eq!(general_body(0.125, 4, 0, false), ".13");
        assert_eq!(fixed(0.125, 9, 2, false).text, "0.12");
        assert_eq!(fixed(0.375, 9, 2, false).text, "0.38");
        assert_eq!(fixed(2.5, 9, 0, false).text, "2");
        assert_eq!(fixed(1.5, 9, 0, false).text, "2");
    }

    #[test]
    fn hex_round_trips() {
        for &v in &[1.1f64, 0.0, 1.0, 2.0, 256.0, -3.5, 1e300, 1234.5] {
            let s = hex_body(v);
            assert_eq!(s.len(), 21, "{s}");
        }
        assert_eq!(hex_body(1.1), "+1.199999999999aX+000");
        assert_eq!(hex_body(0.0), "+0.0000000000000X-3ff");
        assert_eq!(hex_body(-3.5), "-1.c000000000000X+001");
        assert_eq!(hex_body(1e300), "+1.7e43c8800759cX+3e4");
        assert_eq!(hex_body(crate::missing::SYSMISS), "+1.0000000000000X+3ff");
    }
}
