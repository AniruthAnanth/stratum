//! `correlate` and `pwcorr` — `14 + 9k` columns. `05` §11.
//!
//! `correlate` prints `(obs=N)` and a blank line before the table and a blank
//! line after it; `pwcorr` prints neither, and its correlation rows carry one
//! trailing space that `correlate`'s do not.

use stratum_core::fmt::{fmt_f, fmt_fc};
use stratum_proto::result::StyledRun;

use super::{abbrev, Runs};
use crate::correlate::CorrResult;

const STUB: usize = 12;
const CELL: usize = 9;

pub(crate) fn render(c: &CorrResult) -> Vec<StyledRun> {
    let mut o = Runs::new();
    let k = c.names.len();

    if !c.pairwise {
        o.txt("(obs=");
        o.res(fmt_fc(c.n[0] as f64, 12, 0).trim());
        o.txt(")");
        o.nl();
        o.nl();
    }

    o.sp(STUB);
    o.txt(" |");
    for name in &c.names {
        o.txt_r(&abbrev(name, CELL - 1), CELL);
    }
    o.nl();
    o.rule_plus(13, CELL * k);
    o.nl();

    for i in 0..k {
        o.txt_r(&abbrev(&c.names[i], STUB), STUB);
        o.txt(" |");
        for j in 0..=i {
            o.res_r(&c.display_cell(c.at(i, j)), CELL);
        }
        if c.pairwise {
            // Measured: pwcorr's correlation rows end in one space, so that a
            // starred cell and an unstarred one occupy the same width.
            o.txt(" ");
            o.nl();
            sub_rows(&mut o, c, i);
        } else {
            o.nl_trimmed();
        }
    }

    if !c.pairwise {
        o.nl();
    }
    o.into_runs()
}

/// The `sig` / `obs` sub-rows plus the blank continuation row `pwcorr` always
/// writes when either is requested.
fn sub_rows(o: &mut Runs, c: &CorrResult, i: usize) {
    if c.p.is_none() && !c.show_obs {
        return;
    }
    if let Some(p) = &c.p {
        o.sp(STUB);
        o.txt(" |");
        for j in 0..i {
            o.res_r(&fmt_f(p[CorrResult::idx(i, j)], 9, 4), CELL);
        }
        o.nl_trimmed();
    }
    if c.show_obs {
        o.sp(STUB);
        o.txt(" |");
        for j in 0..i {
            o.res_r(&fmt_fc(c.n_at(i, j) as f64, 9, 0), CELL);
        }
        o.nl_trimmed();
    }
    o.sp(STUB);
    o.txt(" |");
    o.nl_trimmed();
}
