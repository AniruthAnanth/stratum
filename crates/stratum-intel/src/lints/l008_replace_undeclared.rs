//! `L008` — `replace` targets a variable this file never creates.
//!
//! Design 07 §6.3: "`replace` targeting a variable never generated in this file
//! and not present in the loaded dataset". The failure it catches is the one
//! that makes a do-file work on the author's machine and nowhere else: the
//! variable exists because something typed in the command bar an hour ago
//! created it.
//!
//! # Why it stays quiet more often than it fires
//!
//! The rule needs a *known* variable universe, and this crate cannot open a
//! `.dta` file to find one. So it fires only when [`Env::varnames`] is `Some` —
//! the caller genuinely knows the live layout — and the name is in neither the
//! live list nor the set this file created above the `replace`. With no live
//! layout the check is silent, because "I could not check" and "the variable is
//! missing" are different statements and only one of them belongs in a gutter.

use rustc_hash::FxHashSet;
use stratum_proto::diagnostic::{Confidence, Diagnostic};

use super::dataflow::{varlist_names, Doc};
use crate::{Env, ParseIndex};

/// Commands whose varlist slot *creates* the names in it.
const CREATORS: &[&str] = &["generate", "egen"];

pub(super) fn check(idx: &ParseIndex<'_>, env: &Env, doc: &Doc<'_>, out: &mut Vec<Diagnostic>) {
    let Some(live) = env.varnames.as_ref() else {
        return;
    };
    let live: FxHashSet<&str> = live.iter().map(String::as_str).collect();
    let mut created: FxHashSet<String> = FxHashSet::default();

    for st in &doc.stmts {
        let name = st.name();
        if CREATORS.contains(&name) {
            if let Some(v) = st.varlist() {
                created.extend(varlist_names(v));
            }
        }
        // `gen()`-style option targets, and `rename`'s new name.
        if let Some(t) = st.option_text("generate") {
            created.extend(t.split_whitespace().map(str::to_owned));
        }
        if name == "rename" {
            if let Some(rest) = st.rest() {
                created.extend(rest.split_whitespace().last().map(str::to_owned));
            }
        }
        // A command that reloads the dataset invalidates what we thought we
        // knew: after `use other.dta` neither the live layout nor the created
        // set describes the frame any more, so the check stands down.
        if matches!(
            name,
            "use" | "sysuse" | "import" | "webuse" | "frame" | "append"
        ) {
            return;
        }

        if name != "replace" {
            continue;
        }
        let Some(v) = st.varlist() else { continue };
        for target in varlist_names(v) {
            if created.contains(&target) || live.contains(target.as_str()) {
                continue;
            }
            let span = st.to_source(idx, v.span);
            let mut d = super::finding(
                "L008",
                format!(
                    "`replace {target}` — this file never creates `{target}`, and it is not in the loaded dataset"
                ),
                span,
            );
            d.offending_token = Some(target.clone());
            d.confidence = Confidence::Probable;
            // The nearest live name, when there is one. This is the same
            // machinery as r(111)'s "Did you mean 'income'?", reused: a
            // `replace` on a mistyped name is that error one keystroke earlier.
            let ranked = crate::similarity::rank(&target, live.iter().copied(), 3);
            if let Some(best) = ranked.first() {
                d.notes.push(format!("did you mean `{}`?", best.item));
            }
            out.push(d);
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;
    use crate::lints::lint_document;

    fn codes(src: &str, env: &Env) -> Vec<String> {
        let idx = ParseIndex::new(src);
        lint_document(&idx, env)
            .into_iter()
            .map(|d| d.code)
            .collect()
    }

    fn with_vars(names: &[&str]) -> Env {
        Env {
            varnames: Some(names.iter().map(|s| (*s).to_owned()).collect()),
            ..Env::default()
        }
    }

    #[test]
    fn fires_on_a_target_that_exists_nowhere() {
        let env = with_vars(&["price", "mpg"]);
        assert!(codes("replace weightt = 1\n", &env).contains(&"L008".to_owned()));
    }

    #[test]
    fn silent_when_the_file_created_the_variable() {
        let env = with_vars(&["price"]);
        let src = "generate ln_price = ln(price)\nreplace ln_price = 0 if price == 0\n";
        assert!(!codes(src, &env).contains(&"L008".to_owned()));
    }

    #[test]
    fn silent_when_the_variable_is_live() {
        let env = with_vars(&["price", "mpg"]);
        assert!(!codes("replace price = 1\n", &env).contains(&"L008".to_owned()));
    }

    #[test]
    fn silent_with_no_live_layout_at_all() {
        // The honesty rule: no dataset means "I could not check", not "missing".
        assert!(!codes("replace anything = 1\n", &Env::default()).contains(&"L008".to_owned()));
    }

    #[test]
    fn stands_down_after_the_frame_is_reloaded() {
        let env = with_vars(&["price"]);
        let src = "use other.dta, clear\nreplace whatever = 1\n";
        assert!(!codes(src, &env).contains(&"L008".to_owned()));
    }

    #[test]
    fn offers_the_nearest_live_name() {
        let env = with_vars(&["income", "price"]);
        let idx = ParseIndex::new("replace incom = 1\n");
        let found = lint_document(&idx, &env);
        let d = found.iter().find(|d| d.code == "L008").expect("L008 fires");
        assert!(
            d.notes.iter().any(|n| n.contains("income")),
            "{:?}",
            d.notes
        );
    }
}
