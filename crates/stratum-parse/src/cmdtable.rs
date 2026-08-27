//! The generated command and function tables — design 02 §6.3 / §8.5.
//!
//! `build.rs` compiles `data/commands.ron` and `data/functions.ron` into the two
//! sorted statics this module wraps. The TYPES — [`CmdId`], [`CommandSig`],
//! [`OptionSpec`], [`CommandTable`] — are W04's, in [`crate::cmdsig`] (A29);
//! this file is only the loader that 02 §6.3 promised.
//!
//! # There are two tables and exactly one of them is authoritative
//!
//! `CommandTable::core()` is W04's hand-written wave-1 table. It exists because
//! segmentation had to label a region with a canonical command name before a
//! parser existed, and it is marked [`CommandTable::is_provisional`] because its
//! `slots`, `weights` and `options` are deliberately empty.
//!
//! [`table`] is the real one. `tests/parse.rs::generated_table_agrees_with_core`
//! asserts that every core row survives here with the SAME `min_abbrev` — the
//! field that decides which command a word resolves to. Two tables that disagree
//! about whether `su` is `summarize` would make the gutter label a region with
//! one command and the executor run another, which is the exact class of bug
//! decision D3 (one declarative table) was chosen to prevent.

use crate::cmdsig::{CmdFlags, SlotMask, WeightMask};
use crate::cmdsig::{
    CmdId, CommandLookup, CommandSig, CommandTable, OptionArgKind, OptionSpec, Tier,
};

include!(concat!(env!("OUT_DIR"), "/cmdtable_generated.rs"));

/// The command table this build parses with.
pub const fn table() -> CommandTable {
    CommandTable::new(COMMANDS)
}

/// Design 02 §13.1's `resolve_command`.
pub fn resolve_command(word: &str) -> CommandLookup {
    table().resolve(word)
}

/// Design 02 §13.1's `all_commands`, in canonical order.
pub const fn all_commands() -> &'static [CommandSig] {
    COMMANDS
}

/// Look a command up by id.
pub fn command(id: CmdId) -> &'static CommandSig {
    &COMMANDS[id.0 as usize]
}

/// Resolve an option name against a command's option list, applying the same
/// abbreviation rule as commands and accepting a `no`-prefixed negation.
///
/// Returns the spec, whether the spelling was negated, and nothing else — an
/// unknown option is `None` and becomes r(198) `option … not allowed` [V]
/// (`tests/golden/stata18/errors.log`: `summarize price, nosuchoption` and the
/// misspelling `summarize price, detial` both give exactly that).
pub fn resolve_option(sig: &'static CommandSig, word: &str) -> Option<(&'static OptionSpec, bool)> {
    if let Some(s) = lookup_option(sig, word) {
        return Some((s, false));
    }
    // `nooption` is always accepted as the negation of `option` (02 §6.1).
    // Tried SECOND: an option whose own name starts with `no` — `noconstant`,
    // `nolabel`, `nogenerate` — must win over reading it as a negation.
    if let Some(rest) = word.strip_prefix("no") {
        if let Some(s) = lookup_option(sig, rest) {
            if s.negatable {
                return Some((s, true));
            }
        }
    }
    None
}

fn lookup_option(sig: &'static CommandSig, word: &str) -> Option<&'static OptionSpec> {
    if word.is_empty() {
        return None;
    }
    if let Ok(i) = sig.options.binary_search_by(|o| o.canonical.cmp(word)) {
        return Some(&sig.options[i]);
    }
    let lo = sig
        .options
        .partition_point(|o| o.canonical.as_bytes() < word.as_bytes());
    let mut hit = None;
    for o in &sig.options[lo..] {
        if !o.canonical.starts_with(word) {
            break;
        }
        if o.min_abbrev > 0 && word.len() >= o.min_abbrev as usize {
            if hit.is_some() {
                // Ambiguous. Stata reports r(198) for the option, not a "did you
                // mean"; returning None puts it on that path.
                return None;
            }
            hit = Some(o);
        }
    }
    hit
}

/// What a function returns.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum FnRet {
    /// Numeric.
    Real,
    /// String.
    Str,
    /// Whichever branch was taken — `cond()`.
    Either,
}

/// One row of the function table.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct FnSig {
    /// Exact spelling. Case-sensitive.
    pub name: &'static str,
    /// Minimum argument count.
    pub min_args: u8,
    /// Maximum argument count. `255` means variadic.
    pub max_args: u8,
    /// Return type.
    pub ret: FnRet,
    /// False for the random generators — two calls with the same arguments may
    /// differ, so a region calling one can never be reused from cache.
    pub deterministic: bool,
    /// Release tier.
    pub tier: Tier,
}

/// Resolve a function name. Functions do NOT abbreviate.
pub fn function(name: &str) -> Option<&'static FnSig> {
    FUNCTIONS
        .binary_search_by(|f| f.name.cmp(name))
        .ok()
        .map(|i| &FUNCTIONS[i])
}

/// Every function signature, in name order.
pub const fn all_functions() -> &'static [FnSig] {
    FUNCTIONS
}

/// True when `n` arguments satisfy this signature.
impl FnSig {
    /// Arity check. `255` is the variadic marker.
    pub const fn accepts(&self, n: usize) -> bool {
        n >= self.min_args as usize && (self.max_args == 255 || n <= self.max_args as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_are_sorted_and_unique() {
        // `CommandTable::resolve` and `function` both binary-search. An unsorted
        // table does not fail loudly, it silently stops resolving.
        for w in COMMANDS.windows(2) {
            assert!(w[0].canonical < w[1].canonical, "commands out of order");
        }
        for w in FUNCTIONS.windows(2) {
            assert!(w[0].name < w[1].name, "functions out of order");
        }
        for c in COMMANDS {
            for w in c.options.windows(2) {
                assert!(
                    w[0].canonical < w[1].canonical,
                    "{}: options out of order",
                    c.canonical
                );
            }
        }
    }

    #[test]
    fn abbreviation_short_circuits_match_the_manual() {
        // 02 §6.3 names these two by hand as the cases a table must get right.
        assert_eq!(
            table().canonical("d").map(|s| s.canonical),
            Some("describe")
        );
        assert_eq!(table().canonical("l").map(|s| s.canonical), Some("list"));
        assert_eq!(
            table().canonical("su").map(|s| s.canonical),
            Some("summarize")
        );
        assert_eq!(
            table().canonical("reg").map(|s| s.canonical),
            Some("regress")
        );
        // `replace` may not be abbreviated ([U] 11.2.1).
        assert_eq!(table().canonical("repl"), None);
        assert_eq!(
            table().canonical("replace").map(|s| s.canonical),
            Some("replace")
        );
    }

    #[test]
    fn option_negation_never_shadows_a_no_named_option() {
        let reg = table().canonical("regress").expect("regress");
        let (spec, neg) = resolve_option(reg, "noconstant").expect("noconstant");
        assert_eq!(spec.canonical, "noconstant");
        assert!(
            !neg,
            "`noconstant` is its own option, not `constant` negated"
        );

        let sum = table().canonical("summarize").expect("summarize");
        let (spec, neg) = resolve_option(sum, "nodetail").expect("nodetail");
        assert_eq!(spec.canonical, "detail");
        assert!(neg);
        let (spec, neg) = resolve_option(sum, "d").expect("d");
        assert_eq!(spec.canonical, "detail");
        assert!(!neg);
        assert!(
            resolve_option(sum, "detial").is_none(),
            "r(198), not a fuzzy hit"
        );
    }

    #[test]
    fn functions_are_case_sensitive_and_arity_checked() {
        assert!(function("F").is_some());
        assert!(function("f").is_none(), "`f` is not a function");
        assert!(function("normal").expect("normal").accepts(1));
        assert!(!function("normal").expect("normal").accepts(2));
        assert!(function("min").expect("min").accepts(9), "min is variadic");
        assert!(!function("runiform").expect("runiform").deterministic);
        assert!(function("normal").expect("normal").deterministic);
    }
}
