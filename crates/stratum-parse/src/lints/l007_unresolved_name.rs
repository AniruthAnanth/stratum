//! `L007` — an unresolved variable name, with an edit-distance suggestion.
//!
//! Spec §21 asks for *"Did you mean 'income'?"*. This is that, and it is
//! Damerau–Levenshtein over the live `VarIndex` capped at distance 2 — no model,
//! no network, works offline and instantly. `summarize incom` is r(111)
//! `variable incom not found` [V]; the return code comes from resolution and the
//! suggestion comes from here.
//!
//! It runs only when the editor HAS a variable list. With no dataset loaded
//! every name is unresolved and the lint would be noise, so it stays silent.

use stratum_proto::{Diagnostic, Edit, SuggestionKind};

use crate::ast::{Command, CommandAst, VarItemKind, VarPattern};
use crate::lints::{code, edit_distance, warn, with_fix, LintCtx};

/// Report bare names that no variable matches.
pub fn check(cmd: &CommandAst, cx: &LintCtx<'_>, out: &mut Vec<Diagnostic>) {
    let Some(vcx) = cx.vars else { return };
    let Command::Known(k) = &cmd.cmd else { return };
    let sig = crate::cmdtable::command(k.id);
    // Only where the leading part is unambiguously a varlist of EXISTING
    // variables. `generate newvar = …` names something that does not exist yet
    // (NEWVARLIST), and `rename old new` / `format price %9.2f` have a tail the
    // universal grammar reads provisionally as a varlist and hands to the
    // command's own parser (REST). Warning in either case is a false positive,
    // and a lint that cries wolf gets switched off.
    if !sig.slots.contains(crate::cmdsig::SlotMask::VARLIST)
        || sig
            .slots
            .intersects(crate::cmdsig::SlotMask::NEWVARLIST.union(crate::cmdsig::SlotMask::REST))
    {
        return;
    }
    let Some(vl) = &k.slots.varlist else { return };
    for item in &vl.items {
        let VarItemKind::Single(atom) = &item.kind else {
            continue;
        };
        let VarPattern::Name(name) = &atom.base else {
            continue;
        };
        if crate::varlist::is_reserved(name) || vcx.vars.position(name).is_some() {
            continue;
        }
        // A legal abbreviation is not an error.
        if vcx.varabbrev && (0..vcx.vars.len()).any(|i| vcx.vars.name(i).starts_with(name.as_str()))
        {
            continue;
        }
        let mut best: Option<(usize, String)> = None;
        for i in 0..vcx.vars.len() {
            let cand = vcx.vars.name(i);
            let d = edit_distance(name, cand, 2);
            if d <= 2 && best.as_ref().is_none_or(|(bd, _)| d < *bd) {
                best = Some((d, cand.to_owned()));
            }
        }
        let diag = warn(code::L007, format!("variable {name} not found"), atom.span);
        out.push(match best {
            Some((_, cand)) => with_fix(
                diag,
                format!("did you mean `{cand}`?"),
                SuggestionKind::Rename,
                vec![Edit {
                    span: atom.span,
                    text: cand,
                }],
            ),
            None => diag,
        });
    }
}
