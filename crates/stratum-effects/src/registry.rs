//! `CommandRegistry` — CONTRACTS §13, A22.
//!
//! The single source of truth for "does THIS BUILD actually implement this
//! command?", as distinct from "is this a Stata command?" — which is
//! [`crate::EffectTable::is_known_command`]'s question.
//!
//! Two things consume it and both get worse without it: `ResultEnvelope.actions`
//! renders a "Margins" button only when the build can run `margins` (A22), and
//! the gutter's run affordance and the CLI's exit-10 path both need to say "this
//! build does not implement that" instead of failing at dispatch time.

/// What this build implements.
pub trait CommandRegistry: Send + Sync {
    /// Is `cmd` implemented by this build? `cmd` is the CANONICAL name — the
    /// caller resolves abbreviations through `stratum_parse::CommandTable`
    /// first, so this is a lookup and not a second abbreviation engine.
    fn implements(&self, cmd: &str) -> bool;

    /// Is `opt` implemented for `cmd`? A command can be implemented while one of
    /// its options is not, and answering that with "the command works" is how a
    /// user gets silently different numbers.
    fn implements_option(&self, cmd: &str, opt: &str) -> bool;

    /// Graph kinds this build can render: `"histogram"`, `"twoway rcap"`, ….
    fn graph_kinds(&self) -> &[&str];
}
