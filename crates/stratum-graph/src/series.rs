//! The seam where a frame becomes a plot.
//!
//! This is the **only** module in the crate that knows `stratum-data` exists.
//! Everything downstream of it works on `Vec<f64>` and `Group`, which is what
//! lets the renderer be tested without building a `Frame` and what keeps the
//! rest of the crate honest about doing no data engine work of its own.
//!
//! Extraction goes through [`Column::gather_f64`], not through a hand-rolled
//! loop, for a reason worth stating: `Sample` keeps `All`/`Range` distinct from
//! `Mask` precisely so that a contiguous `if` copies run-by-run and never
//! touches an unselected row (`04` §5.1), and `gather_f64` is the code that
//! knows that. A `for obs in 0..nobs { if sample.contains(obs) }` here would
//! silently make every `graph … if` an O(nobs) membership test.

use crate::spec::Group;
use stratum_core::missing::is_missing;
use stratum_data::labels::ValueLabel;
use stratum_data::{Column, Sample};

/// One variable, restricted to the sample, in observation order.
///
/// Missing values are **kept**: the plot layer drops them, and it has to,
/// because the drop is pairwise across the layer's own variables. Removing them
/// here would misalign `y` against `x`.
#[must_use]
pub fn series(col: &Column, sample: &Sample) -> Vec<f64> {
    let mut out = Vec::new();
    col.gather_f64(sample, &mut out);
    out
}

/// Split `y` into `over(g)` categories.
///
/// Categories come out **ascending by level**, which is the order Stata lays
/// `over()` out in. Labels come from the level's value label when there is one;
/// otherwise the level is formatted as `%9.0g`, which is the only formatting
/// this crate does to a *category*, and it goes through `stratum_core::fmt` like
/// every other user-visible number (C12).
///
/// An observation missing on `g` is not in any category — Stata excludes it
/// rather than inventing a "missing" bar.
#[must_use]
pub fn groups(
    y: &Column,
    over: &Column,
    sample: &Sample,
    labels: Option<&ValueLabel>,
) -> Vec<Group> {
    let ys = series(y, sample);
    let gs = series(over, sample);

    // A Vec of (level, values) kept sorted by insertion rather than a HashMap:
    // `over()` has a handful of categories in every real use, a linear probe
    // over eight entries beats hashing a f64, and — the part that matters —
    // iteration order of a HashMap must never reach output (ARCHITECTURE §8.7's
    // sibling rule). This container cannot have that bug.
    let mut buckets: Vec<(f64, Vec<f64>)> = Vec::new();
    for (&g, &v) in gs.iter().zip(ys.iter()) {
        if is_missing(g) || !g.is_finite() {
            continue;
        }
        match buckets.iter_mut().find(|(level, _)| *level == g) {
            Some((_, vals)) => vals.push(v),
            None => buckets.push((g, vec![v])),
        }
    }
    buckets.sort_by(|a, b| a.0.total_cmp(&b.0));

    buckets
        .into_iter()
        .map(|(level, values)| Group {
            label: level_label(level, labels),
            values,
        })
        .collect()
}

fn level_label(level: f64, labels: Option<&ValueLabel>) -> String {
    labels.and_then(|t| t.get(level)).map_or_else(
        || stratum_core::fmt::fmt_g(level, 9).trim().to_owned(),
        str::to_owned,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use stratum_core::missing::SYSMISS;
    use stratum_data::column::NumCol;

    fn dbl(v: &[f64]) -> Column {
        Column::Double(NumCol::from_slice(v))
    }

    #[test]
    fn a_sample_restricts_the_series() {
        let c = dbl(&[1.0, 2.0, 3.0, 4.0]);
        assert_eq!(series(&c, &Sample::all(4)), vec![1.0, 2.0, 3.0, 4.0]);
        // `Sample::range` is half-open, so [1, 3) is observations 2 and 3.
        assert_eq!(series(&c, &Sample::range(4, 1, 3)), vec![2.0, 3.0]);
    }

    #[test]
    fn missing_survives_extraction_so_the_layer_can_drop_pairwise() {
        let c = dbl(&[1.0, SYSMISS, 3.0]);
        let s = series(&c, &Sample::all(3));
        assert_eq!(s.len(), 3);
        assert!(is_missing(s[1]));
    }

    #[test]
    fn groups_come_out_ascending_by_level() {
        let y = dbl(&[10.0, 20.0, 30.0, 40.0]);
        let g = dbl(&[2.0, 1.0, 2.0, 1.0]);
        let out = groups(&y, &g, &Sample::all(4), None);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].label, "1");
        assert_eq!(out[0].values, vec![20.0, 40.0]);
        assert_eq!(out[1].label, "2");
        assert_eq!(out[1].values, vec![10.0, 30.0]);
    }

    #[test]
    fn a_missing_group_is_excluded_not_bucketed() {
        let y = dbl(&[10.0, 20.0]);
        let g = dbl(&[1.0, SYSMISS]);
        let out = groups(&y, &g, &Sample::all(2), None);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].values, vec![10.0]);
    }

    #[test]
    fn value_labels_name_the_categories() {
        let mut table = ValueLabel::new();
        table.insert(0, "Domestic".to_owned());
        table.insert(1, "Foreign".to_owned());
        let y = dbl(&[1.0, 2.0]);
        let g = dbl(&[1.0, 0.0]);
        let out = groups(&y, &g, &Sample::all(2), Some(&table));
        assert_eq!(
            out.iter().map(|g| g.label.as_str()).collect::<Vec<_>>(),
            ["Domestic", "Foreign"]
        );
    }
}
