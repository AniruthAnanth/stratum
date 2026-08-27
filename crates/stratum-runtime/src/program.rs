//! `program define … end` — storage, lookup, and the call frame.
//!
//! A program is stored as **text**, not as an AST, and that is not laziness. A
//! program body is macro-expanded at *call* time, once per call, with that
//! call's `` `1' ``…`` `n' `` in scope. Parsing it at definition time would
//! either freeze the expansion (wrong: `` `1' `` would be the definition-time
//! value, which is empty) or require a parallel "unexpanded AST" representation
//! that the executor would then have to re-expand anyway. `ast::BlockCommand`
//! makes the same choice for loop bodies, and for the same reason — see its
//! note: "Loop bodies are a `Span` into the PRE-EXPANSION logical-line text,
//! never a parsed AST."
//!
//! # Redefinition is an error, and it is the error users actually hit
//!
//! `program define p` when `p` already exists is `r(110)` "p already defined".
//! Users hit it constantly when re-running a do-file, which is why the real fix
//! — `program drop p` first, or `capture program drop p` — is what the
//! diagnostic suggests rather than something clever.

use rustc_hash::FxHashMap;
use stratum_parse::lints::StataError;

/// What a program does to `r()` / `e()` when it returns.
///
/// Declared on the `program define` line (`, rclass`) and recorded here because
/// design 03 §5.3 rule 8 substitutes a program's effects at its call site: a
/// program that is not `rclass` provably does not write `r()`, and saying so is
/// the difference between a downstream block staying `Current` and going stale
/// on every call.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub enum ProgramClass {
    /// Writes nothing to the stored-result namespaces.
    #[default]
    Plain,
    /// `, rclass` — may set `r()`.
    RClass,
    /// `, eclass` — may set `e()`.
    EClass,
    /// `, sclass` — may set `s()`.
    SClass,
}

/// One `program define` body.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Program {
    /// The name it was defined under.
    pub name: String,
    /// The body, verbatim, between the `program define` line and `end`.
    pub body: String,
    /// Stored-result class.
    pub class: ProgramClass,
    /// `, byable(recall)` — recorded, not yet honoured (v1.5).
    pub byable: bool,
    /// `, sortpreserve`.
    pub sortpreserve: bool,
}

/// Every program defined in this session.
///
/// `FxHashMap` keyed by name, and it is **never iterated where the order can
/// reach output**: [`ProgramTable::names`] sorts, because `program dir` prints a
/// list and an unspecified order there is a different transcript on every run
/// (ADR-013).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProgramTable {
    by_name: FxHashMap<String, Program>,
}

impl ProgramTable {
    /// An empty table.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Define a program.
    ///
    /// # Errors
    ///
    /// `r(110)` when the name is already taken. The message and the suggestion
    /// are what a user re-running a do-file needs, not a bare code.
    pub fn define(&mut self, p: Program) -> Result<(), StataError> {
        if self.by_name.contains_key(&p.name) {
            return Err(
                StataError::new(110, format!("{} already defined", p.name)).token(p.name.clone())
            );
        }
        self.by_name.insert(p.name.clone(), p);
        Ok(())
    }

    /// Look one up. Programs do **not** abbreviate ([U] 11.2.1).
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Program> {
        self.by_name.get(name)
    }

    /// Is this name a program?
    #[must_use]
    pub fn contains(&self, name: &str) -> bool {
        self.by_name.contains_key(name)
    }

    /// `program drop`.
    ///
    /// # Errors
    ///
    /// `r(111)` when no such program is defined — the same code a missing
    /// variable gets, because it is the same class of mistake.
    pub fn drop(&mut self, name: &str) -> Result<(), StataError> {
        if self.by_name.remove(name).is_none() {
            return Err(StataError::new(111, format!("{name} not found")).token(name.to_owned()));
        }
        Ok(())
    }

    /// Drop every program — `program drop _all`.
    pub fn clear(&mut self) {
        self.by_name.clear();
    }

    /// How many are defined.
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_name.len()
    }

    /// True when none are.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }

    /// Every name, **sorted** — see the struct note.
    #[must_use]
    pub fn names(&self) -> Vec<&str> {
        let mut v: Vec<&str> = self.by_name.keys().map(String::as_str).collect();
        v.sort_unstable();
        v
    }
}

/// The recursion cap.
///
/// Design 03 §5.3 rule 8 caps *static* substitution at 8; this is the dynamic
/// limit, and it is deliberately much larger — a recursive ado is legal Stata
/// and 8 would break real code. What it must not do is let a runaway recursion
/// take the process down with a stack overflow, which `catch_unwind` cannot
/// catch. Stata's own limit is 64 nested `do`/program levels; we allow the same
/// and answer `r(1000)` past it.
pub const MAX_CALL_DEPTH: u32 = 64;

#[cfg(test)]
mod tests {
    use super::*;

    fn prog(name: &str) -> Program {
        Program {
            name: name.to_owned(),
            body: "display 1\n".to_owned(),
            class: ProgramClass::Plain,
            byable: false,
            sortpreserve: false,
        }
    }

    #[test]
    fn redefinition_is_r110_and_names_the_program() {
        // The offending_token is the whole point: spec §21 turns it into
        // "program p is already defined — drop it first?".
        let mut t = ProgramTable::new();
        t.define(prog("p")).unwrap();
        let e = t.define(prog("p")).unwrap_err();
        assert_eq!(e.rc, 110);
        assert_eq!(e.offending_token.as_deref(), Some("p"));
    }

    #[test]
    fn dropping_an_undefined_program_is_r111_with_the_name() {
        let mut t = ProgramTable::new();
        let e = t.drop("nope").unwrap_err();
        assert_eq!(e.rc, 111);
        assert_eq!(e.offending_token.as_deref(), Some("nope"));
    }

    #[test]
    fn names_are_sorted_so_program_dir_is_reproducible() {
        let mut t = ProgramTable::new();
        for n in ["zeta", "alpha", "mid"] {
            t.define(prog(n)).unwrap();
        }
        assert_eq!(t.names(), vec!["alpha", "mid", "zeta"]);
    }
}
