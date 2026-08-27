//! Command-position completion — design 07 §7.1's first row.
//!
//! Built-in commands, commands resolvable from the ado path, user `program
//! define`s earlier in the file, and the five snippets. Abbreviations are not
//! offered as separate rows: `su` already matches `summarize` in the exact-prefix
//! tier, and a popup that listed every legal abbreviation of every command would
//! be unreadable.

use super::rank::Ranker;
use super::{CompletionContext, CompletionKind};

/// The multi-line constructs worth a snippet. Deliberately five: these are the
/// ones with a closing token that is easy to forget, which is the only thing a
/// snippet genuinely buys over typing.
const SNIPPETS: &[(&str, &str)] = &[
    ("foreach", "foreach v of varlist  {\n    \n}"),
    ("forvalues", "forvalues i = 1/10 {\n    \n}"),
    ("preserve", "preserve\n\nrestore"),
    ("program", "program define \nend"),
    ("while", "while  {\n    \n}"),
];

pub(super) fn offer<'a>(r: &mut Ranker<'a>, ctx: &CompletionContext<'a>) {
    for (i, sig) in stratum_parse::all_commands().iter().enumerate() {
        r.offer(
            sig.canonical,
            CompletionKind::Command,
            0,
            i as u32,
            Some(sig.help),
            None,
            ctx.recency(sig.canonical),
            ctx.frequency(sig.canonical),
        );
    }
    if let Some(file) = ctx.file {
        for (i, name) in file.programs().iter().enumerate() {
            r.offer(
                name,
                CompletionKind::Command,
                1,
                i as u32,
                Some("this file"),
                None,
                ctx.recency(name),
                ctx.frequency(name),
            );
        }
    }
    for (i, name) in ctx.env.installed_ado.iter().enumerate() {
        r.offer(
            name,
            CompletionKind::Command,
            2,
            i as u32,
            Some("ado"),
            None,
            ctx.recency(name),
            0,
        );
    }
    for (i, (label, body)) in SNIPPETS.iter().enumerate() {
        r.offer(
            label,
            CompletionKind::Snippet,
            3,
            i as u32,
            Some("snippet"),
            Some(body),
            u32::MAX,
            0,
        );
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use crate::complete::{complete, CompletionContext, CompletionKind, FileIndex};
    use crate::{Env, ParseIndex};

    #[test]
    fn a_builtin_completes_with_nothing_configured() {
        let env = Env::default();
        let list = complete(&CompletionContext::new("regr", 4, &env));
        assert_eq!(list.items[0].label, "regress");
        assert_eq!(list.items[0].kind, CompletionKind::Command);
    }

    #[test]
    fn a_user_program_defined_in_the_file_is_offered() {
        let src = "program define mytable\n    di 1\nend\nmyt";
        let idx = ParseIndex::new(src);
        let file = FileIndex::new(&idx);
        let env = Env::default();
        let list = complete(&CompletionContext {
            text: src,
            cursor: src.len(),
            env: &env,
            file: Some(&file),
        });
        assert!(
            list.items.iter().any(|i| i.label == "mytable"),
            "{:?}",
            list.items
        );
    }

    #[test]
    fn snippets_are_offered_and_carry_their_body() {
        let env = Env::default();
        let list = complete(&CompletionContext::new("forea", 5, &env));
        let s = list
            .items
            .iter()
            .find(|i| i.kind == CompletionKind::Snippet)
            .expect("a snippet");
        assert_eq!(s.label, "foreach");
        assert!(s.insert.as_deref().is_some_and(|b| b.contains('{')));
    }

    #[test]
    fn an_ado_command_the_caller_told_us_about_is_offered() {
        let env = Env {
            installed_ado: vec!["estout".to_owned()],
            ..Env::default()
        };
        let list = complete(&CompletionContext::new("esto", 4, &env));
        assert!(list.items.iter().any(|i| i.label == "estout"));
    }
}
