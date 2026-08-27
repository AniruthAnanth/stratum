//! The macro environment — design 02 §4.1.
//!
//! Stata macros are **always strings**. There is no numeric macro: `local x = 1`
//! stores the two bytes `"1"`, which is why 02 §4.4's stringification rule is a
//! semantic contract and not a display detail.
//!
//! # Scoping is not lexical
//!
//! Locals live in exactly one [`LocalScope`], the innermost one, and a program
//! cannot see its caller's locals ([U] 18.3.1). That is why [`MacroEnv::local`]
//! reads only `scopes.last()` and never walks outward — a walk would make
//! `program define x` accidentally inherit the do-file's `` `i' ``, which is the
//! single most common way a hand-rolled interpreter diverges from Stata on a
//! real ado-file.
//!
//! Globals are one flat map with no scoping at all, which is exactly what makes
//! them dangerous and exactly what users expect.

use rustc_hash::FxHashMap;

/// Design 02 §4.1's `MacroValue`. `String` rather than `compact_str` for the
/// reason `ast/command.rs` records — the workspace dependency table is W00's
/// file and a member crate reaching outside it resolves two versions of a crate.
pub type MacroValue = String;

/// Limits that turn a runaway expansion into an error instead of a hang.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct MacroLimits {
    /// Maximum nesting depth of substitution. Stata's is 250.
    pub max_depth: u32,
    /// Maximum length of the expanded line. Stata/MP's line and macro limit.
    pub max_expanded_len: u32,
}

impl Default for MacroLimits {
    fn default() -> Self {
        MacroLimits {
            max_depth: 250,
            max_expanded_len: 1_081_511,
        }
    }
}

/// What opened a scope. The executor pops on the matching event.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ScopeKind {
    /// The scope a `do`/`run`/`include` file body executes in.
    DoFile,
    /// A `program define` body. Only this kind has positional arguments.
    Program {
        /// The program's name.
        name: String,
    },
    /// A `foreach`/`forvalues`/`while` body.
    ///
    /// Stata does NOT open a new local scope for a loop — `local` inside a loop
    /// is visible after it — so this variant exists for the executor's own
    /// bookkeeping and [`MacroEnv::push_scope`] is deliberately not what a loop
    /// calls. It is here because 02 §4.1 names it.
    Loop,
}

/// One frame of locals.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LocalScope {
    locals: FxHashMap<String, MacroValue>,
    /// What opened this scope.
    pub kind: ScopeKind,
    /// Temporary names allocated in this scope, dropped when it pops.
    owned_temps: Vec<String>,
}

impl LocalScope {
    /// An empty scope.
    pub fn new(kind: ScopeKind) -> Self {
        LocalScope {
            locals: FxHashMap::default(),
            kind,
            owned_temps: Vec::new(),
        }
    }

    /// Names defined in this scope, sorted — for `macro list` and for the
    /// `MacroInfo` view the AI context packer reads (CONTRACTS §13).
    ///
    /// Sorted because `FxHashMap`'s iteration order is unspecified and would
    /// otherwise reach the wire, breaking ADR-013's determinism gate.
    pub fn names(&self) -> Vec<&str> {
        let mut v: Vec<&str> = self.locals.keys().map(String::as_str).collect();
        v.sort_unstable();
        v
    }
}

/// The `__000000`, `__000001`, … allocator [V].
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub struct TempAlloc {
    next: u32,
}

impl TempAlloc {
    /// The next temporary name. Six digits, zero padded, exactly as Stata.
    pub fn alloc(&mut self) -> String {
        let n = self.next;
        self.next += 1;
        format!("__{n:06}")
    }

    /// How many names have been handed out. The executor asserts on this rather
    /// than on a name, because the NAMES are Stata-compatible and the count is
    /// what a leak shows up in.
    pub fn issued(&self) -> u32 {
        self.next
    }
}

/// Locals, globals, temporaries and the limits, in one place.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct MacroEnv {
    globals: FxHashMap<String, MacroValue>,
    scopes: Vec<LocalScope>,
    temps: TempAlloc,
    /// Depth and length caps.
    pub limits: MacroLimits,
}

impl Default for MacroEnv {
    fn default() -> Self {
        Self::new()
    }
}

impl MacroEnv {
    /// A fresh environment with one [`ScopeKind::DoFile`] scope already open.
    ///
    /// There is always at least one scope: `local x 1` typed at the console has
    /// to go somewhere, and an environment that can be in a scopeless state
    /// makes every `local` a `match` on `Option`.
    pub fn new() -> Self {
        MacroEnv {
            globals: FxHashMap::default(),
            scopes: vec![LocalScope::new(ScopeKind::DoFile)],
            temps: TempAlloc::default(),
            limits: MacroLimits::default(),
        }
    }

    /// Open a scope. Called on program entry, not on loop entry.
    pub fn push_scope(&mut self, kind: ScopeKind) {
        self.scopes.push(LocalScope::new(kind));
    }

    /// Close the innermost scope and return the temporary names it owned, which
    /// the executor must now destroy. Never pops the last scope.
    pub fn pop_scope(&mut self) -> Vec<String> {
        if self.scopes.len() <= 1 {
            return Vec::new();
        }
        self.scopes.pop().map(|s| s.owned_temps).unwrap_or_default()
    }

    /// The innermost scope.
    pub fn scope(&self) -> &LocalScope {
        self.scopes.last().expect("MacroEnv always has one scope")
    }

    /// Depth of the scope stack. One means "top level".
    pub fn depth(&self) -> usize {
        self.scopes.len()
    }

    /// Look up a local in the innermost scope only.
    pub fn local(&self, name: &str) -> Option<&str> {
        self.scope().locals.get(name).map(String::as_str)
    }

    /// Set a local in the innermost scope.
    pub fn set_local(&mut self, name: &str, value: impl Into<MacroValue>) {
        let s = self
            .scopes
            .last_mut()
            .expect("MacroEnv always has one scope");
        s.locals.insert(name.to_owned(), value.into());
    }

    /// Remove a local. `local x` with no value is a deletion in Stata, not an
    /// assignment of the empty string — `macro list` stops showing it.
    pub fn drop_local(&mut self, name: &str) {
        self.scopes
            .last_mut()
            .expect("MacroEnv always has one scope")
            .locals
            .remove(name);
    }

    /// Look up a global.
    pub fn global(&self, name: &str) -> Option<&str> {
        self.globals.get(name).map(String::as_str)
    }

    /// Set a global.
    pub fn set_global(&mut self, name: &str, value: impl Into<MacroValue>) {
        self.globals.insert(name.to_owned(), value.into());
    }

    /// Remove a global.
    pub fn drop_global(&mut self, name: &str) {
        self.globals.remove(name);
    }

    /// Global names, sorted. See [`LocalScope::names`] for why sorted.
    pub fn global_names(&self) -> Vec<&str> {
        let mut v: Vec<&str> = self.globals.keys().map(String::as_str).collect();
        v.sort_unstable();
        v
    }

    /// Allocate a temporary name and register it for destruction when the
    /// current scope pops.
    pub fn alloc_temp(&mut self) -> String {
        let name = self.temps.alloc();
        self.scopes
            .last_mut()
            .expect("MacroEnv always has one scope")
            .owned_temps
            .push(name.clone());
        name
    }

    /// How many temporaries have been issued in this session.
    pub fn temps_issued(&self) -> u32 {
        self.temps.issued()
    }

    /// Install `` `0' ``…`` `n' `` from a program's argument string.
    ///
    /// `` `0' `` is the whole argument string verbatim; `` `1' ``… are the
    /// whitespace-split positionals with one layer of quoting removed. Verified:
    /// `program define zz` / `di "`1'"` / `zz  X` prints `X` [V].
    pub fn set_positionals(&mut self, args: &str) {
        self.set_local("0", args);
        for (i, w) in split_args(args).into_iter().enumerate() {
            self.set_local(&(i + 1).to_string(), w);
        }
    }

    /// Pre-increment or pre-decrement a local, returning the NEW value.
    ///
    /// A macro that is not a number starts from zero, which is what Stata does
    /// and is why `` `++i' `` works on a fresh `i`.
    pub fn pre_step(&mut self, name: &str, delta: f64) -> String {
        let v = self.numeric_local(name) + delta;
        let s = crate::macros::stringify_number(v);
        self.set_local(name, s.clone());
        s
    }

    /// Post-increment or post-decrement a local, returning the OLD value.
    pub fn post_step(&mut self, name: &str, delta: f64) -> String {
        let old = self.numeric_local(name);
        let s = crate::macros::stringify_number(old + delta);
        self.set_local(name, s);
        crate::macros::stringify_number(old)
    }

    fn numeric_local(&self, name: &str) -> f64 {
        self.local(name)
            .and_then(|s| s.trim().parse::<f64>().ok())
            .unwrap_or(0.0)
    }
}

/// Split a program argument string the way `` `1' ``… are split: on whitespace,
/// honouring `"…"` and `` `"…"' `` as one argument each and removing one layer
/// of quoting.
pub fn split_args(s: &str) -> Vec<&str> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < b.len() {
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= b.len() {
            break;
        }
        if b[i] == b'`' && b.get(i + 1) == Some(&b'"') {
            let start = i;
            let mut depth = 0usize;
            while i < b.len() {
                if b[i] == b'`' && b.get(i + 1) == Some(&b'"') {
                    depth += 1;
                    i += 2;
                } else if b[i] == b'"' && b.get(i + 1) == Some(&b'\'') {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    i += 1;
                }
            }
            out.push(crate::lex::unquote(&s[start..i]));
            continue;
        }
        if b[i] == b'"' {
            let start = i;
            i += 1;
            while i < b.len() && b[i] != b'"' {
                i += 1;
            }
            i = (i + 1).min(b.len());
            out.push(crate::lex::unquote(&s[start..i]));
            continue;
        }
        let start = i;
        while i < b.len() && !b[i].is_ascii_whitespace() {
            i += 1;
        }
        out.push(&s[start..i]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locals_do_not_leak_across_a_program_boundary() {
        let mut env = MacroEnv::new();
        env.set_local("i", "7");
        env.push_scope(ScopeKind::Program {
            name: "zz".to_owned(),
        });
        assert_eq!(
            env.local("i"),
            None,
            "a program must not see its caller's locals"
        );
        env.set_local("i", "1");
        env.pop_scope();
        assert_eq!(env.local("i"), Some("7"), "the caller's local survives");
    }

    #[test]
    fn temps_are_stata_shaped_and_scope_owned() {
        let mut env = MacroEnv::new();
        assert_eq!(env.alloc_temp(), "__000000");
        assert_eq!(env.alloc_temp(), "__000001");
        env.push_scope(ScopeKind::DoFile);
        let inner = env.alloc_temp();
        assert_eq!(inner, "__000002");
        assert_eq!(env.pop_scope(), vec![inner]);
    }

    #[test]
    fn positionals_split_and_unquote() {
        let mut env = MacroEnv::new();
        env.set_positionals(r#"  X "two words" `"c"' "#);
        assert_eq!(env.local("0"), Some(r#"  X "two words" `"c"' "#));
        assert_eq!(env.local("1"), Some("X"));
        assert_eq!(env.local("2"), Some("two words"));
        assert_eq!(env.local("3"), Some("c"));
    }

    #[test]
    fn step_starts_from_zero_on_an_unset_macro() {
        let mut env = MacroEnv::new();
        assert_eq!(env.pre_step("i", 1.0), "1");
        assert_eq!(env.post_step("i", 1.0), "1");
        assert_eq!(env.local("i"), Some("2"));
    }
}
