//! The byte-exact classic renderers, and the run builder they share.
//!
//! Every layout in here was measured off `tests/golden/stata18/*.log` with a
//! column ruler, not inferred from the manuals. Where a width looks arbitrary
//! it is because Stata's is: the `summarize` header is a literal because its
//! inter-column spacing is not the same as its data rows', and the two-way
//! `tabulate` data rows carry exactly one trailing space that the header does
//! not (F13).
//!
//! # Styling (A12)
//!
//! [`Runs::res`] marks a computed number, [`Runs::txt`] marks everything else.
//! The distinction is what lets the Classic pane print result values in Stata's
//! ink, and `tests/styled_runs.rs` pins the boundaries against committed
//! `.runs.json` so a style regression is exactly as loud as a spacing one.

pub(crate) mod correlate_txt;
pub(crate) mod regress_txt;
pub(crate) mod summarize_txt;
pub(crate) mod tabulate_txt;
pub(crate) mod ttest_txt;

use stratum_proto::result::{StyleId, StyledRun};

/// A styled-run accumulator that merges adjacent same-style text.
///
/// Merging is not cosmetic: without it every table would emit one run per cell
/// per line, and the `.runs.json` goldens would encode the renderer's call
/// sequence rather than its visible structure.
#[derive(Default, Debug)]
pub(crate) struct Runs {
    runs: Vec<StyledRun>,
    /// Characters written since the last newline, so `pad_to` and the
    /// right-trimming rules can be expressed in columns.
    col: usize,
}

impl Runs {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    fn push(&mut self, text: &str, style: StyleId) {
        if text.is_empty() {
            return;
        }
        self.col += text.chars().count();
        match self.runs.last_mut() {
            Some(last) if last.style == style => last.text.push_str(text),
            _ => self.runs.push(StyledRun {
                text: text.to_owned(),
                style,
            }),
        }
    }

    /// Literal text: labels, headers, rules, padding.
    pub(crate) fn txt(&mut self, s: &str) {
        self.push(s, StyleId::Text);
    }

    /// A computed number, exactly as `stratum_core::fmt` produced it.
    ///
    /// `stratum_core::fmt`'s formatters return a **fixed-width** string — a
    /// `%10.0fc` of 74 is nine spaces and `74` — so the padding arrives
    /// attached to the value. The convention this crate publishes (see the
    /// crate docs) is that `Result` covers the characters of the number and
    /// never its padding, because that is what lets the Classic pane ink the
    /// value without inking the gutter around it. Splitting here rather than at
    /// every call site keeps the rule true by construction: a renderer cannot
    /// forget to trim. The bytes are unchanged either way — the spaces are
    /// still emitted, just as `Text`.
    pub(crate) fn res(&mut self, s: &str) {
        let body = s.trim_matches(' ');
        if body.is_empty() {
            self.push(s, StyleId::Text);
            return;
        }
        let lead = s.len() - s.trim_start_matches(' ').len();
        self.push(&s[..lead], StyleId::Text);
        self.push(body, StyleId::Result);
        self.push(&s[lead + body.len()..], StyleId::Text);
    }

    /// `n` spaces of padding.
    pub(crate) fn sp(&mut self, n: usize) {
        if n > 0 {
            self.txt(&" ".repeat(n));
        }
    }

    /// Pad with spaces until the cursor sits at column `c`.
    pub(crate) fn pad_to(&mut self, c: usize) {
        if c > self.col {
            self.sp(c - self.col);
        }
    }

    /// A right-aligned literal cell of width `w`.
    pub(crate) fn txt_r(&mut self, s: &str, w: usize) {
        let n = s.chars().count();
        self.sp(w.saturating_sub(n));
        self.txt(s);
    }

    /// A left-aligned literal cell of width `w`.
    pub(crate) fn txt_l(&mut self, s: &str, w: usize) {
        let n = s.chars().count();
        self.txt(s);
        self.sp(w.saturating_sub(n));
    }

    /// A right-aligned **result** cell of width `w`. `s` is already formatted;
    /// only the padding is `Text`.
    pub(crate) fn res_r(&mut self, s: &str, w: usize) {
        let n = s.chars().count();
        self.sp(w.saturating_sub(n));
        self.res(s);
    }

    /// End the line.
    pub(crate) fn nl(&mut self) {
        self.push("\n", StyleId::Text);
        self.col = 0;
    }

    /// Drop trailing spaces written since the last newline, then end the line.
    ///
    /// F13: every classic table is emitted with no trailing whitespace — except
    /// two-way `tabulate` data rows, which is why this is a call and not a
    /// blanket rule.
    pub(crate) fn nl_trimmed(&mut self) {
        while let Some(last) = self.runs.last_mut() {
            let trimmed = last.text.trim_end_matches(' ');
            if trimmed.len() == last.text.len() {
                break;
            }
            let dropped = last.text.len() - trimmed.len();
            last.text.truncate(trimmed.len());
            self.col -= dropped;
            if last.text.is_empty() {
                self.runs.pop();
            } else {
                break;
            }
        }
        self.nl();
    }

    /// A rule of `n` hyphens.
    pub(crate) fn rule(&mut self, n: usize) {
        self.txt(&"-".repeat(n));
    }

    /// `left` hyphens, a `+`, then `right` hyphens — the stub separator every
    /// table in `05` uses.
    pub(crate) fn rule_plus(&mut self, left: usize, right: usize) {
        self.rule(left);
        self.txt("+");
        self.rule(right);
    }

    pub(crate) fn into_runs(self) -> Vec<StyledRun> {
        self.runs
    }
}

/// Abbreviate a name to `w` columns the way Stata's stub does: keep the first
/// `w - 1` characters and append `~`.
///
/// Stata abbreviates in the middle for some commands and at the end for the
/// `summarize`/`regress` stub; the stub form is what the goldens show
/// (`displacement` is exactly 12 and survives whole).
pub(crate) fn abbrev(name: &str, w: usize) -> String {
    if name.chars().count() <= w {
        return name.to_owned();
    }
    let mut s: String = name.chars().take(w.saturating_sub(1)).collect();
    s.push('~');
    s
}

/// Greedy word wrap to `w` columns, used by the `tabulate` stub header.
///
/// A word longer than `w` is hard-split rather than allowed to overflow the
/// stub, because the `|` must stay in column 11.
pub(crate) fn wrap(text: &str, w: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut cur = String::new();
    for word in text.split_whitespace() {
        let mut word = word;
        loop {
            let wl = word.chars().count();
            if cur.is_empty() {
                if wl <= w {
                    cur.push_str(word);
                    break;
                }
                let head: String = word.chars().take(w).collect();
                let tail: usize = head.len();
                lines.push(head);
                word = &word[tail..];
                continue;
            }
            if cur.chars().count() + 1 + wl <= w {
                cur.push(' ');
                cur.push_str(word);
                break;
            }
            lines.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

/// Centre `s` in `w` columns, biased right when the slack is odd.
///
/// Measured: the two-way `tabulate` column band is 55 wide and
/// `Repair record 1978` (18) starts 19 columns in, not 18
/// (`tests/golden/stata18/core_surface.log`).
pub(crate) fn centre_lead(w: usize, len: usize) -> usize {
    w.saturating_sub(len).div_ceil(2)
}

/// Centre `s` in `w` columns, biased left when the slack is odd — the `Key` box
/// of `tabulate, row col`, where `row percentage` (14) sits 2 in and 3 out.
pub(crate) fn centre_lead_left(w: usize, len: usize) -> usize {
    w.saturating_sub(len) / 2
}
