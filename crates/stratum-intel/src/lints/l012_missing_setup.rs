//! `L012` — command requires `tsset`, `xtset` or `svyset`.
//!
//! Design 07 §6.3: "command requiring `tsset`/`xtset`/`svyset` with no such
//! statement earlier in the file". Stata's own error for this is r(111) on a
//! variable the user never wrote, which is one of the least legible messages in
//! the product; catching it in the editor turns it into a sentence.
//!
//! A time-series *operator* — `L.gnp`, `D2.x`, `F.y`, `S.z` — carries the same
//! requirement as a `tsset`-needing command, so it fires the same rule.

use stratum_parse::ast::{VarItemKind, VarList};
use stratum_proto::diagnostic::{Diagnostic, Suggestion, SuggestionKind};
use stratum_proto::{Edit, Span};

use super::dataflow::Doc;
use super::facts;
use crate::ParseIndex;

/// Which declaration a statement is missing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Needs {
    Tsset,
    Xtset,
    Svyset,
}

impl Needs {
    const fn word(self) -> &'static str {
        match self {
            Needs::Tsset => "tsset",
            Needs::Xtset => "xtset",
            Needs::Svyset => "svyset",
        }
    }
}

pub(super) fn check(idx: &ParseIndex<'_>, doc: &Doc<'_>, out: &mut Vec<Diagnostic>) {
    let mut tsset = false;
    let mut xtset = false;
    let mut svyset = false;

    for st in &doc.stmts {
        let name = st.name();
        match name {
            // `xtset` declares a panel, which is also a time-series declaration.
            "xtset" => {
                xtset = true;
                tsset = true;
                continue;
            }
            "tsset" => {
                tsset = true;
                continue;
            }
            "svyset" => {
                svyset = true;
                continue;
            }
            _ => {}
        }

        let need = if facts::in_list(facts::NEEDS_XTSET, name) && name != "xtset" {
            (!xtset).then_some(Needs::Xtset)
        } else if facts::in_list(facts::NEEDS_SVYSET, name) || st.has_prefix("svy") {
            (!svyset).then_some(Needs::Svyset)
        } else if facts::in_list(facts::NEEDS_TSSET, name)
            || st.varlist().is_some_and(has_ts_operator)
        {
            (!tsset).then_some(Needs::Tsset)
        } else {
            None
        };
        let Some(need) = need else { continue };

        let why = if facts::in_list(facts::NEEDS_TSSET, name)
            || facts::in_list(facts::NEEDS_XTSET, name)
            || facts::in_list(facts::NEEDS_SVYSET, name)
            || st.has_prefix("svy")
        {
            format!("`{name}` requires `{}`", need.word())
        } else {
            format!(
                "a time-series operator in this varlist requires `{}`",
                need.word()
            )
        };
        let d = super::finding(
            "L012",
            format!(
                "{why}, and no `{}` appears earlier in this file",
                need.word()
            ),
            st.span,
        );
        // The fix inserts the declaration at the top of the file with the
        // variables left as a placeholder: we cannot know the panel and time
        // keys, and inventing them would be worse than an obvious hole.
        out.push(super::with_fix(
            d,
            Suggestion {
                label: format!("Insert `{}` at the top of the file", need.word()),
                kind: SuggestionKind::InsertLine,
                edits: vec![Edit {
                    span: Span { start: 0, end: 0 },
                    text: match need {
                        Needs::Tsset => "tsset timevar\n".to_owned(),
                        Needs::Xtset => "xtset panelvar timevar\n".to_owned(),
                        Needs::Svyset => "svyset psu [pweight=w], strata(stratum)\n".to_owned(),
                    },
                }],
            },
        ));
    }
    let _ = idx;
}

/// Any atom carrying `L.`, `F.`, `D.` or `S.`.
fn has_ts_operator(v: &VarList) -> bool {
    v.items.iter().any(|item| match &item.kind {
        VarItemKind::Single(a) => a.ts.is_some(),
        VarItemKind::Interact { atoms, .. } => atoms.iter().any(|a| a.ts.is_some()),
    })
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
            .any(|d| d.code == "L012")
    }

    #[test]
    fn fires_on_an_undeclared_panel_command() {
        assert!(fires("xtreg y x, fe\n"));
    }

    #[test]
    fn silent_once_declared() {
        assert!(!fires("xtset id year\nxtreg y x, fe\n"));
    }

    #[test]
    fn xtset_also_satisfies_a_time_series_command() {
        assert!(!fires("xtset id year\ndfuller y\n"));
    }

    #[test]
    fn a_lag_operator_needs_tsset_too() {
        assert!(fires("regress y L.y\n"));
        assert!(!fires("tsset t\nregress y L.y\n"));
    }

    #[test]
    fn the_fix_inserts_at_the_top() {
        let idx = ParseIndex::new("xtreg y x, fe\n");
        let found = lint_document(&idx, &Env::default());
        let d = found.iter().find(|d| d.code == "L012").expect("L012");
        let s = d.suggestions.first().expect("a fix");
        assert_eq!(s.edits[0].span, Span { start: 0, end: 0 });
        assert!(s.edits[0].text.starts_with("xtset "));
    }
}
