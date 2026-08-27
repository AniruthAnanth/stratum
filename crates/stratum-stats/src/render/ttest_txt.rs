//! `ttest` — 78 columns. `05` §12.
//!
//! The footer is the fiddly part and every column in it was measured. The three
//! `Ha:` labels are centred on the three `Pr(...)` labels below them at columns
//! 10, 39 and 69, each starting at `centre − len/2`; that single rule reproduces
//! all three golden variants, whose alternatives differ in length by up to six
//! characters.

use stratum_core::fmt::{fmt_f, fmt_fc, fmt_g};
use stratum_proto::result::StyledRun;

use super::Runs;
use crate::ttest::{tail_str, TTestGroup, TTestKind, TTestResult};

const STUB: usize = 8;
/// Where each `Ha:` alternative is centred.
const HA_CENTRES: [usize; 3] = [10, 39, 69];

pub(crate) fn render(t: &TTestResult) -> Vec<StyledRun> {
    let mut o = Runs::new();
    o.txt(t.title);
    o.nl();
    o.rule(78);
    o.nl();
    o.txt_r(
        match t.kind {
            TTestKind::TwoSample { .. } => "Group",
            _ => "Variable",
        },
        STUB,
    );
    o.txt(" |");
    o.txt("     Obs        Mean    Std. err.   Std. dev.   [");
    o.txt(&level_label(t.level));
    o.txt("% conf. interval]");
    o.nl();
    o.rule_plus(9, 68);
    o.nl();

    let two_sample = matches!(t.kind, TTestKind::TwoSample { .. });
    for (i, g) in t.groups.iter().enumerate() {
        // The `Combined` row of a two-sample test is set off by a rule.
        if two_sample && i + 1 == t.groups.len() {
            o.rule_plus(9, 68);
            o.nl();
        }
        body_row(&mut o, g, true, true);
    }
    if let Some(d) = &t.diff {
        o.rule_plus(9, 68);
        o.nl();
        // Two-sample: the diff row has no Obs and no Std. dev. Paired: `diff`
        // is a real variable, so every cell is filled.
        body_row(&mut o, d, !two_sample, !two_sample);
    }
    o.rule(78);
    o.nl();

    footer(&mut o, t);
    o.into_runs()
}

fn body_row(o: &mut Runs, g: &TTestGroup, obs: bool, sd: bool) {
    o.txt_r(&g.label, STUB);
    o.txt(" |");
    if obs {
        o.res_r(&fmt_fc(g.n as f64, 7, 0), 8);
    } else {
        o.sp(8);
    }
    o.res_r(&fmt_g(g.mean, 9), 12);
    o.res_r(&fmt_g(g.se, 9), 12);
    if sd {
        o.res_r(&fmt_g(g.sd, 9), 12);
    } else {
        o.sp(12);
    }
    o.res_r(&fmt_g(g.ci_lo, 9), 12);
    o.res_r(&fmt_g(g.ci_hi, 9), 12);
    o.nl();
}

/// The statistic's name, the right-hand side of its definition, and the field
/// the two `=` signs are right-aligned in.
///
/// The field width is measured, not derived: the one- and two-sample forms
/// align at 8 and the paired form at 15.
fn lhs(t: &TTestResult) -> (String, String, String, usize) {
    match t.kind {
        TTestKind::OneSample { mu0 } => (
            "mean".to_owned(),
            format!("mean({})", t.varnames[0]),
            num_label(mu0),
            8,
        ),
        TTestKind::TwoSample { .. } => (
            "diff".to_owned(),
            format!("mean({}) - mean({})", t.varnames[0], t.varnames[1]),
            "0".to_owned(),
            8,
        ),
        TTestKind::Paired => (
            "mean(diff)".to_owned(),
            format!("mean({} - {})", t.varnames[0], t.varnames[1]),
            "0".to_owned(),
            15,
        ),
    }
}

fn footer(o: &mut Runs, t: &TTestResult) {
    let (name, rhs, h0, w) = lhs(t);

    o.txt_r(&name, w);
    o.txt(" = ");
    o.txt(&rhs);
    o.pad_to(78 - 12);
    o.txt("t =");
    o.res_r(&fmt_f(t.t, 9, 4), 9);
    o.nl();

    let (df_label, df_text) = match t.kind {
        TTestKind::TwoSample { unequal: true } => {
            ("Satterthwaite's degrees of freedom =", fmt_f(t.df, 9, 4))
        }
        _ => ("Degrees of freedom =", fmt_f(t.df, 9, 0)),
    };
    o.txt_r(&format!("H0: {name}"), w);
    o.txt(" = ");
    o.txt(&h0);
    o.pad_to(78 - df_label.len() - 9);
    o.txt(df_label);
    o.res_r(&df_text, 9);
    o.nl();

    o.nl();

    let alts = [
        format!("Ha: {name} < {h0}"),
        format!("Ha: {name} != {h0}"),
        format!("Ha: {name} > {h0}"),
    ];
    for (a, c) in alts.iter().zip(HA_CENTRES) {
        o.pad_to(c - a.chars().count() / 2);
        o.txt(a);
    }
    o.nl_trimmed();

    o.txt(" Pr(T < t) = ");
    o.res(&tail_str(t.p_l));
    o.sp(9);
    o.txt("Pr(|T| > |t|) = ");
    o.res(&tail_str(t.p));
    o.sp(10);
    o.txt("Pr(T > t) = ");
    o.res(&tail_str(t.p_u));
    o.nl();
}

/// `20` rather than `20.000000`, for the hypothesised mean in the `Ha:` labels.
fn num_label(x: f64) -> String {
    fmt_g(x, 18).trim().to_owned()
}

fn level_label(level: f64) -> String {
    if level == level.trunc() {
        format!("{}", level as i64)
    } else {
        fmt_g(level, 9).trim().to_owned()
    }
}
