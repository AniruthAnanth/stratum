//! Stata's date and time formats, on Howard Hinnant's calendar algorithms.
//!
//! The epoch is **1960-01-01**. `%td` counts days from it, `%tm` months from
//! `1960m1`, `%tq`/`%th`/`%tw` quarters/half-years/weeks likewise, and `%tc`
//! counts **milliseconds**. Everything except the civil-date conversion is
//! integer arithmetic on the epoch, and the conversion is `days_from_civil` /
//! `civil_from_days`: twelve lines, exact for the proleptic Gregorian calendar,
//! no table and no timezone database.
//!
//! Two measured surprises, both of which a from-memory implementation gets
//! wrong:
//!
//! * **A Stata week is 1/52 of a year, not seven days.** `%tw 22000` is
//!   `2383w5`, i.e. `year = 1960 + w div 52` and `week = w mod 52 + 1`, so the
//!   52nd week is eight or nine days long. Weeks do not accumulate drift
//!   against years, which is the whole point.
//! * **Out-of-range values fall back to the general numeric format** at the
//!   date format's own width, rather than erroring or wrapping: `%td 1e9` is
//!   ` 1.00e+09` and `%th 22000` is `2.2e+04`.
//!
//! Division is FLOOR division throughout, so negative dates work: `%tm -1` is
//! `1959m12`, not `1960m-1`.

use super::leapsec;

/// Days from 1970-01-01 to 1960-01-01 — the offset between the Unix epoch that
/// [`days_from_civil`] is written against and Stata's.
pub(crate) const STATA_EPOCH_DAYS: i64 = -3_653;

/// Days from the civil date to 1970-01-01. Hinnant's algorithm, exact for the
/// proleptic Gregorian calendar over the whole `i32` year range.
///
/// `const` so that [`leapsec`]'s table is built at compile time.
#[must_use]
pub const fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = y as i64 - if m <= 2 { 1 } else { 0 };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = ((m as i64) + 9) % 12; // Mar = 0
    let doy = (153 * mp + 2) / 5 + (d as i64) - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

/// The inverse of [`days_from_civil`].
#[must_use]
pub const fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11], Mar = 0
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m as u32, d as u32)
}

/// Floor division; `-1 / 52` must be `-1`, not `0`, or every pre-1960 date is
/// off by a year.
#[inline]
const fn fdiv(a: i64, b: i64) -> i64 {
    let q = a / b;
    if (a % b != 0) && ((a < 0) != (b < 0)) {
        q - 1
    } else {
        q
    }
}

#[inline]
const fn fmod(a: i64, b: i64) -> i64 {
    a - fdiv(a, b) * b
}

/// Lowercase three-letter month abbreviations, as `%td` prints them.
const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];

/// The inclusive year range every `%t*` format renders inside. Outside it the
/// caller falls back to the general numeric format.
const YEAR_MIN: i32 = 100;
const YEAR_MAX: i32 = 9_999;

/// `%td` — `26mar2020`. `None` when the value is out of calendar range.
#[must_use]
pub fn fmt_td(days: i64) -> Option<String> {
    let (y, m, d) = civil_from_days(days + STATA_EPOCH_DAYS);
    if !(YEAR_MIN..=YEAR_MAX).contains(&y) {
        return None;
    }
    Some(format!("{:02}{}{:04}", d, MONTHS[(m - 1) as usize], y))
}

/// `%tc` / `%tC` — `01jan1960 00:00:00`.
///
/// `leap` selects `%tC`: the value then counts real elapsed seconds, so the
/// inserted leap seconds are subtracted to get civil time, and an instant that
/// lands inside one displays as `:60`.
#[must_use]
pub fn fmt_tc(ms: i64, leap: bool) -> Option<String> {
    let (civil_ms, sixty) = if leap {
        // Inside the leap second the insertion has NOT fully elapsed, so
        // `leaps_elapsed` does not count it — and subtracting only the earlier
        // ones lands civil time on the following midnight, which then prints as
        // `00:00:60`. Stata prints `30jun1972 23:59:60`: the second belongs to
        // the END of the old day. Counting the in-progress one puts the clock
        // on 23:59:59 and the `sixty` flag rewrites the seconds field.
        let sixty = leapsec::inside_leap(ms);
        let elapsed = leapsec::leaps_elapsed(ms) + i64::from(sixty);
        (ms - elapsed * 1000, sixty)
    } else {
        (ms, false)
    };

    let days = fdiv(civil_ms, 86_400_000);
    let rem = fmod(civil_ms, 86_400_000);
    let (y, m, d) = civil_from_days(days + STATA_EPOCH_DAYS);
    if !(YEAR_MIN..=YEAR_MAX).contains(&y) {
        return None;
    }
    let hh = rem / 3_600_000;
    let mm = (rem % 3_600_000) / 60_000;
    let ss = if sixty { 60 } else { (rem % 60_000) / 1000 };
    Some(format!(
        "{:02}{}{:04} {:02}:{:02}:{:02}",
        d,
        MONTHS[(m - 1) as usize],
        y,
        hh,
        mm,
        ss
    ))
}

/// `%tw` — `2383w5`. A Stata year has exactly 52 weeks; the last one is long.
#[must_use]
pub fn fmt_tw(w: i64) -> Option<String> {
    period(w, 52, 'w')
}

/// `%tm` — `1960m1`.
#[must_use]
pub fn fmt_tm(m: i64) -> Option<String> {
    period(m, 12, 'm')
}

/// `%tq` — `1960q1`.
#[must_use]
pub fn fmt_tq(q: i64) -> Option<String> {
    period(q, 4, 'q')
}

/// `%th` — `1960h1`.
#[must_use]
pub fn fmt_th(h: i64) -> Option<String> {
    period(h, 2, 'h')
}

/// `%ty` — the calendar year itself, zero-padded to four digits.
#[must_use]
pub fn fmt_ty(y: i64) -> Option<String> {
    if !(i64::from(YEAR_MIN)..=i64::from(YEAR_MAX)).contains(&y) {
        return None;
    }
    Some(format!("{y:04}"))
}

/// The shared `year<letter><index>` shape behind `%tw`/`%tm`/`%tq`/`%th`.
fn period(v: i64, per_year: i64, letter: char) -> Option<String> {
    let y = 1960 + fdiv(v, per_year);
    if !(i64::from(YEAR_MIN)..=i64::from(YEAR_MAX)).contains(&y) {
        return None;
    }
    let idx = fmod(v, per_year) + 1;
    Some(format!("{y:04}{letter}{idx}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hinnant_round_trips() {
        for z in [-800_000i64, -719_468, -1, 0, 1, 18_000, 100_000, 800_000] {
            let (y, m, d) = civil_from_days(z);
            assert_eq!(days_from_civil(y, m, d), z, "z = {z}");
        }
        // The epoch offset itself.
        assert_eq!(days_from_civil(1960, 1, 1), STATA_EPOCH_DAYS);
        assert_eq!(days_from_civil(1970, 1, 1), 0);
    }

    #[test]
    fn measured_dates() {
        // semantics.log: `di %td 22000` -> 26mar2020, `di %td 0` -> 01jan1960.
        assert_eq!(fmt_td(0).as_deref(), Some("01jan1960"));
        assert_eq!(fmt_td(22_000).as_deref(), Some("26mar2020"));
        assert_eq!(fmt_td(365).as_deref(), Some("31dec1960"));
        assert_eq!(fmt_td(-1).as_deref(), Some("31dec1959"));
        assert_eq!(fmt_tc(0, false).as_deref(), Some("01jan1960 00:00:00"));
        assert_eq!(
            fmt_tc(1_000_000_000, false).as_deref(),
            Some("12jan1960 13:46:40")
        );
        assert_eq!(fmt_tc(-1, false).as_deref(), Some("31dec1959 23:59:59"));
    }

    #[test]
    fn a_leap_second_belongs_to_the_end_of_the_old_day() {
        // fmt_corpus: the first insertion is 30jun1972 23:59:60, and on the
        // %tC axis it BEGINS at midnight ending that day (no earlier leap has
        // accumulated yet). Subtracting only the fully elapsed ones lands on
        // 01jul1972 00:00:00, which then printed as `00:00:60` — a date that
        // does not exist. The corpus catches it; this catches it locally.
        let start = (days_from_civil(1972, 7, 1) - STATA_EPOCH_DAYS) * 86_400_000;
        assert_eq!(
            fmt_tc(start, true).as_deref(),
            Some("30jun1972 23:59:60"),
            "inside the leap second"
        );
        assert_eq!(
            fmt_tc(start + 999, true).as_deref(),
            Some("30jun1972 23:59:60")
        );
        assert_eq!(
            fmt_tc(start + 1000, true).as_deref(),
            Some("01jul1972 00:00:00")
        );
        assert_eq!(
            fmt_tc(start - 1, true).as_deref(),
            Some("30jun1972 23:59:59")
        );
        // %tc has no leap seconds at all: the same instant is plain midnight.
        assert_eq!(fmt_tc(start, false).as_deref(), Some("01jul1972 00:00:00"));
    }

    #[test]
    fn stata_weeks_are_fifty_seconds_of_a_year_not_seven_days() {
        assert_eq!(fmt_tw(0).as_deref(), Some("1960w1"));
        assert_eq!(fmt_tw(22_000).as_deref(), Some("2383w5"));
        assert_eq!(fmt_tw(-1).as_deref(), Some("1959w52"));
        assert_eq!(fmt_tm(-1).as_deref(), Some("1959m12"));
        assert_eq!(fmt_tm(-22_000).as_deref(), Some("0126m9"));
        assert_eq!(fmt_tq(-1).as_deref(), Some("1959q4"));
        assert_eq!(fmt_th(-1).as_deref(), Some("1959h2"));
        assert_eq!(fmt_th(365).as_deref(), Some("2142h2"));
    }

    #[test]
    fn out_of_range_is_none_so_the_caller_falls_back() {
        assert!(fmt_th(22_000).is_none());
        assert!(fmt_td(1_000_000_000).is_none());
        assert!(fmt_ty(1).is_none());
        assert_eq!(fmt_ty(365).as_deref(), Some("0365"));
    }
}
