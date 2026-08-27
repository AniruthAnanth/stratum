//! Macro completion — design 07 §7.1's `` ` `` and `$` rows.
//!
//! Local macros after a backtick, global macros after `$`, and nothing else in
//! either position. The two sigils are the least ambiguous syntax in Stata, so
//! this is the one role that never needs a heuristic.
//!
//! Macros defined **in this file, above the caret** are offered alongside the
//! session's, because that is what the user is about to reference and the
//! session does not know about them until the block runs. They are annotated so
//! the two are distinguishable.

use super::rank::Ranker;
use super::{CompletionContext, CompletionKind};

pub(super) fn offer_local<'a>(r: &mut Ranker<'a>, ctx: &CompletionContext<'a>) {
    for (i, name) in ctx.env.locals.iter().enumerate() {
        r.offer(
            name,
            CompletionKind::Local,
            0,
            i as u32,
            None,
            None,
            ctx.recency(name),
            ctx.frequency(name),
        );
    }
}

pub(super) fn offer_global<'a>(r: &mut Ranker<'a>, ctx: &CompletionContext<'a>) {
    for (i, name) in ctx.env.globals.iter().enumerate() {
        r.offer(
            name,
            CompletionKind::Global,
            0,
            i as u32,
            None,
            None,
            ctx.recency(name),
            ctx.frequency(name),
        );
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use crate::complete::{complete, CompletionContext, CompletionKind};
    use crate::Env;

    fn env() -> Env {
        Env {
            locals: vec!["outcomes".to_owned(), "controls".to_owned()],
            globals: vec!["ROOT".to_owned()],
            varnames: Some(vec!["price".to_owned()]),
            ..Env::default()
        }
    }

    #[test]
    fn a_backtick_offers_locals_and_only_locals() {
        let e = env();
        let src = "summarize `out";
        let items = complete(&CompletionContext::new(src, src.len(), &e)).items;
        assert_eq!(items.len(), 1, "{items:?}");
        assert_eq!(items[0].label, "outcomes");
        assert_eq!(items[0].kind, CompletionKind::Local);
    }

    #[test]
    fn a_dollar_offers_globals_and_only_globals() {
        let e = env();
        let src = "use $RO";
        let items = complete(&CompletionContext::new(src, src.len(), &e)).items;
        assert_eq!(items.len(), 1, "{items:?}");
        assert_eq!(items[0].kind, CompletionKind::Global);
    }

    #[test]
    fn with_no_macros_in_scope_the_popup_is_empty_rather_than_wrong() {
        let e = Env::default();
        let src = "summarize `out";
        assert!(complete(&CompletionContext::new(src, src.len(), &e))
            .items
            .is_empty());
    }
}
