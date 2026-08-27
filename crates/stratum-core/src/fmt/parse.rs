//! The display-format grammar (`04` §8), accepting exactly what Stata accepts.
//!
//! ```text
//! format   := '%' ['-'] ['0'] width ['.' prec] type ['c']
//!           | '%' ['-'] 't' datecode
//! width    := 1..=244
//! prec     := 0..=99,  and measured: prec <= width - 1
//! type     := 'f' | 'g' | 'e' | 's' | 'x' | 'X'
//! datecode := 'c'|'C'|'d'|'w'|'m'|'q'|'h'|'y'|'g'
//! ```
//!
//! The `prec <= width - 1` rule is measured, not assumed: `%9.8g` is accepted
//! and `%9.9g` is `r(120) invalid %format`, and likewise `%12.11e` against
//! `%12.12e`. So is the restriction of `%x` to width 21 — `%20x` and `%9x` are
//! both rejected.
//!
//! A format is parsed ONCE, at load or at `format` time, and stored parsed.
//! Re-parsing per cell would put a string scan on the Data Editor's hot path.

use super::{DateTimeFmt, FormatKind, StataFormat};

/// Why a format string was rejected. Stata answers `r(120) invalid %format`
/// for all of them; the variants exist so a diagnostic can say which.
#[derive(Clone, Copy, PartialEq, Eq, Debug, thiserror::Error)]
pub enum FormatError {
    /// Did not start with `%`.
    #[error("a display format starts with %")]
    NoPercent,
    /// Width missing, zero, or above 244.
    #[error("width must be 1..=244")]
    BadWidth,
    /// Precision above 99, or not below the width.
    #[error("precision must be 0..=99 and less than the width")]
    BadPrecision,
    /// Type letter absent or not one of `fgesxX` / `t<code>`.
    #[error("unknown format type")]
    BadType,
    /// `%x` exists only at width 21.
    #[error("the hexadecimal format is %21x")]
    BadHexWidth,
    /// Characters after a complete format.
    #[error("trailing characters after the format")]
    Trailing,
}

impl StataFormat {
    /// Parse a Stata display format.
    ///
    /// # Errors
    ///
    /// [`FormatError`] for anything Stata answers `r(120)` to.
    pub fn parse(s: &str) -> Result<StataFormat, FormatError> {
        let b = s.as_bytes();
        let mut i = 0usize;
        if b.first() != Some(&b'%') {
            return Err(FormatError::NoPercent);
        }
        i += 1;

        let mut left = false;
        if b.get(i) == Some(&b'-') {
            left = true;
            i += 1;
        }

        // `%t*` carries no width in the string; each code has its own.
        if b.get(i) == Some(&b't') {
            i += 1;
            let code = *b.get(i).ok_or(FormatError::BadType)?;
            i += 1;
            let dt = match code {
                b'c' => DateTimeFmt::Ms,
                b'C' => DateTimeFmt::MsLeap,
                b'd' => DateTimeFmt::Day,
                b'w' => DateTimeFmt::Week,
                b'm' => DateTimeFmt::Month,
                b'q' => DateTimeFmt::Quarter,
                b'h' => DateTimeFmt::HalfYear,
                b'y' => DateTimeFmt::Year,
                b'g' => DateTimeFmt::Generic,
                _ => return Err(FormatError::BadType),
            };
            // Stata allows a trailing detail string (`%tdD_m_Y`). v1 renders the
            // default layout and does not silently ignore a detail it cannot
            // honour, so anything after the code is rejected rather than dropped.
            if i != b.len() {
                return Err(FormatError::Trailing);
            }
            return Ok(StataFormat {
                kind: FormatKind::DateTime(dt),
                width: dt.default_width(),
                prec: 0,
                left,
                zero_pad: false,
                commas: false,
            });
        }

        let mut zero_pad = false;
        if b.get(i) == Some(&b'0') {
            zero_pad = true;
            i += 1;
        }

        let start = i;
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        if i == start {
            return Err(FormatError::BadWidth);
        }
        let width: u32 = s[start..i].parse().map_err(|_| FormatError::BadWidth)?;
        if width == 0 || width > 244 {
            return Err(FormatError::BadWidth);
        }

        let mut prec: u32 = 0;
        let mut had_prec = false;
        if b.get(i) == Some(&b'.') {
            i += 1;
            let ps = i;
            while b.get(i).is_some_and(u8::is_ascii_digit) {
                i += 1;
            }
            if i == ps {
                return Err(FormatError::BadPrecision);
            }
            prec = s[ps..i].parse().map_err(|_| FormatError::BadPrecision)?;
            had_prec = true;
            if prec > 99 || prec >= width {
                return Err(FormatError::BadPrecision);
            }
        }

        let ty = *b.get(i).ok_or(FormatError::BadType)?;
        i += 1;
        let kind = match ty {
            b'g' => FormatKind::General,
            b'f' => FormatKind::Fixed,
            b'e' => FormatKind::Exponential,
            b's' => FormatKind::Str,
            b'x' | b'X' => {
                if had_prec {
                    return Err(FormatError::BadPrecision);
                }
                if width != 21 {
                    return Err(FormatError::BadHexWidth);
                }
                FormatKind::Hex
            }
            _ => return Err(FormatError::BadType),
        };

        let mut commas = false;
        if b.get(i) == Some(&b'c') {
            if !matches!(kind, FormatKind::General | FormatKind::Fixed) {
                return Err(FormatError::BadType);
            }
            commas = true;
            i += 1;
        }
        if i != b.len() {
            return Err(FormatError::Trailing);
        }

        Ok(StataFormat {
            kind,
            width: width as u16,
            prec: prec as u8,
            left,
            zero_pad,
            commas,
        })
    }
}

impl DateTimeFmt {
    /// The field width Stata renders this code in.
    #[must_use]
    pub const fn default_width(self) -> u16 {
        match self {
            DateTimeFmt::Ms | DateTimeFmt::MsLeap => 18,
            DateTimeFmt::Day | DateTimeFmt::Generic => 9,
            DateTimeFmt::Week | DateTimeFmt::Month => 7,
            DateTimeFmt::Quarter | DateTimeFmt::HalfYear => 6,
            DateTimeFmt::Year => 4,
        }
    }

    /// The width the NUMERIC fallback is computed at when the value is outside
    /// the calendar. It is the display width for every code except `%tc`/`%tC`,
    /// whose 18-column field carries a 12-column number: `%tc 1e15` is
    /// `       1.00000e+15`, five decimals rather than the eleven an 18-column
    /// general format would give.
    #[must_use]
    pub const fn content_width(self) -> u16 {
        match self {
            DateTimeFmt::Ms | DateTimeFmt::MsLeap => 12,
            other => other.default_width(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_what_stata_accepts() {
        for s in [
            "%9.0g", "%-9.0g", "%09.2f", "%12.2fc", "%8.0gc", "%21x", "%21X", "%20.18f", "%12.4e",
            "%9s", "%-12s", "%tc", "%tC", "%td", "%tw", "%tm", "%tq", "%th", "%ty", "%tg",
            "%244.0g", "%9.8g", "%20.19g", "%12.11e",
        ] {
            assert!(StataFormat::parse(s).is_ok(), "should accept {s}");
        }
    }

    #[test]
    fn rejects_what_stata_rejects() {
        // Every one of these is r(120) on StataMP 18.5 (probe: `capture di %f 1.5`).
        for s in [
            "9.0g", "%0.0g", "%245.0g", "%9.9g", "%20.20g", "%12.12e", "%9.99f", "%9.100f",
            "%9.0x", "%20x", "%9x", "%tz", "%t", "%9q", "%9.0gz",
        ] {
            assert!(StataFormat::parse(s).is_err(), "should reject {s}");
        }
    }

    #[test]
    fn flags_are_read_off() {
        let f = StataFormat::parse("%-012.4fc").unwrap();
        assert!(f.left && f.zero_pad && f.commas);
        assert_eq!(f.width, 12);
        assert_eq!(f.prec, 4);
        assert_eq!(f.kind, FormatKind::Fixed);
    }

    #[test]
    fn date_widths_are_the_measured_ones() {
        assert_eq!(StataFormat::parse("%tc").unwrap().width, 18);
        assert_eq!(StataFormat::parse("%td").unwrap().width, 9);
        assert_eq!(StataFormat::parse("%tw").unwrap().width, 7);
        assert_eq!(StataFormat::parse("%tq").unwrap().width, 6);
        assert_eq!(StataFormat::parse("%ty").unwrap().width, 4);
        assert_eq!(StataFormat::parse("%tg").unwrap().width, 9);
    }
}
