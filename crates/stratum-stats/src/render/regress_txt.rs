//! `regress` — 78 columns at `linesize 80`. `05` §§8.5, 8.6.
//!
//! Two headers, one coefficient block. The OLS header is the ANOVA table with a
//! six-row right block starting at column 51; the robust/cluster header has no
//! ANOVA block at all, five right-block rows starting at column 48, and a
//! `Robust` banner above the column titles.
//!
//! The one detail that looks like a typo and is not: the column title is
//! `Std. err.` under OLS and **`std. err.`** under robust and cluster. Both
//! spellings are in the goldens.

use stratum_core::fmt::{fmt_f, fmt_g};
use stratum_proto::result::StyledRun;

use super::{abbrev, Runs};
use crate::regress::{header_rows, header_rows_robust, RegressResult};

pub(crate) fn render(r: &RegressResult) -> Vec<StyledRun> {
    let mut o = Runs::new();

    // Notes come first, one per omitted column, in VARLIST order — verified:
    // `regress y c b a` with a and c collinear printed `c` then `a`.
    if !r.omitted_names.is_empty() {
        for name in &r.omitted_names {
            o.txt(&format!("note: {name} omitted because of collinearity."));
            o.nl();
        }
        o.nl();
    }

    if r.vce.is_robust() {
        robust_header(&mut o, r);
    } else {
        anova_header(&mut o, r);
    }
    o.nl();

    // Cluster slots one line between the blank and the top rule.
    if let (Some(g), Some(name)) = (r.vce.n_clust, r.vce.clustvar.as_deref()) {
        let plural = if g == 1 { "cluster" } else { "clusters" };
        let count = stratum_core::fmt::fmt_fc(g as f64, 12, 0);
        o.txt_r(
            &format!(
                "(Std. err. adjusted for {} {plural} in {name})",
                count.trim()
            ),
            78,
        );
        o.nl();
    }

    o.rule(78);
    o.nl();
    if r.vce.is_robust() {
        // The banner sits above the standard-error column and is right-trimmed
        // at 35 characters.
        o.txt("             |               Robust");
        o.nl();
    }
    coef_header(&mut o, r);
    o.rule_plus(13, 64);
    o.nl();
    for c in &r.coefs {
        coef_row(&mut o, r, c);
    }
    o.rule(78);
    o.nl();

    o.into_runs()
}

fn anova_header(o: &mut Runs, r: &RegressResult) {
    let a = r
        .anova
        .as_ref()
        .expect("the ANOVA block is present under OLS");
    let d = a.display();
    let right = header_rows(r);

    // The header's own inter-column spacing differs from the data rows', so the
    // first 51 columns are a literal.
    o.txt("      Source |       SS           df       MS      ");
    right_cell(o, &right[0], 16, 10);
    o.nl();

    o.rule_plus(13, 34);
    o.sp(3);
    right_cell(o, &right[1], 16, 10);
    o.nl();

    anova_row(o, "Model", &d[0], &d[1], &d[2]);
    right_cell(o, &right[2], 16, 10);
    o.nl();

    anova_row(o, "Residual", &d[3], &d[4], &d[5]);
    right_cell(o, &right[3], 16, 10);
    o.nl();

    o.rule_plus(13, 34);
    o.sp(3);
    right_cell(o, &right[4], 16, 10);
    o.nl();

    anova_row(o, "Total", &d[6], &d[7], &d[8]);
    right_cell(o, &right[5], 16, 10);
    o.nl();
}

fn anova_row(o: &mut Runs, label: &str, ss: &str, df: &str, ms: &str) {
    o.txt_r(label, 12);
    o.txt(" |");
    o.res_r(ss, 12);
    o.res_r(df, 10);
    o.res_r(ms, 12);
    o.sp(3);
}

fn right_cell(o: &mut Runs, (label, value): &(String, String), lw: usize, vw: usize) {
    o.txt_l(label, lw);
    o.txt("=");
    o.res_r(value, vw);
}

fn robust_header(o: &mut Runs, r: &RegressResult) {
    let right = header_rows_robust(r);
    o.txt("Linear regression");
    o.pad_to(48);
    right_cell(o, &right[0], 18, 11);
    o.nl();
    for row in &right[1..] {
        o.sp(48);
        right_cell(o, row, 18, 11);
        o.nl();
    }
}

fn coef_header(o: &mut Runs, r: &RegressResult) {
    o.txt_r(&abbrev(&r.depvar, 12), 12);
    o.txt(" |");
    o.txt(" Coefficient");
    o.txt(if r.vce.is_robust() {
        "  std. err."
    } else {
        "  Std. err."
    });
    o.txt("      t");
    o.txt("    P>|t|");
    if r.show_beta {
        o.txt_r("Beta", 25);
    } else {
        o.txt_r(&format!("[{}% conf. interval]", level_label(r.level)), 25);
    }
    o.nl();
}

fn coef_row(o: &mut Runs, r: &RegressResult, c: &crate::regress::Coef) {
    o.txt_r(&abbrev(&c.name, 12), 12);
    o.txt(" |");
    o.res_r(&fmt_g(c.b, 9), 11);
    if c.omitted {
        o.txt("  (omitted)");
        o.nl_trimmed();
        return;
    }
    o.res_r(&fmt_g(c.se, 9), 11);
    o.res_r(&fmt_f(c.t, 8, 2), 9);
    o.res_r(&fmt_f(c.p, 5, 3), 8);
    if r.show_beta {
        // `_cons` has no standardized coefficient; Stata prints `.`.
        let b = c.beta.unwrap_or(stratum_core::missing::SYSMISS);
        o.res_r(&fmt_g(b, 9), 25);
    } else {
        o.res_r(&fmt_g(c.ci_lo, 9), 13);
        o.res_r(&fmt_g(c.ci_hi, 9), 12);
    }
    o.nl();
}

/// `95` rather than `95.0000000`, and `97.5` when the user asks for it.
fn level_label(level: f64) -> String {
    if level == level.trunc() {
        format!("{}", level as i64)
    } else {
        fmt_g(level, 9).trim().to_owned()
    }
}
