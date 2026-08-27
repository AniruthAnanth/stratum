//! Call frames: local-macro scopes, `tempvar`/`tempname` lifetimes, and the
//! depth cap.
//!
//! `MacroEnv` (W04b) owns the *storage* — the scope stack, the `__000000`
//! allocator, and which locals a `pop_scope` discards. What lives here is the
//! part that needs the rest of the interpreter: a temporary VARIABLE is a column
//! in the frame, so dropping it when its scope ends is a dataset edit, not a map
//! removal, and nothing in `stratum-parse` can do it.
//!
//! # The rule that makes `tempvar` safe
//!
//! `tempvar x` allocates the name `__000000` and binds `` `x' `` to it. The
//! column is created later, if at all, by whatever command uses `` `x' `` — so
//! at scope exit we must drop *the columns that were actually created under a
//! name this scope allocated*, and must not error on the ones that never were.
//! [`Frame::index_of`](stratum_data::Frame::index_of) answering `None` is the
//! normal case, not a failure.
//!
//! # Why the depth cap is here and not in `program.rs`
//!
//! Recursion depth is a property of the *call stack*, and a `do` file calling a
//! program calling another `do` file shares one. Counting it in one place is
//! what makes `r(1000)` fire before the real stack does; `catch_unwind` cannot
//! catch a stack overflow, so this counter is the only thing between a runaway
//! recursion and a dead engine process.

use stratum_parse::lints::StataError;
use stratum_parse::macros::{MacroEnv, ScopeKind};

use crate::program::MAX_CALL_DEPTH;

/// One entry of the interpreter's call stack.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Frame {
    /// What opened it.
    pub kind: ScopeKind,
    /// Temporary NAMES allocated in this frame, in allocation order. Every one
    /// that turned into a column is dropped when the frame pops.
    pub temp_names: Vec<String>,
}

/// The interpreter's call stack, alongside `MacroEnv`'s scope stack.
///
/// The two are kept in step by [`CallStack::push`]/[`CallStack::pop`], which are
/// the only functions that touch either. They are separate types because
/// `MacroEnv` lives in `stratum-parse`, which must build for
/// `wasm32-unknown-unknown` and therefore cannot know what a dataset column is.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CallStack {
    frames: Vec<Frame>,
}

impl CallStack {
    /// An empty stack. The base scope belongs to `MacroEnv`, which always has
    /// one open, so depth `0` here means "console or top-level do-file".
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Current depth.
    #[must_use]
    pub fn depth(&self) -> u32 {
        self.frames.len() as u32
    }

    /// Enter a program or do-file body, opening a matching macro scope.
    ///
    /// # Errors
    ///
    /// `r(1000)` past [`MAX_CALL_DEPTH`]. The message names the thing being
    /// entered, because "too many nested levels" with no name is unactionable in
    /// a recursive ado.
    pub fn push(&mut self, env: &mut MacroEnv, kind: ScopeKind) -> Result<(), StataError> {
        if self.depth() >= MAX_CALL_DEPTH {
            let what = match &kind {
                ScopeKind::Program { name } => name.clone(),
                ScopeKind::DoFile => "do-file".to_owned(),
                ScopeKind::Loop => "loop".to_owned(),
            };
            return Err(StataError::new(
                1000,
                format!(
                    "too many nested levels entering {what}; recursion limit is {MAX_CALL_DEPTH}"
                ),
            )
            .token(what));
        }
        env.push_scope(kind.clone());
        self.frames.push(Frame {
            kind,
            temp_names: Vec::new(),
        });
        Ok(())
    }

    /// Allocate a temporary name in the innermost frame and bind `` `name' ``
    /// to it — `tempvar name` / `tempname name`.
    ///
    /// At the top level there is no frame to own it, so the name is allocated
    /// and bound but never auto-dropped; that matches Stata, where a `tempvar`
    /// at the console lives until `clear`.
    pub fn alloc_temp(&mut self, env: &mut MacroEnv, bind_to: &str) -> String {
        let temp = env.alloc_temp();
        env.set_local(bind_to, temp.clone());
        if let Some(f) = self.frames.last_mut() {
            f.temp_names.push(temp.clone());
        }
        temp
    }

    /// Leave the innermost frame, returning the temporary names it owned so the
    /// caller can drop any columns that were created under them.
    ///
    /// Answers an empty vector — and pops nothing — at depth `0`. An unbalanced
    /// pop is a bug in the executor, but the executor is what runs user code
    /// under `catch_unwind`, so this must not be the thing that panics.
    pub fn pop(&mut self, env: &mut MacroEnv) -> Vec<String> {
        let Some(f) = self.frames.pop() else {
            return Vec::new();
        };
        env.pop_scope();
        f.temp_names
    }

    /// The innermost program's name, when there is one — what `syntax`'s error
    /// messages and `_rc` reporting quote.
    #[must_use]
    pub fn current_program(&self) -> Option<&str> {
        self.frames.iter().rev().find_map(|f| match &f.kind {
            ScopeKind::Program { name } => Some(name.as_str()),
            _ => None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_temp_name_is_bound_and_owned_by_the_frame_that_allocated_it() {
        let mut env = MacroEnv::new();
        let mut st = CallStack::new();
        st.push(
            &mut env,
            ScopeKind::Program {
                name: "p".to_owned(),
            },
        )
        .unwrap();
        let t = st.alloc_temp(&mut env, "v");
        assert_eq!(env.local("v"), Some(t.as_str()));
        assert_eq!(st.pop(&mut env), vec![t]);
        // The scope went with it, so the binding is gone.
        assert_eq!(env.local("v"), None);
    }

    #[test]
    fn depth_is_capped_and_the_error_names_the_callee() {
        let mut env = MacroEnv::new();
        let mut st = CallStack::new();
        for _ in 0..MAX_CALL_DEPTH {
            st.push(
                &mut env,
                ScopeKind::Program {
                    name: "rec".to_owned(),
                },
            )
            .unwrap();
        }
        let e = st
            .push(
                &mut env,
                ScopeKind::Program {
                    name: "rec".to_owned(),
                },
            )
            .unwrap_err();
        assert_eq!(e.rc, 1000);
        assert_eq!(e.offending_token.as_deref(), Some("rec"));
    }

    #[test]
    fn popping_an_empty_stack_answers_rather_than_panicking() {
        // The executor runs user code under catch_unwind; an unbalanced pop must
        // surface as a no-op, not as a second panic inside the handler.
        let mut env = MacroEnv::new();
        let mut st = CallStack::new();
        assert!(st.pop(&mut env).is_empty());
    }

    #[test]
    fn current_program_finds_the_innermost_one_through_a_do_file() {
        let mut env = MacroEnv::new();
        let mut st = CallStack::new();
        st.push(
            &mut env,
            ScopeKind::Program {
                name: "outer".to_owned(),
            },
        )
        .unwrap();
        st.push(&mut env, ScopeKind::DoFile).unwrap();
        assert_eq!(st.current_program(), Some("outer"));
    }
}
