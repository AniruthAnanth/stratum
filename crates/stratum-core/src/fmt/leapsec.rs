//! The leap-second table `%tC` needs, hand-maintained (`04` §8).
//!
//! **Decision: no `chrono`, no `time`.** Both carry a timezone database we will
//! never use, and on Windows `chrono`'s local-time path makes OS calls, which
//! spec §28 forbids in the core runtime — and `time` is on the wasm-clean set's
//! banned list outright (ARCHITECTURE §8.4). The price is this file: twenty-odd
//! entries that change at most twice a year, announced by the IERS six months
//! ahead, and versioned with releases.
//!
//! `%tc` counts milliseconds since 1960-01-01 **ignoring** leap seconds; `%tC`
//! counts them. The two therefore drift apart by exactly the number of leap
//! seconds inserted before the instant being displayed — 16 s by 1991, 23 s by
//! 2007, 27 s today — which is measured in `tests/data/fmt_corpus.jsonl` and
//! not something to take on faith.

use super::datetime::{days_from_civil, STATA_EPOCH_DAYS};

/// The 27 leap seconds inserted since 1960, as `(year, month, day)` of the day
/// they were appended to. Each is inserted as `23:59:60` at the END of that
/// day, so the affected instant is midnight of the following day.
///
/// Source: IERS Bulletin C. Last verified 2026-08; the most recent insertion
/// was 2016-12-31 and none has been announced since.
pub const LEAP_DAYS: [(i32, u32, u32); 27] = [
    (1972, 6, 30),
    (1972, 12, 31),
    (1973, 12, 31),
    (1974, 12, 31),
    (1975, 12, 31),
    (1976, 12, 31),
    (1977, 12, 31),
    (1978, 12, 31),
    (1979, 12, 31),
    (1981, 6, 30),
    (1982, 6, 30),
    (1983, 6, 30),
    (1985, 6, 30),
    (1987, 12, 31),
    (1989, 12, 31),
    (1990, 12, 31),
    (1992, 6, 30),
    (1993, 6, 30),
    (1994, 6, 30),
    (1995, 12, 31),
    (1997, 6, 30),
    (1998, 12, 31),
    (2005, 12, 31),
    (2008, 12, 31),
    (2012, 6, 30),
    (2015, 6, 30),
    (2016, 12, 31),
];

/// The `%tC` millisecond at which each leap second BEGINS.
///
/// The `i * 1000` term is the accumulation: by the time the `i`-th leap second
/// arrives, the `%tC` clock is already `i` seconds ahead of the `%tc` clock, so
/// its position on the `%tC` axis has moved by that much.
static LEAP_TC_MS: [i64; 27] = build_leap_table();

const fn build_leap_table() -> [i64; 27] {
    let mut out = [0i64; 27];
    let mut i = 0usize;
    while i < 27 {
        let (y, m, d) = LEAP_DAYS[i];
        // Midnight ENDING that day, plus the seconds already inserted.
        // days_from_civil counts from 1970 and the %tC axis from 1960.
        let days = days_from_civil(y, m, d) + 1 - STATA_EPOCH_DAYS;
        out[i] = days * 86_400_000 + (i as i64) * 1000;
        i += 1;
    }
    out
}

/// How many leap seconds have fully elapsed by `%tC` millisecond `tc`.
#[must_use]
pub fn leaps_elapsed(tc: i64) -> i64 {
    let mut n = 0i64;
    let mut i = 0usize;
    while i < LEAP_TC_MS.len() {
        if tc >= LEAP_TC_MS[i] + 1000 {
            n += 1;
        } else {
            break;
        }
        i += 1;
    }
    n
}

/// True when `tc` lands inside a leap second, i.e. the displayed second is 60.
#[must_use]
pub fn inside_leap(tc: i64) -> bool {
    LEAP_TC_MS
        .iter()
        .any(|&start| tc >= start && tc < start + 1000)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_measured_offsets() {
        // fmt_corpus: %tc 1e12 is 09sep1991 01:46:40 and %tC 1e12 is
        // 01:46:24 — sixteen leap seconds by 1991.
        assert_eq!(leaps_elapsed(1_000_000_000_000), 16);
        // Twenty-three by 2007 (%tc 1.5e12 = 14jul2007 02:40:00 vs 02:39:37).
        assert_eq!(leaps_elapsed(1_500_000_000_000), 23);
        // None before 1972.
        assert_eq!(leaps_elapsed(0), 0);
        assert_eq!(leaps_elapsed(-1), 0);
        // All of them, far in the future.
        assert_eq!(leaps_elapsed(4_000_000_000_000), 27);
    }

    #[test]
    fn table_is_sorted_and_matches_iers() {
        for w in LEAP_TC_MS.windows(2) {
            assert!(w[0] < w[1]);
        }
        assert_eq!(LEAP_DAYS.len(), 27);
    }
}
