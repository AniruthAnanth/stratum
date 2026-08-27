//! This crate's own [`EffectTable`] rows, and the [`CommandRegistry`] answer
//! for what it implements (A1, A22, CONTRACTS §13).
//!
//! The trait is declared in `stratum-effects` rather than `stratum-runtime`
//! precisely so this file can exist: `stratum-runtime` depends on
//! `stratum-stats`, so a trait declared there and implemented here is a hard
//! Cargo cycle.
//!
//! # The one rule
//!
//! Every set is a MAY-set and every answer is biased toward "yes". Under-
//! approximating a read set leaves a downstream block marked `Current` after its
//! input changed (INV-1) — a wrong number in a paper. Over-approximating costs a
//! spurious re-run. So a varlist we cannot resolve statically becomes
//! [`VarSet::unknown`], never an empty set.

use stratum_effects::{
    Atomicity, CommandRegistry, EffectSet, EffectTable, RwEffect, StaticCtx, VarSet,
};
use stratum_parse::ast::command::{Command, Slots};
use stratum_parse::ast::varlist::{VarItemKind, VarList, VarPattern};
use stratum_parse::CommandAst;

/// The commands this crate implements, canonical spelling, sorted.
pub const COMMANDS: &[&str] = &[
    "correlate",
    "predict",
    "pwcorr",
    "regress",
    "summarize",
    "tabulate",
    "ttest",
];

/// The options this crate implements, per command. `05` §15's v1 list and
/// nothing more: a command that works while one of its options silently does
/// not is how a user gets different numbers without being told.
const OPTIONS: &[(&str, &[&str])] = &[
    ("correlate", &["covariance"]),
    ("predict", &["xb", "residuals", "stdp"]),
    ("pwcorr", &["obs", "print", "sig", "star"]),
    ("regress", &["beta", "level", "noconstant", "robust", "vce"]),
    ("summarize", &["detail", "meanonly"]),
    (
        "tabulate",
        &["cell", "chi2", "col", "missing", "nofreq", "nolabel", "row"],
    ),
    ("ttest", &["by", "level", "unequal"]),
];

/// `stratum-stats`' rows of the static effect table.
#[derive(Clone, Copy, Debug, Default)]
pub struct StatsEffects;

impl EffectTable for StatsEffects {
    fn effects(&self, cmd: &CommandAst, ctx: &StaticCtx<'_>) -> EffectSet {
        let Some((name, slots)) = resolve(cmd) else {
            return EffectSet::unknown_all();
        };
        if !COMMANDS.contains(&name) {
            return EffectSet::unknown_all();
        }

        let mut e = EffectSet::new();
        // Everything here is read-only against the frame, so the undo journal
        // has nothing to restore and INV-2 holds trivially.
        e.atomicity = Atomicity::Rollbackable;
        // `if`/`in` make an answer depend on which rows exist and in what order.
        e.order_sensitive = slots.in_.is_some();
        if let Some(x) = &slots.if_ {
            // The `if` expression reads variables we do not parse here; the
            // conservative answer is the only sound one.
            let _ = x;
            e.reads = VarSet::unknown();
        }

        match name {
            "predict" => {
                // The regressors come from `e(b)`, which is a RUNTIME fact. A
                // static reader cannot know them, so `predict` may read
                // anything, and it creates the variable it names.
                e.reads = VarSet::unknown();
                e.estimates = RwEffect::Read;
                e.rclass = RwEffect::Write;
                e.creates = varset(slots.varlist.as_ref(), ctx);
            }
            "regress" => {
                e.reads.union(&varset(slots.varlist.as_ref(), ctx));
                e.reads.union(&option_varset(slots, "vce"));
                e.estimates = RwEffect::Write;
                // `regress` clears r() and posts nothing to it.
                e.rclass = RwEffect::Write;
            }
            "ttest" => {
                e.reads.union(&varset(slots.varlist.as_ref(), ctx));
                e.reads.union(&option_varset(slots, "by"));
                e.reads_metadata = true;
                e.rclass = RwEffect::Write;
            }
            "summarize" | "tabulate" => {
                e.reads.union(&varset(slots.varlist.as_ref(), ctx));
                // Both print labels and formats, so both depend on var_layout.
                e.reads_metadata = true;
                e.rclass = RwEffect::Write;
            }
            _ => {
                // correlate, pwcorr.
                e.reads.union(&varset(slots.varlist.as_ref(), ctx));
                e.rclass = RwEffect::Write;
            }
        }
        e
    }

    fn is_known_command(&self, name: &str) -> bool {
        COMMANDS.contains(&name)
    }
}

impl CommandRegistry for StatsEffects {
    fn implements(&self, cmd: &str) -> bool {
        COMMANDS.contains(&cmd)
    }

    fn implements_option(&self, cmd: &str, opt: &str) -> bool {
        OPTIONS
            .iter()
            .find(|(c, _)| *c == cmd)
            .is_some_and(|(_, o)| o.contains(&opt))
    }

    fn graph_kinds(&self) -> &[&str] {
        // No graphs. `stratum-graph` answers this question for its own rows.
        &[]
    }
}

/// The canonical command name and its slots, or `None` for anything this table
/// has no business answering for.
fn resolve(cmd: &CommandAst) -> Option<(&'static str, &Slots)> {
    match &cmd.cmd {
        Command::Known(k) => Some((stratum_parse::cmdtable::command(k.id).canonical, &k.slots)),
        _ => None,
    }
}

/// A varlist as a [`VarSet`], resolving `_all` and globs against the known
/// layout when there is one and falling back to `unknown` when there is not.
fn varset(list: Option<&VarList>, ctx: &StaticCtx<'_>) -> VarSet {
    let Some(list) = list else {
        return VarSet::new();
    };
    // The narrowing hook, deliberately not taken yet. With `ctx.known_vars`
    // live, `inc*`, `a-z` and `_all` could each expand to an exact name list —
    // but that expansion is `stratum_parse::expand_varlist`, and a second copy
    // of it here would be the twin A10 bans and could disagree with the one the
    // runtime actually runs. So today the answer does not depend on `ctx`, and
    // the parameter stays so that the day it does is a change to this function
    // and not to every caller.
    let _ = ctx;
    names_of(list)
}

/// The variables a varlist may name, with no context to narrow it.
fn names_of(list: &VarList) -> VarSet {
    if list.has_hole() {
        // An unexpanded macro where a name belongs: we do not guess.
        return VarSet::unknown();
    }
    let mut out = VarSet::new();
    for item in &list.items {
        let atoms = match &item.kind {
            VarItemKind::Single(a) => std::slice::from_ref(a),
            VarItemKind::Interact { atoms, .. } => atoms.as_slice(),
        };
        for a in atoms {
            match &a.base {
                VarPattern::Name(n) => out.insert(n),
                VarPattern::Glob(_)
                | VarPattern::Tilde(_)
                | VarPattern::All
                | VarPattern::Range { .. }
                | VarPattern::Typed { .. }
                | VarPattern::Labeled { .. }
                | VarPattern::Hole { .. } => return VarSet::unknown(),
            }
        }
    }
    out
}

/// The variables named inside an option argument — `vce(cluster rep78)`,
/// `by(foreign)`. Conservative: anything we cannot read as a bare name widens
/// to `unknown`.
///
/// Two argument shapes reach here and both must be handled, which is the thing
/// that is easy to get wrong. The command table declares `ttest`'s `by()` as a
/// varlist, so it arrives already parsed as [`OptionArg::VarList`]; `regress`'s
/// `vce()` is declared shallow and arrives as [`OptionArg::Raw`] text. Treating
/// only the `Raw` shape and widening the other to `unknown` is sound but says
/// "`ttest x, by(g)` may read every variable in the dataset", which re-runs
/// every `ttest` in the document whenever anything at all is edited.
fn option_varset(slots: &Slots, opt: &str) -> VarSet {
    use stratum_parse::ast::command::OptionArg;
    let mut out = VarSet::new();
    for item in &slots.options.items {
        if item.canonical != Some(opt) {
            continue;
        }
        let Some(arg) = &item.arg else { continue };
        match arg {
            // Already a parsed varlist: `by(foreign)`, `by(foreign rep78)`.
            OptionArg::VarList(list) => out.union(&names_of(list)),
            OptionArg::Raw(raw) => out.union(&raw_option_varset(&raw.text)),
            // A number, a string, a format: nothing that can name a variable.
            // `level(90)` and `star(.05)` land here and read nothing.
            OptionArg::Int(_) | OptionArg::Real(_) | OptionArg::Fmt(_) => {}
            _ => return VarSet::unknown(),
        }
    }
    out
}

/// The variables a shallow-parsed option argument names.
///
/// `vce()` is the only one this crate reads, and its vocabulary is closed:
/// `ols` and `robust` name no variable at all, and `cluster clustvar` names
/// exactly one. Taking "the last word" unconditionally — the obvious reading —
/// would put a *variable* called `robust` into the read set of every
/// `regress …, vce(robust)`, so a user with a variable of that name would see
/// spurious invalidations. Everything outside the closed vocabulary widens.
fn raw_option_varset(text: &str) -> VarSet {
    let mut words = text.split_whitespace();
    let Some(head) = words.next() else {
        return VarSet::new();
    };
    match head {
        "ols" | "robust" => VarSet::new(),
        "cluster" => {
            let mut out = VarSet::new();
            let rest: Vec<&str> = words.collect();
            // Exactly one variable, spelled plainly, or we do not know.
            match rest.as_slice() {
                [w] if is_plain_name(w) => out.insert(w),
                _ => return VarSet::unknown(),
            }
            out
        }
        _ => VarSet::unknown(),
    }
}

fn is_plain_name(w: &str) -> bool {
    !w.is_empty()
        && w.chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && w.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;
    use stratum_parse::{parse_command, ParseMode};

    /// Parse a command the way the runtime hands it to the effect table: one
    /// macro-EXPANDED logical line.
    fn ast(src: &str) -> CommandAst {
        let (a, _diags) = parse_command(src, ParseMode::Execute);
        a
    }

    /// `resolve` must return the CANONICAL spelling, so the `match` below it
    /// never has to know that `su`, `sum` and `summarize` are one command.
    #[test]
    fn resolve_canonicalises_abbreviations() {
        for src in ["summarize price", "sum price", "su price"] {
            let a = ast(src);
            let (name, _) = resolve(&a).unwrap_or_else(|| panic!("`{src}` did not resolve"));
            assert_eq!(name, "summarize", "`{src}`");
        }
        let (name, _) = resolve(&ast("reg price mpg")).expect("regress");
        assert_eq!(name, "regress");
    }

    /// The registry answers for exactly `05` §15's v1 surface. A command that
    /// works while one of its options silently does nothing is how a user gets
    /// different numbers without being told, so an option we do not implement
    /// must answer `false` here rather than be quietly accepted.
    #[test]
    fn the_registry_claims_only_what_is_implemented() {
        let r = StatsEffects;
        for c in COMMANDS {
            assert!(r.implements(c), "{c}");
            assert!(r.is_known_command(c), "{c}");
        }
        for c in ["regres", "logit", "areg", "anova", "xtreg", "summarise"] {
            assert!(!r.implements(c), "{c} is not ours");
        }
        assert!(r.implements_option("regress", "vce"));
        assert!(r.implements_option("regress", "noconstant"));
        assert!(r.implements_option("summarize", "detail"));
        assert!(r.implements_option("ttest", "unequal"));
        // `05` §15 defers these; claiming them would silently drop the option.
        for (c, o) in [
            ("regress", "hc2"),
            ("regress", "hc3"),
            ("regress", "depname"),
            ("summarize", "separator"),
            ("predict", "cooksd"),
            ("predict", "stdf"),
            ("tabulate", "summarize"),
            ("ttest", "welch"),
        ] {
            assert!(!r.implements_option(c, o), "{c}, {o} is deferred");
        }
        // An option name that belongs to a different command must not leak
        // across: `OPTIONS` is keyed by command for exactly this reason.
        assert!(!r.implements_option("summarize", "vce"));
        assert!(!r.implements_option("correlate", "detail"));
        assert!(r.graph_kinds().is_empty());
    }

    /// The cluster and `by()` variables are read by the command, so a block
    /// that rewrites `rep78` must invalidate `regress …, vce(cluster rep78)`.
    /// They live inside an option argument, which is the one place a varlist
    /// scan does not look.
    #[test]
    fn option_variables_join_the_read_set() {
        let a = ast("regress price mpg weight, vce(cluster rep78)");
        let (_, slots) = resolve(&a).expect("regress");
        let v = option_varset(slots, "vce");
        assert!(v.contains_name("rep78"), "vce(cluster rep78) reads rep78");

        let a = ast("ttest mpg, by(foreign)");
        let (_, slots) = resolve(&a).expect("ttest");
        let v = option_varset(slots, "by");
        assert!(v.contains_name("foreign"), "by(foreign) reads foreign");
    }

    /// …and anything we cannot read as a bare name widens to `unknown` rather
    /// than to the empty set. Under-approximating a read set leaves a stale
    /// block marked `Current` (INV-1); over-approximating only costs a re-run.
    #[test]
    fn an_unreadable_option_argument_is_unknown_not_empty() {
        let a = ast("regress price mpg, vce(cluster `clustvar')");
        let (_, slots) = resolve(&a).expect("regress");
        let v = option_varset(slots, "vce");
        assert!(
            !v.is_empty(),
            "an unresolved cluster variable must not read nothing"
        );
        assert!(v.may_intersect(&VarSet::unknown()));
    }

    /// An option this command was not given contributes nothing at all — the
    /// scan is keyed on the canonical option name, not on position.
    #[test]
    fn an_absent_option_contributes_no_variables() {
        let a = ast("regress price mpg weight");
        let (_, slots) = resolve(&a).expect("regress");
        assert!(option_varset(slots, "vce").is_empty());
        assert!(option_varset(slots, "by").is_empty());
    }

    /// `vce()`'s vocabulary is closed, and only `cluster` names a variable.
    /// Reading "the last word" would make `vce(robust)` claim to read a
    /// variable called `robust`, and a user who has one would watch every
    /// regression in the document re-run whenever they touched it.
    #[test]
    fn vce_names_a_variable_only_under_cluster() {
        assert!(raw_option_varset("robust").is_empty());
        assert!(raw_option_varset("ols").is_empty());
        assert!(raw_option_varset("cluster rep78").contains_name("rep78"));
        // Two words after `cluster` is not something we can read; so is a
        // vce() vocabulary we do not know.
        assert!(!raw_option_varset("cluster a b").is_empty());
        assert!(!raw_option_varset("cluster a b").contains_name("a"));
        assert!(!raw_option_varset("hc3").is_empty());
        assert!(!raw_option_varset("hc3").contains_name("hc3"));
    }

    /// A varlist we can read exactly is read exactly; anything with a pattern
    /// or an unexpanded macro in it widens to `unknown`.
    #[test]
    fn a_varlist_widens_only_when_it_has_to() {
        let a = ast("summarize price mpg weight");
        let (_, slots) = resolve(&a).expect("summarize");
        let v = names_of(slots.varlist.as_ref().expect("varlist"));
        assert!(v.contains_name("price") && v.contains_name("mpg") && v.contains_name("weight"));
        assert_eq!(v.named.len(), 3, "no extra names");
        assert!(!v.unknown, "a plain varlist is exact");

        for src in ["summarize pri*", "summarize _all", "summarize price-weight"] {
            let a = ast(src);
            let (_, slots) = resolve(&a).unwrap_or_else(|| panic!("`{src}`"));
            let v = names_of(slots.varlist.as_ref().expect("varlist"));
            assert!(v.unknown, "`{src}` cannot be resolved without a layout");
        }
    }

    #[test]
    fn plain_names_are_stata_names() {
        assert!(is_plain_name("rep78"));
        assert!(is_plain_name("_cons"));
        assert!(!is_plain_name(""));
        assert!(!is_plain_name("78rep"), "a name cannot start with a digit");
        assert!(!is_plain_name("rep*"), "a glob is not a name");
        assert!(!is_plain_name("a-z"), "a range is not a name");
    }
}
