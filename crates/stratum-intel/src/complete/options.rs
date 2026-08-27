//! Option-position completion — design 07 §7.1.
//!
//! **That command's option grammar, with arity and value type, and nothing
//! else.** This is the row of §7.1's table that most visibly separates
//! role-dispatched completion from a word-frequency popup: after
//! `summarize price,` the only correct answers are `summarize`'s own options,
//! and offering a variable name there is worse than offering nothing.
//!
//! When the command word did not resolve — an ado command, a typo, or a macro in
//! the command position — there is no grammar to offer and the list is empty.
//! An empty popup is the honest answer; guessing from a neighbouring command's
//! grammar would produce a plausible option that fails at run time.

use stratum_parse::cmdsig::OptionArgKind;

use super::rank::Ranker;
use super::CompletionKind;

pub(super) fn offer<'a>(r: &mut Ranker<'a>, command: Option<&'static str>) {
    let Some(sig) = command.and_then(|c| stratum_parse::table().canonical(c)) else {
        return;
    };
    for (i, opt) in sig.options.iter().enumerate() {
        r.offer(
            opt.canonical,
            CompletionKind::Option,
            0,
            i as u32,
            Some(arity(opt.arg)),
            insert_for(opt.canonical, opt.arg),
            u32::MAX,
            0,
        );
    }
}

/// The right-aligned annotation: what the option takes.
const fn arity(kind: OptionArgKind) -> &'static str {
    match kind {
        OptionArgKind::None => "flag",
        OptionArgKind::Int => "(integer)",
        OptionArgKind::Real => "(number)",
        OptionArgKind::Str => "(string)",
        OptionArgKind::Numlist => "(numlist)",
        OptionArgKind::Varlist => "(varlist)",
        OptionArgKind::Exprs => "(expression)",
        OptionArgKind::Fmt => "(format)",
        OptionArgKind::Raw => "(…)",
    }
}

/// An option that takes an argument inserts its opening parenthesis, so the
/// caret lands where the value goes. A flag inserts nothing extra.
fn insert_for(canonical: &'static str, kind: OptionArgKind) -> Option<&'static str> {
    match kind {
        OptionArgKind::None => None,
        // `&'static str` and no allocation: the concatenation would need one,
        // and the popup's insert text is read on accept, not on every keystroke.
        // The consumer appends `(` when `detail` is not "flag".
        _ => {
            let _ = canonical;
            None
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use crate::complete::{complete, CompletionContext, CompletionKind};
    use crate::Env;

    fn labels(src: &str) -> Vec<String> {
        let env = Env::default();
        complete(&CompletionContext::new(src, src.len(), &env))
            .items
            .into_iter()
            .map(|i| i.label)
            .collect()
    }

    #[test]
    fn only_that_commands_options_are_offered() {
        let got = labels("summarize price, det");
        assert_eq!(got.first().map(String::as_str), Some("detail"), "{got:?}");
        assert!(
            got.iter().all(|l| l != "price"),
            "a variable name is never an option: {got:?}"
        );
    }

    #[test]
    fn an_unresolved_command_offers_nothing_rather_than_guessing() {
        assert!(labels("nosuchcommand x, det").is_empty());
    }

    #[test]
    fn the_annotation_says_what_the_option_takes() {
        let env = Env::default();
        let src = "summarize price, ";
        let list = complete(&CompletionContext::new(src, src.len(), &env));
        assert!(!list.items.is_empty());
        for item in &list.items {
            assert_eq!(item.kind, CompletionKind::Option);
            assert!(item.detail.is_some(), "{item:?}");
        }
    }
}
