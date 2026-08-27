//! `L009` — `predict` after the estimation sample changed.
//!
//! Design 07 §6.3: "`predict` after the dataset changed since the estimation
//! that defined `e(sample)`". `predict` computes over whatever is in memory now;
//! `e(sample)` marks the observations the model was *fitted* on. Insert a
//! `drop`, a `merge` or a `replace` between the two and the two sets differ,
//! silently, with no error and no warning from Stata.
//!
//! The evidence is the modifying statement, not the `predict`, because that is
//! the line the user has to look at.

use stratum_proto::diagnostic::{Diagnostic, Related};

use super::dataflow::Doc;
use super::facts;
use crate::ParseIndex;

pub(super) fn check(idx: &ParseIndex<'_>, doc: &Doc<'_>, out: &mut Vec<Diagnostic>) {
    let _ = idx;
    // Statement index of the most recent estimation, and of every data-modifying
    // statement since.
    let mut last_estimation: Option<usize> = None;
    let mut modified_since: Vec<usize> = Vec::new();

    for (i, st) in doc.stmts.iter().enumerate() {
        let name = st.name();

        if name == "predict" || name == "predictnl" || name == "margins" {
            let (Some(est), false) = (last_estimation, modified_since.is_empty()) else {
                continue;
            };
            let est_name = doc.stmts.get(est).map_or("the estimation", |s| s.name());
            let mut d = super::finding(
                "L009",
                format!(
                    "`{name}` after the dataset changed since `{est_name}` — `e(sample)` no longer \
                     describes the observations in memory"
                ),
                st.span,
            );
            d.related = modified_since
                .iter()
                .filter_map(|j| doc.stmts.get(*j))
                .map(|m| Related {
                    span: m.span,
                    file: None,
                    message: format!("`{}` changed the data here", m.name()),
                })
                .collect();
            out.push(d);
            continue;
        }

        if facts::in_list(facts::ESTIMATION, name) || st.ast.is_estimation_head() {
            last_estimation = Some(i);
            modified_since.clear();
            continue;
        }
        if last_estimation.is_some() && facts::in_list(facts::MODIFIES_DATA, name) {
            // `sort` and `order` permute without changing membership, and
            // `e(sample)` is per-observation, so they are not a change of
            // sample. `format`/`label` are cosmetic. Excluded here rather than
            // from MODIFIES_DATA, which other checks read for a different
            // question.
            if !matches!(
                name,
                "sort" | "gsort" | "order" | "format" | "label" | "compress"
            ) {
                modified_since.push(i);
            }
        }
    }
}

/// `CommandAst` does not carry the region head's estimation bit, so the
/// canonical-name list above is the primary test; this catches a `by:`-prefixed
/// or otherwise-wrapped estimation whose head still resolves.
trait IsEstimationHead {
    fn is_estimation_head(&self) -> bool;
}

impl IsEstimationHead for stratum_parse::CommandAst {
    fn is_estimation_head(&self) -> bool {
        match &self.cmd {
            stratum_parse::ast::Command::Known(k) => facts::in_list(
                facts::ESTIMATION,
                stratum_parse::table().get(k.id).canonical,
            ),
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;
    use crate::lints::lint_document;
    use crate::Env;

    fn fires(src: &str) -> bool {
        let idx = ParseIndex::new(src);
        lint_document(&idx, &Env::default())
            .iter()
            .any(|d| d.code == "L009")
    }

    #[test]
    fn fires_when_a_drop_sits_between_the_model_and_the_prediction() {
        assert!(fires(
            "regress price mpg weight\ndrop if mpg < 15\npredict yhat\n"
        ));
    }

    #[test]
    fn silent_when_nothing_changed() {
        assert!(!fires("regress price mpg weight\npredict yhat\n"));
    }

    #[test]
    fn a_sort_is_not_a_change_of_sample() {
        assert!(!fires("regress price mpg\nsort mpg\npredict yhat\n"));
    }

    #[test]
    fn refitting_clears_the_finding() {
        assert!(!fires(
            "regress price mpg\ndrop if mpg < 15\nregress price mpg\npredict yhat\n"
        ));
    }

    #[test]
    fn the_evidence_points_at_the_modifying_line() {
        let src = "regress price mpg\ndrop if mpg < 15\npredict yhat\n";
        let idx = ParseIndex::new(src);
        let found = lint_document(&idx, &Env::default());
        let d = found.iter().find(|d| d.code == "L009").expect("L009");
        assert_eq!(d.related.len(), 1);
        let r = &d.related[0];
        let text = src.get(r.span.start as usize..r.span.end as usize).unwrap();
        assert!(text.starts_with("drop"), "{text:?}");
    }
}
