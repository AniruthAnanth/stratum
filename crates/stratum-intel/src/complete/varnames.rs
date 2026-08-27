//! Variable, stored-result, frame and value-label completion — design 07 §7.1.
//!
//! The varlist sources return empty when no dataset is loaded, and that is a
//! feature: §7.1's closing line is "**Everything above works with no network, no
//! key and no loaded dataset**". Commands, functions, keywords and time-series
//! operators still complete; the variable rows simply are not there.
//!
//! # `_all`, wildcards and time-series operators
//!
//! Offered as first-class rows in varlist position, because they are what a
//! Stata user reaches for and because none of them is discoverable by typing a
//! prefix of a variable name.

use stratum_parse::ast::StoredClass;

use super::rank::Ranker;
use super::{CompletionContext, CompletionKind};

/// Group order inside an `Expr` completion: variables, then functions, then
/// keywords and operators. The tier still dominates — an exact-prefix keyword
/// beats a subsequence-matched variable — but at equal tier the variable wins,
/// which is what a statistics IDE should do.
const G_VAR: u8 = 0;
const G_PENDING: u8 = 1;
const G_FN: u8 = 2;
const G_KEYWORD: u8 = 3;

/// Language keywords that can follow a varlist.
const KEYWORDS: &[&str] = &[
    "if", "in", "using", "by", "bysort", "_all", "_n", "_N", "_pi", "_rc",
];

/// Time-series and factor-variable operators, offered as prefixes.
const OPERATORS: &[(&str, &str)] = &[
    ("L.", "lag"),
    ("L2.", "lag 2"),
    ("F.", "lead"),
    ("D.", "difference"),
    ("S.", "seasonal difference"),
    ("i.", "indicators"),
    ("c.", "continuous"),
];

pub(super) fn offer_expr<'a>(r: &mut Ranker<'a>, ctx: &CompletionContext<'a>) {
    offer_variables(r, ctx);
    for (i, f) in stratum_parse::all_functions().iter().enumerate() {
        r.offer(
            f.name,
            CompletionKind::Function,
            G_FN,
            i as u32,
            Some(if f.deterministic {
                "function"
            } else {
                "random"
            }),
            None,
            ctx.recency(f.name),
            ctx.frequency(f.name),
        );
    }
    for (i, k) in KEYWORDS.iter().enumerate() {
        r.offer(
            k,
            CompletionKind::Keyword,
            G_KEYWORD,
            i as u32,
            None,
            None,
            u32::MAX,
            0,
        );
    }
    for (i, (op, what)) in OPERATORS.iter().enumerate() {
        r.offer(
            op,
            CompletionKind::Keyword,
            G_KEYWORD,
            (KEYWORDS.len() + i) as u32,
            Some(what),
            None,
            u32::MAX,
            0,
        );
    }
    for (i, name) in ctx.env.scalars.iter().enumerate() {
        r.offer(
            name,
            CompletionKind::Scalar,
            G_VAR,
            i as u32,
            Some("scalar"),
            None,
            ctx.recency(name),
            0,
        );
    }
    for (i, name) in ctx.env.matrices.iter().enumerate() {
        r.offer(
            name,
            CompletionKind::Matrix,
            G_VAR,
            i as u32,
            Some("matrix"),
            None,
            ctx.recency(name),
            0,
        );
    }
}

/// The live varlist, in storage order, plus variables an unexecuted block above
/// would create.
pub(super) fn offer_variables<'a>(r: &mut Ranker<'a>, ctx: &CompletionContext<'a>) {
    if let Some(vars) = ctx.env.varnames.as_ref() {
        for (i, name) in vars.iter().enumerate() {
            r.offer(
                name,
                CompletionKind::Variable,
                G_VAR,
                i as u32,
                None,
                None,
                ctx.recency(name),
                ctx.frequency(name),
            );
        }
    }
    // Design 07 §7.1: "variables created by *unexecuted* blocks above, badged
    // 'not created yet'". They are offered because the user is writing the code
    // that will use them; the badge is what stops it being a lie.
    for (i, p) in ctx.env.pending_vars.iter().enumerate() {
        r.offer(
            &p.name,
            CompletionKind::Variable,
            G_PENDING,
            i as u32,
            Some("not created yet"),
            None,
            u32::MAX,
            ctx.frequency(&p.name),
        );
    }
}

pub(super) fn offer_stored<'a>(
    r: &mut Ranker<'a>,
    ctx: &CompletionContext<'a>,
    class: StoredClass,
) {
    let names = match class {
        StoredClass::E => &ctx.env.e_names,
        StoredClass::R => &ctx.env.r_names,
        StoredClass::S => &ctx.env.s_names,
        // `c()` is Stata's own settings namespace, not a session product. The
        // keys we know about are the ones the reproducibility checks name.
        StoredClass::C => {
            for (i, k) in crate::lints::facts::ENVIRONMENT_C_KEYS.iter().enumerate() {
                r.offer(
                    k,
                    CompletionKind::StoredResult,
                    0,
                    i as u32,
                    Some("machine-dependent"),
                    None,
                    u32::MAX,
                    0,
                );
            }
            return;
        }
    };
    for (i, name) in names.iter().enumerate() {
        r.offer(
            name,
            CompletionKind::StoredResult,
            0,
            i as u32,
            None,
            None,
            ctx.recency(name),
            0,
        );
    }
}

pub(super) fn offer_frames<'a>(r: &mut Ranker<'a>, ctx: &CompletionContext<'a>) {
    for (i, name) in ctx.env.frames.iter().enumerate() {
        r.offer(
            name,
            CompletionKind::Frame,
            0,
            i as u32,
            None,
            None,
            ctx.recency(name),
            0,
        );
    }
}

pub(super) fn offer_value_labels<'a>(r: &mut Ranker<'a>, ctx: &CompletionContext<'a>) {
    for (i, name) in ctx.env.value_labels.iter().enumerate() {
        r.offer(
            name,
            CompletionKind::ValueLabel,
            0,
            i as u32,
            None,
            None,
            ctx.recency(name),
            0,
        );
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use crate::complete::{complete, CompletionContext, CompletionKind};
    use crate::{Env, PendingVar};

    fn auto() -> Env {
        Env {
            varnames: Some(
                ["make", "price", "mpg", "rep78", "foreign"]
                    .iter()
                    .map(|s| (*s).to_owned())
                    .collect(),
            ),
            e_names: vec!["N".to_owned(), "r2".to_owned(), "df_m".to_owned()],
            ..Env::default()
        }
    }

    fn list(src: &str, env: &Env) -> Vec<(String, CompletionKind)> {
        complete(&CompletionContext::new(src, src.len(), env))
            .items
            .into_iter()
            .map(|i| (i.label, i.kind))
            .collect()
    }

    #[test]
    fn a_variable_outranks_a_function_at_the_same_tier() {
        let got = list("summarize pr", &auto());
        assert_eq!(got[0].0, "price", "{got:?}");
    }

    #[test]
    fn stored_results_complete_from_the_session() {
        let got = list("display e(r", &auto());
        assert_eq!(got.first().map(|g| g.0.as_str()), Some("r2"), "{got:?}");
        assert_eq!(got[0].1, CompletionKind::StoredResult);
    }

    #[test]
    fn with_no_dataset_the_variable_rows_are_simply_absent() {
        let env = Env::default();
        let got = list("summarize pr", &env);
        assert!(
            got.iter().all(|g| g.1 != CompletionKind::Variable),
            "{got:?}"
        );
        // ... and everything else still works.
        assert!(!got.is_empty());
    }

    #[test]
    fn a_pending_variable_is_offered_and_badged() {
        let mut env = auto();
        env.pending_vars.push(PendingVar {
            name: "ln_price".to_owned(),
            block_label: "B4".to_owned(),
        });
        let items = complete(&CompletionContext::new("summarize ln_", 13, &env)).items;
        let it = items
            .iter()
            .find(|i| i.label == "ln_price")
            .expect("offered");
        assert_eq!(it.detail.as_deref(), Some("not created yet"));
    }

    #[test]
    fn time_series_operators_are_discoverable() {
        let got = list("regress y L", &auto());
        assert!(got.iter().any(|g| g.0 == "L."), "{got:?}");
    }
}
