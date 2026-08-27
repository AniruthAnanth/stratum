//! Stata display formats — ONE implementation for the whole workspace (C12).
//!
//! Three were planned: `05`'s `fmt_g/fmt_g5/fmt_f/fmt_fc` family, `04`'s
//! `StataFormat` grammar with `%w.dfc` and the `%t*` dates, and `02`'s
//! `format_g(v, 18, 0)` for stringifying `=exp` in a macro. They are the same
//! function, and three copies of a `%g` that nobody has fully specified is
//! three answers to "what does this number look like". So both layers live
//! here, and `fmt_g(x, w) == StataFormat::parse("%{w}.0g").format_f64(x)` is a
//! test rather than a convention.
//!
//! `scripts/check-topology.sh check_number_format` greps the workspace for a
//! float precision spec in a format string outside this module: a user-visible
//! number that goes through `format!` directly will disagree with the classic
//! text on the first tie-breaking case.

pub mod datetime;
pub mod leapsec;
pub mod numeric;
pub mod parse;

pub use numeric::MAX_SIG;
pub use parse::FormatError;

use serde::{Deserialize, Serialize};

/// A parsed display format. Parsed once at load, never per cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StataFormat {
    /// Which rendering rule applies.
    pub kind: FormatKind,
    /// Field width, 1..=244.
    pub width: u16,
    /// Digits after `.` in the format string. `0` means "automatic", which is
    /// why `%9.0g` is the default format and not a degenerate one.
    pub prec: u8,
    /// Leading `-`: left-justify.
    pub left: bool,
    /// Leading `0`: pad with zeros rather than spaces.
    pub zero_pad: bool,
    /// Trailing `c`: thousands separators.
    pub commas: bool,
}

/// The format families Stata has.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FormatKind {
    /// `%w.dg`
    General,
    /// `%w.df`
    Fixed,
    /// `%w.de`
    Exponential,
    /// `%ws`
    Str,
    /// `%21x`
    Hex,
    /// `%t*`
    DateTime(DateTimeFmt),
}

/// The date/time codes. The epoch is 1960-01-01 for all of them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DateTimeFmt {
    /// `%tc` — milliseconds since 1960-01-01 00:00:00.000, no leap seconds.
    Ms,
    /// `%tC` — the same, WITH leap seconds.
    MsLeap,
    /// `%td` — days since 1960-01-01.
    Day,
    /// `%tw` — weeks; a Stata year has exactly 52 of them.
    Week,
    /// `%tm` — months since 1960m1.
    Month,
    /// `%tq` — quarters.
    Quarter,
    /// `%th` — half-years.
    HalfYear,
    /// `%ty` — the calendar year itself.
    Year,
    /// `%tg` — generic: the number, in the general format.
    Generic,
}

impl Default for StataFormat {
    /// `%9.0g`, Stata's own default for a `float`.
    fn default() -> Self {
        StataFormat {
            kind: FormatKind::General,
            width: 9,
            prec: 0,
            left: false,
            zero_pad: false,
            commas: false,
        }
    }
}

impl StataFormat {
    /// A `%w.dg` format, the shape almost every caller wants.
    #[must_use]
    pub const fn general(width: u16, prec: u8) -> Self {
        StataFormat {
            kind: FormatKind::General,
            width,
            prec,
            left: false,
            zero_pad: false,
            commas: false,
        }
    }

    /// Render a numeric value.
    #[must_use]
    pub fn format_f64(&self, x: f64) -> String {
        let w = self.width as usize;
        let r = match self.kind {
            FormatKind::General | FormatKind::Str => numeric::general(x, w, self.prec, self.commas),
            FormatKind::Fixed => numeric::fixed(x, w, self.prec, self.commas),
            FormatKind::Exponential => numeric::exponential(x, w, self.prec),
            FormatKind::Hex => numeric::Rendered::of(numeric::hex_body(x), w),
            FormatKind::DateTime(dt) => self.format_datetime(dt, x),
        };
        // Zero padding is a property of a POSITIONAL number, so a missing
        // value, a date and a scientific fallback all revert to spaces:
        // `%012.2f .` is eleven spaces and a dot, and `%012.2f 2997197234.5`
        // is `    3.00e+09`.
        let zero = self.zero_pad
            && !crate::missing::is_missing(x)
            && !r.text.contains('e')
            && matches!(
                self.kind,
                FormatKind::General | FormatKind::Fixed | FormatKind::Exponential
            );
        justify(&r, self.left, zero, w)
    }

    /// Render a string value. Never truncates: a value wider than the field
    /// overflows it, exactly as an over-wide number does.
    #[must_use]
    pub fn format_str(&self, s: &str) -> String {
        justify(
            &numeric::Rendered::of(s.to_owned(), self.width as usize),
            self.left,
            false,
            self.width as usize,
        )
    }

    fn format_datetime(&self, dt: DateTimeFmt, x: f64) -> numeric::Rendered {
        let w = self.width as usize;
        if let Some(t) = crate::missing::tag_of(x) {
            return numeric::Rendered::of(crate::missing::TAG_NAME[t as usize].to_owned(), w);
        }
        // Dates are integer scales; a fractional value FLOORS, so 00:00:00.5 is
        // still 01jan1960 and -0.5 is 31dec1959 (measured).
        let floored = numeric_floor_i64(x);
        let rendered = floored.and_then(|v| match dt {
            DateTimeFmt::Ms => datetime::fmt_tc(v, false),
            DateTimeFmt::MsLeap => datetime::fmt_tc(v, true),
            DateTimeFmt::Day => datetime::fmt_td(v),
            DateTimeFmt::Week => datetime::fmt_tw(v),
            DateTimeFmt::Month => datetime::fmt_tm(v),
            DateTimeFmt::Quarter => datetime::fmt_tq(v),
            DateTimeFmt::HalfYear => datetime::fmt_th(v),
            DateTimeFmt::Year => datetime::fmt_ty(v),
            DateTimeFmt::Generic => None,
        });
        // Out of the calendar (and `%tg`, which never had one) falls back to the
        // general format's BODY at the code's content width, then plain
        // right-justification in the display width. The two differ only for
        // `%tc`/`%tC`, whose 18-column field renders a 12-column number:
        // `%tc 1e15` is `       1.00000e+15`.
        numeric::Rendered::of(
            rendered
                .unwrap_or_else(|| numeric::general_body(x, dt.content_width() as usize, 0, false)),
            w,
        )
    }
}

/// `i64` floor of a date value, or `None` when it cannot be one.
fn numeric_floor_i64(x: f64) -> Option<i64> {
    let f = crate::math::floor(x);
    if (-9.0e18..=9.0e18).contains(&f) {
        Some(f as i64)
    } else {
        None
    }
}

/// Right-justify, left-justify, or zero-pad after the sign.
///
/// The padding is computed from [`numeric::Rendered::layout`], not from the
/// text length. They differ only where Stata truncates a long integer and pads
/// for the number it meant to print.
///
/// A body wider than the field is returned whole. Stata never truncates a
/// number to fit the column — it overflows — and silently dropping a digit is
/// the worst available failure for a statistics product.
fn justify(r: &numeric::Rendered, left: bool, zero_pad: bool, w: usize) -> String {
    // The sign column a scientific body reserves is a RIGHT-justification
    // artefact: `%12.0g 9.999e99` is ` 1.00000e+100`, thirteen columns, while
    // `%-12.0g` of the same double is the bare `1.00000e+100` with no trailing
    // space. Left-justified output never pads past the format's own width.
    let field = if left {
        r.field.min(w.max(r.layout))
    } else {
        r.field
    };
    let pad = field.saturating_sub(r.layout);
    if pad == 0 {
        return r.text.clone();
    }
    let mut s = String::with_capacity(field);
    if left {
        s.push_str(&r.text);
        s.extend(core::iter::repeat_n(' ', pad));
    } else if zero_pad {
        let rest = if let Some(t) = r.text.strip_prefix('-') {
            s.push('-');
            t
        } else {
            &r.text
        };
        s.extend(core::iter::repeat_n('0', pad));
        s.push_str(rest);
    } else {
        s.extend(core::iter::repeat_n(' ', pad));
        s.push_str(&r.text);
    }
    s
}

/// Stata's `%w.0g` — "the most informative representation that fits".
#[must_use]
pub fn fmt_g(x: f64, w: usize) -> String {
    StataFormat::general(w as u16, 0).format_f64(x)
}

/// `%w.5g` — five significant digits. Root MSE and nothing else, in v1.
#[must_use]
pub fn fmt_g5(x: f64, w: usize) -> String {
    StataFormat::general(w as u16, 5).format_f64(x)
}

/// `%w.df`.
#[must_use]
pub fn fmt_f(x: f64, w: usize, d: usize) -> String {
    StataFormat {
        kind: FormatKind::Fixed,
        width: w as u16,
        prec: d as u8,
        left: false,
        zero_pad: false,
        commas: false,
    }
    .format_f64(x)
}

/// `%w.dfc` — comma grouping. "Number of obs = 12,481".
#[must_use]
pub fn fmt_fc(x: f64, w: usize, d: usize) -> String {
    StataFormat {
        kind: FormatKind::Fixed,
        width: w as u16,
        prec: d as u8,
        left: false,
        zero_pad: false,
        commas: true,
    }
    .format_f64(x)
}

/// `%w.de`.
#[must_use]
pub fn fmt_e(x: f64, w: usize, d: usize) -> String {
    StataFormat {
        kind: FormatKind::Exponential,
        width: w as u16,
        prec: d as u8,
        left: false,
        zero_pad: false,
        commas: false,
    }
    .format_f64(x)
}

/// The lossless `%21x` transport for a double.
#[must_use]
pub fn fmt_hex(x: f64) -> String {
    numeric::hex_body(x)
}

/// Stringify a value for macro expansion (`02`'s `format_g(v, 18, 0)` then
/// trim). `local a = 1/3` puts `.33333333333333` in the macro, not a rounded
/// display string.
#[must_use]
pub fn fmt_macro(x: f64) -> String {
    fmt_g(x, 18).trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_two_layers_are_the_same_function() {
        // C12's assertion, over a spread wide enough to hit every branch.
        for w in 1usize..=20 {
            for &x in &[
                0.0f64,
                1.5,
                -1.5,
                1_234_567.891,
                0.000_012_345,
                317_252_881.243_971_1,
                1e300,
                crate::missing::SYSMISS,
                crate::missing::missing_f64(26),
            ] {
                let spec = format!("%{w}.0g");
                assert_eq!(
                    fmt_g(x, w),
                    StataFormat::parse(&spec).unwrap().format_f64(x),
                    "x = {x}, w = {w}"
                );
            }
        }
    }

    #[test]
    fn justification_and_padding() {
        assert_eq!(fmt_g(1234.5, 9), "   1234.5");
        assert_eq!(
            StataFormat::parse("%-12.0g").unwrap().format_f64(1234.5),
            "1234.5      "
        );
        assert_eq!(
            StataFormat::parse("%012.2f").unwrap().format_f64(1234.5),
            "000001234.50"
        );
        assert_eq!(
            StataFormat::parse("%012.2f")
                .unwrap()
                .format_f64(crate::missing::SYSMISS),
            "           ."
        );
    }

    #[test]
    fn dates_render_and_fall_back() {
        assert_eq!(
            StataFormat::parse("%td").unwrap().format_f64(22_000.0),
            "26mar2020"
        );
        assert_eq!(
            StataFormat::parse("%tc").unwrap().format_f64(0.0),
            "01jan1960 00:00:00"
        );
        assert_eq!(
            StataFormat::parse("%td").unwrap().format_f64(1e9),
            " 1.00e+09"
        );
        assert_eq!(
            StataFormat::parse("%th").unwrap().format_f64(22_000.0),
            "2.2e+04"
        );
        assert_eq!(
            StataFormat::parse("%tw").unwrap().format_f64(22_000.0),
            " 2383w5"
        );
    }
}
