//! `StaticCtx` — design 03 §5.2.

use camino::Utf8Path;
use rustc_hash::FxHashMap;

use crate::varset::Name;

/// What static extraction is allowed to know about the world.
///
/// Everything here NARROWS an effect set, and every narrowing must be justified
/// by something concrete: a variable layout that is actually loaded, a macro
/// whose literal value was assigned earlier in the same file with no intervening
/// conditional reassignment. There is deliberately no field that lets the
/// extractor guess.
#[derive(Clone, Debug)]
pub struct StaticCtx<'a> {
    /// Current known variable layout, in storage order, when a dataset is
    /// loaded. Lets `_all`, `a-z` and `inc*` expand EXACTLY instead of becoming
    /// [`crate::VarSet::unknown`].
    pub known_vars: Option<&'a [Name]>,
    /// Literal macro values known from earlier STATIC assignment in the same
    /// file. An entry is dropped, never guessed, the moment a reassignment
    /// appears inside a loop or a conditional.
    pub const_macros: &'a FxHashMap<Name, Name>,
    /// Working directory, for resolving a relative `using` path to a literal.
    pub cwd: &'a Utf8Path,
    /// Audit mode expands more aggressively and reports what it could not
    /// resolve; engine mode is fast. The ANSWERS must agree — audit mode may
    /// only ever narrow further, never differently.
    pub for_audit: bool,
}

impl<'a> StaticCtx<'a> {
    /// A context that knows nothing: no dataset, no macros. Every extraction is
    /// still sound, just coarser.
    pub fn bare(cwd: &'a Utf8Path, empty: &'a FxHashMap<Name, Name>) -> Self {
        Self {
            known_vars: None,
            const_macros: empty,
            cwd,
            for_audit: false,
        }
    }
}
