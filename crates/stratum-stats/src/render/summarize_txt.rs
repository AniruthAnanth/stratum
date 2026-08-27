//! `summarize` — 71 columns — and `summarize, detail` — 61. `05` §§7.3, 7.4.

use stratum_core::fmt::{fmt_fc, fmt_g};
use stratum_proto::result::StyledRun;

use super::{abbrev, centre_lead, Runs};
use crate::summarize::{SummarizeDetail, SummarizeResult, SummarizeVar, PERCENTILES};

/// The header is a literal, not five right-aligned cells: Stata's inter-column
/// spacing in the header differs from its data rows' (`Std. dev.` sits three
/// columns left of where a right-aligned 12-wide cell would put it).
const HEADER: &str = "    Variable |        Obs        Mean    Std. dev.       Min        Max";

/// Stata breaks the table with a stub rule every five variables.
const GROUP: usize = 5;

pub(crate) fn render(r: &SummarizeResult) -> Vec<StyledRun> {
    let mut o = Runs::new();
    if r.meanonly {
        // `summarize, meanonly` computes and prints nothing.
        return o.into_runs();
    }
    if r.detail {
        for (i, v) in r.vars.iter().enumerate() {
            if i > 0 {
                o.nl();
            }
            detail_block(&mut o, v);
        }
        return o.into_runs();
    }

    o.txt(HEADER);
    o.nl();
    o.rule_plus(13, 57);
    o.nl();
    for (i, v) in r.vars.iter().enumerate() {
        if i > 0 && i % GROUP == 0 {
            o.rule_plus(13, 57);
            o.nl();
        }
        row(&mut o, v);
    }
    o.into_runs()
}

fn row(o: &mut Runs, v: &SummarizeVar) {
    o.txt_r(&abbrev(&v.name, 12), 12);
    o.txt(" |");
    o.res_r(&fmt_fc(v.n as f64, 10, 0), 11);
    if v.n == 0 {
        // A string variable, or one with no non-missing observation in the
        // sample: Stata prints the count and stops (`make` in
        // core_surface.log).
        o.nl_trimmed();
        return;
    }
    o.res_r(&fmt_g(v.mean, 9), 12);
    o.res_r(&fmt_g(v.sd, 9), 12);
    o.res_r(&fmt_g(v.min, 9), 11);
    o.res_r(&fmt_g(v.max, 9), 11);
    o.nl();
}

/// Which percentile the `detail` row at index `row` carries.
fn percentile_index(row: usize) -> usize {
    match row {
        1..=4 => row - 1, // p1 p5 p10 p25
        6 => 4,           // p50
        // Rows 5 and 7 carry no percentile — the blank row and the `Largest`
        // banner — so the index falls three behind the row, not four.
        8..=11 => row - 3, // p75 p90 p95 p99
        _ => unreachable!("row {row} carries no percentile"),
    }
}

/// Which of the four extremes the `detail` row at index `row` carries.
fn extreme(d: &SummarizeDetail, row: usize) -> Option<f64> {
    match row {
        1..=4 => Some(d.smallest4[row - 1]),
        8..=11 => Some(d.largest4[row - 8]),
        _ => None,
    }
}

/// The right-hand block: a label left-aligned in 14 and a value right-aligned
/// in 9, on seven of the twelve rows.
struct Side<'a>(&'a str, String);

fn detail_block(o: &mut Runs, v: &SummarizeVar) {
    let title = if v.label.is_empty() {
        v.name.as_str()
    } else {
        v.label.as_str()
    };
    o.sp(centre_lead(61, title.chars().count()));
    o.txt(title);
    o.nl_trimmed();
    o.rule(61);
    o.nl();

    let d = v
        .detail
        .as_ref()
        .expect("detail rendering requires the detail moments");

    let side: [Option<Side<'_>>; 12] = [
        None,
        None,
        None,
        Some(Side("Obs", fmt_fc(v.n as f64, 9, 0))),
        Some(Side("Sum of wgt.", fmt_fc(v.sum_w, 9, 0))),
        None,
        Some(Side("Mean", fmt_g(v.mean, 9))),
        Some(Side("Std. dev.", fmt_g(v.sd, 9))),
        None,
        Some(Side("Variance", fmt_g(v.var, 9))),
        Some(Side("Skewness", fmt_g(d.skewness, 9))),
        Some(Side("Kurtosis", fmt_g(d.kurtosis, 9))),
    ];

    for (i, s) in side.iter().enumerate() {
        match i {
            0 => o.txt("      Percentiles      Smallest"),
            5 => {
                // The row between p25 and p50 is entirely empty.
                o.nl();
                continue;
            }
            7 => {
                // The `Largest` banner replaces the percentile and smallest
                // cells; it is right-aligned in the 15-wide extremes column.
                o.sp(16);
                o.txt_r("Largest", 15);
            }
            _ => {
                let pi = percentile_index(i);
                o.txt(&format!("{:2}%", PERCENTILES[pi]));
                o.res_r(&fmt_g(d.percentiles[pi], 9), 13);
                match extreme(d, i) {
                    Some(x) => o.res_r(&fmt_g(x, 9), 15),
                    None => o.sp(15),
                }
            }
        }
        match s {
            Some(Side(label, value)) => {
                o.sp(7);
                o.txt_l(label, 14);
                o.res_r(value, 9);
                o.nl();
            }
            None => o.nl_trimmed(),
        }
    }
}
