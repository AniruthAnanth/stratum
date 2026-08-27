//! `tabulate` — one-way at 48 columns, two-way at 78/79. `05` §§10.1, 10.2.
//!
//! F13's exception lives here: two-way **data** rows carry exactly one trailing
//! space and the header, rules and χ² line do not. That single byte is the
//! difference between 78 and 79 columns and it is in the golden.

use stratum_core::fmt::fmt_f;
use stratum_proto::result::StyledRun;

use super::{centre_lead, centre_lead_left, wrap, Runs};
use crate::tabulate::{freq_str, OneWayTab, TwoWayTab};

// One-way geometry.
const STUB1: usize = 11;
// Two-way geometry: a 10-wide cell plus one space, per column.
const STUB2: usize = 10;
const CELL: usize = 10;

pub(crate) fn render_oneway(t: &OneWayTab) -> Vec<StyledRun> {
    let mut o = Runs::new();
    let title = if t.label.is_empty() {
        t.var.as_str()
    } else {
        t.label.as_str()
    };
    let stub = wrap(title, STUB1);
    for line in &stub[..stub.len() - 1] {
        o.txt_r(line, STUB1);
        o.txt(" |");
        o.nl_trimmed();
    }
    o.txt_r(&stub[stub.len() - 1], STUB1);
    o.txt(" |");
    o.txt("      Freq.     Percent        Cum.");
    o.nl();
    o.rule_plus(12, 35);
    o.nl();

    let pcts = t.percents();
    let last = t.cells.len() - 1;
    for (i, (_, _, f)) in t.cells.iter().enumerate() {
        o.txt_r(&t.row_header(i), STUB1);
        o.txt(" |");
        o.res_r(&freq_str(*f, 10), 11);
        o.res_r(&fmt_f(pcts[i].0, 11, 2), 12);
        // The last cumulative is forced to exactly 100.00 rather than left to
        // the accumulation's last ulp.
        let cum = if i == last { 100.0 } else { pcts[i].1 };
        o.res_r(&fmt_f(cum, 11, 2), 12);
        o.nl();
    }

    o.rule_plus(12, 35);
    o.nl();
    o.txt_r("Total", STUB1);
    o.txt(" |");
    o.res_r(&freq_str(t.n, 10), 11);
    o.res_r(&fmt_f(100.0, 11, 2), 12);
    o.nl_trimmed();
    o.into_runs()
}

pub(crate) fn render_twoway(t: &TwoWayTab) -> Vec<StyledRun> {
    let mut o = Runs::new();
    let stats = t.show.list();
    let nc = t.col_keys.len();
    let band = nc * (CELL + 1);

    if t.show.needs_key() {
        key_box(&mut o, t);
        o.nl();
    }

    // The column-variable label, centred over the data band.
    let ctitle = if t.col_label.is_empty() {
        t.col_var.as_str()
    } else {
        t.col_label.as_str()
    };
    o.sp(STUB2 + 1);
    o.txt("|");
    o.sp(centre_lead(band, ctitle.chars().count()));
    o.txt(ctitle);
    o.nl_trimmed();

    // The row-variable label in the stub, then the column headers.
    let rtitle = if t.row_label.is_empty() {
        t.row_var.as_str()
    } else {
        t.row_label.as_str()
    };
    let stub = wrap(rtitle, STUB2);
    for line in &stub[..stub.len() - 1] {
        o.txt_r(line, STUB2);
        o.txt(" |");
        o.nl_trimmed();
    }
    o.txt_r(&stub[stub.len() - 1], STUB2);
    o.txt(" |");
    for j in 0..nc {
        o.txt_r(&t.col_header(j), CELL);
        o.txt(" ");
    }
    o.txt("|");
    // The header's Total carries NO trailing space; every data row's does.
    o.txt_r("Total", CELL);
    o.nl();
    rule(&mut o, nc);

    let nf = t.n as f64;
    for i in 0..t.row_keys.len() {
        if i > 0 && stats.len() > 1 {
            rule(&mut o, nc);
        }
        for (s, stat) in stats.iter().enumerate() {
            if s == 0 {
                o.txt_r(&t.row_header(i), STUB2);
            } else {
                o.sp(STUB2);
            }
            o.txt(" |");
            for j in 0..nc {
                cell(
                    &mut o,
                    *stat,
                    t.at(i, j) as f64,
                    t.row_tot[i] as f64,
                    t.col_tot[j] as f64,
                    nf,
                );
            }
            o.txt("|");
            cell(
                &mut o,
                *stat,
                t.row_tot[i] as f64,
                t.row_tot[i] as f64,
                nf,
                nf,
            );
            o.nl();
        }
    }

    rule(&mut o, nc);
    for (s, stat) in stats.iter().enumerate() {
        if s == 0 {
            o.txt_r("Total", STUB2);
        } else {
            o.sp(STUB2);
        }
        o.txt(" |");
        for j in 0..nc {
            cell(
                &mut o,
                *stat,
                t.col_tot[j] as f64,
                nf,
                t.col_tot[j] as f64,
                nf,
            );
        }
        o.txt("|");
        cell(&mut o, *stat, nf, nf, nf, nf);
        o.nl();
    }

    if let Some(c) = t.chi2 {
        o.nl();
        o.sp(10);
        o.txt(&format!("Pearson chi2({}) =", c.df));
        o.res_r(&fmt_f(c.stat, 9, 4), 9);
        o.txt("   Pr = ");
        o.res_r(&fmt_f(c.p, 5, 3), 5);
        o.nl();
    }
    o.into_runs()
}

fn rule(o: &mut Runs, nc: usize) {
    o.rule(STUB2 + 1);
    o.txt("+");
    o.rule(nc * (CELL + 1));
    o.txt("+");
    o.rule(CELL);
    o.nl();
}

/// One cell of a two-way table, followed by the trailing space F13 records.
fn cell(
    o: &mut Runs,
    stat: stratum_proto::result::CellStat,
    v: f64,
    row_tot: f64,
    col_tot: f64,
    n: f64,
) {
    use stratum_proto::result::CellStat as S;
    let text = match stat {
        S::Freq => freq_str(v as u64, 9),
        S::RowPct => fmt_f(100.0 * v / row_tot, 9, 2),
        S::ColPct => fmt_f(100.0 * v / col_tot, 9, 2),
        S::CellPct | S::Expected => fmt_f(100.0 * v / n, 9, 2),
    };
    o.res_r(&text, CELL);
    o.txt(" ");
}

/// The `Key` box, listing only the requested statistics and always in Stata's
/// order.
fn key_box(o: &mut Runs, t: &TwoWayTab) {
    const W: usize = 19;
    let mut entries: Vec<&str> = Vec::new();
    if t.show.freq {
        entries.push("frequency");
    }
    if t.show.row {
        entries.push("row percentage");
    }
    if t.show.col {
        entries.push("column percentage");
    }
    if t.show.cell {
        entries.push("cell percentage");
    }

    o.txt("+");
    o.rule(W);
    o.txt("+");
    o.nl();
    o.txt("|");
    o.txt(" Key");
    o.sp(W - 4);
    o.txt("|");
    o.nl();
    o.txt("|");
    o.rule(W);
    o.txt("|");
    o.nl();
    for e in entries {
        o.txt("|");
        let lead = centre_lead_left(W, e.chars().count());
        o.sp(lead);
        o.txt(e);
        o.sp(W - lead - e.chars().count());
        o.txt("|");
        o.nl();
    }
    o.txt("+");
    o.rule(W);
    o.txt("+");
    o.nl();
}
