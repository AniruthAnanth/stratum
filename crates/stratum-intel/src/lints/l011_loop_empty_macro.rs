//! `L011` — loop over a macro that can be empty.
//!
//! Design 07 §6.3: "`foreach`/`forvalues` over a macro that can be empty on some
//! path". An empty macro makes the body execute zero times. Stata reports
//! nothing: no error, no warning, no output. The analysis that was supposed to
//! run over twelve outcomes runs over none, and the log looks fine.
//!
//! # What "can be empty on some path" means here
//!
//! Path-sensitivity is approximated in the only direction that does not produce
//! noise:
//!
//! * The macro is never assigned anywhere above the loop → **fires**. Either it
//!   is inherited from the session (which is `R006`'s hidden-interactive-
//!   dependency failure showing up as a silent no-op) or it is a typo.
//! * The macro is assigned only inside a deeper brace scope than the loop — a
//!   conditional or another loop — → **fires**. That assignment does not
//!   dominate the loop.
//! * The macro is assigned at or above the loop's scope with a non-empty
//!   right-hand side → silent.
//! * The macro is assigned at or above the loop's scope with an *empty*
//!   right-hand side and never re-assigned → **fires**, because `local x` is
//!   how you deliberately empty a macro and then forget to fill it.

use rustc_hash::FxHashMap;
use stratum_parse::ast::{BlockCommand, Command, ForeachSource};
use stratum_proto::diagnostic::{Confidence, Diagnostic};

use super::dataflow::Doc;
use crate::{Env, ParseIndex};

/// How a macro was last assigned above the point of use.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Assigned {
    /// Assigned at a scope that dominates the loop, with something in it.
    DominatingNonEmpty,
    /// Assigned only inside a deeper scope.
    ConditionalOnly,
    /// Assigned with nothing on the right-hand side.
    Empty,
}

pub(super) fn check(idx: &ParseIndex<'_>, env: &Env, doc: &Doc<'_>, out: &mut Vec<Diagnostic>) {
    let mut state: FxHashMap<String, Assigned> = FxHashMap::default();

    for st in &doc.stmts {
        // Record assignments first: `local x "a b"` on the line above the loop
        // does dominate it.
        if let Some((name, empty)) = assignment(st.name(), st.rest()) {
            let how = if empty {
                Assigned::Empty
            } else if st.depth == 0 {
                Assigned::DominatingNonEmpty
            } else {
                Assigned::ConditionalOnly
            };
            // A dominating assignment wins over a conditional one; a later
            // conditional assignment does not weaken an earlier dominating one.
            let slot = state.entry(name).or_insert(how);
            if *slot != Assigned::DominatingNonEmpty {
                *slot = how;
            }
            continue;
        }

        let Command::Block(b) = &st.ast.cmd else {
            continue;
        };
        let (macro_name, kind) = match b.as_ref() {
            BlockCommand::Foreach { source, .. } => match source {
                ForeachSource::OfLocal(n) => (n.clone(), "local"),
                ForeachSource::OfGlobal(n) => (n.clone(), "global"),
                ForeachSource::In(raw) => match sole_macro_ref(&raw.text) {
                    Some((n, k)) => (n, k),
                    None => continue,
                },
                _ => continue,
            },
            _ => continue,
        };

        // A macro the session already holds is not a finding here: the caller
        // told us it exists, and `R006` owns the "why does it exist" question.
        let live = if kind == "local" {
            &env.locals
        } else {
            &env.globals
        };
        if live.contains(&macro_name) {
            continue;
        }

        let why = match state.get(&macro_name) {
            Some(Assigned::DominatingNonEmpty) => continue,
            Some(Assigned::ConditionalOnly) => {
                format!("`{macro_name}` is only assigned inside a conditional above this loop")
            }
            Some(Assigned::Empty) => format!("`{macro_name}` was assigned an empty value"),
            None => format!("`{macro_name}` is never assigned in this file"),
        };
        let mut d = super::finding(
            "L011",
            format!("this loop runs zero times when `{macro_name}` is empty — {why}"),
            st.span,
        );
        d.offending_token = Some(macro_name);
        d.confidence = Confidence::Probable;
        let _ = idx;
        out.push(d);
    }
}

/// `local x …` / `global x …` → `(name, right-hand side is empty)`.
fn assignment(cmd: &str, rest: Option<&str>) -> Option<(String, bool)> {
    if cmd != "local" && cmd != "global" {
        return None;
    }
    let rest = rest?.trim();
    let mut words = rest.splitn(2, char::is_whitespace);
    let name = words.next()?;
    // `local x = expr` and `local x value` both assign; `local x` alone empties.
    let value = words.next().unwrap_or("").trim();
    let value = value.strip_prefix('=').unwrap_or(value).trim();
    let name = name.trim_start_matches('`').trim_end_matches('\'');
    if name.is_empty() {
        return None;
    }
    let empty = value.is_empty() || value == "\"\"";
    Some((name.to_owned(), empty))
}

/// `` `x' `` or `$x` as the whole of a `foreach … in` list.
fn sole_macro_ref(text: &str) -> Option<(String, &'static str)> {
    let t = text.trim();
    if let Some(inner) = t.strip_prefix('`').and_then(|s| s.strip_suffix('\'')) {
        if !inner.is_empty() && !inner.contains(['`', '\'', ' ']) {
            return Some((inner.to_owned(), "local"));
        }
    }
    if let Some(inner) = t.strip_prefix('$') {
        if !inner.is_empty() && inner.chars().all(|c| c.is_alphanumeric() || c == '_') {
            return Some((inner.to_owned(), "global"));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;
    use crate::lints::lint_document;

    fn fires(src: &str) -> bool {
        fires_with(src, &Env::default())
    }

    fn fires_with(src: &str, env: &Env) -> bool {
        let idx = ParseIndex::new(src);
        lint_document(&idx, env).iter().any(|d| d.code == "L011")
    }

    #[test]
    fn fires_on_a_macro_that_is_never_assigned() {
        assert!(fires(
            "foreach v of local outcomes {\n    summarize `v'\n}\n"
        ));
    }

    #[test]
    fn silent_when_assigned_at_file_scope() {
        assert!(!fires(
            "local outcomes price mpg\nforeach v of local outcomes {\n    summarize `v'\n}\n"
        ));
    }

    #[test]
    fn fires_when_the_assignment_is_conditional() {
        let src = "\
if 1 {
    local outcomes price
}
foreach v of local outcomes {
    summarize `v'
}
";
        assert!(fires(src));
    }

    #[test]
    fn fires_on_a_deliberately_emptied_macro() {
        assert!(fires(
            "local outcomes\nforeach v of local outcomes {\n    summarize `v'\n}\n"
        ));
    }

    #[test]
    fn silent_when_the_session_already_holds_the_macro() {
        let env = Env {
            locals: vec!["outcomes".to_owned()],
            ..Env::default()
        };
        assert!(!fires_with(
            "foreach v of local outcomes {\n    summarize `v'\n}\n",
            &env
        ));
    }

    #[test]
    fn a_bare_macro_reference_in_an_in_list_is_recognised() {
        assert_eq!(
            sole_macro_ref("`outcomes'"),
            Some(("outcomes".to_owned(), "local"))
        );
        assert_eq!(sole_macro_ref("$G"), Some(("G".to_owned(), "global")));
        assert_eq!(sole_macro_ref("price mpg"), None);
    }
}
