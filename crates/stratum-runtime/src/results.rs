//! `r()`, `e()` and `s()` — the three stored-result singletons, their clear
//! semantics, and the `return list` family's classic layout.
//!
//! # Insertion order is the layout (C31)
//!
//! `return list` and `ereturn list` print in the order the command stored
//! things, not alphabetically — `tests/golden/stata18/core_surface.log` shows
//! `e(cmdline)`, `e(title)`, `e(marginsok)`, `e(vce)`, `e(depvar)`, `e(cmd)`, …
//! which is neither alphabetical nor sorted by anything else. So the store is
//! ordered, and `StoredResultsView` (CONTRACTS §9.1) is a `Vec` of pairs for the
//! same reason. A `HashMap` here would produce a different transcript on every
//! run and break `--deterministic` (A8).
//!
//! [`Ordered`] is a `Vec` rather than an `IndexMap` because the sets are tiny —
//! `regress` posts twelve scalars, ten macros and three matrices, and that is
//! the largest thing in the built-in surface. A linear scan of a dozen short
//! `Box<str>` keys beats hashing them, and it keeps `indexmap` out of
//! `stratum-runtime`'s dependency list, which is W06a's file.
//!
//! # Clear semantics
//!
//! Stored results are *singletons with a lifetime*, and getting the lifetime
//! wrong is a fidelity bug that shows up as a do-file silently reading a stale
//! `r(mean)`. [`StoredResults::begin_command`] is the one place the rules live:
//!
//! | command class | effect on entry |
//! |---|---|
//! | r-class | `r()` cleared |
//! | e-class | `e()` **and** `r()` cleared |
//! | s-class | `s()` cleared |
//! | n-class | nothing cleared |
//!
//! The e-class row is the one the committed goldens do not pin — no capture in
//! `tests/golden/stata18/` runs `return list` after an estimation command — so
//! it is implemented from `[P] ereturn` ("`ereturn post` … eliminates any
//! existing `e()` results and any `r()` results") and is flagged as unverified.
//! It is deliberately one line in one method so that correcting it is a
//! one-line change rather than an audit.

use std::sync::Arc;

use stratum_core::fmt::fmt_g;
use stratum_proto::{MatrixMeta, StoredResultsView, StyleId, StyledRun};

use crate::state::fingerprint::{Ns, StateFingerprint};

/// Which singleton.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Hash)]
pub enum Class {
    /// `r()` — general commands.
    R,
    /// `e()` — estimation commands.
    E,
    /// `s()` — parsing and utility commands.
    S,
}

impl Class {
    /// The sigil the classic listing prints.
    #[must_use]
    pub fn sigil(self) -> &'static str {
        match self {
            Class::R => "r",
            Class::E => "e",
            Class::S => "s",
        }
    }
}

/// What a command declares itself to be, for the clear rules.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Hash)]
pub enum CommandClass {
    /// Stores nothing; leaves every singleton alone (`generate`, `label`).
    N,
    /// Stores `r()`.
    R,
    /// Stores `e()`.
    E,
    /// Stores `s()`.
    S,
}

/// A stored matrix. Row-major, with Stata's row and column names.
#[derive(Clone, Debug, PartialEq)]
pub struct Matrix {
    /// Rows.
    pub rows: u32,
    /// Columns.
    pub cols: u32,
    /// Row names, or empty for `r1`, `r2`, …
    pub rownames: Vec<String>,
    /// Column names, or empty for `c1`, `c2`, …
    pub colnames: Vec<String>,
    /// `rows * cols` values, row-major.
    pub data: Vec<f64>,
}

impl Matrix {
    /// The wire projection.
    #[must_use]
    pub fn meta(&self) -> MatrixMeta {
        MatrixMeta {
            rows: self.rows,
            cols: self.cols,
            rownames: self.rownames.clone(),
            colnames: self.colnames.clone(),
        }
    }
}

/// An insertion-ordered map. See the module header for why it is a `Vec`.
#[derive(Clone, Debug, PartialEq)]
pub struct Ordered<T>(Vec<(Box<str>, T)>);

impl<T> Default for Ordered<T> {
    fn default() -> Self {
        Self(Vec::new())
    }
}

impl<T> Ordered<T> {
    /// Insert, or overwrite in place. A rewrite keeps the original position:
    /// `regress` sets `e(cmdline)` before `e(title)` and re-posting the former
    /// must not move it to the end of the listing.
    pub fn insert(&mut self, name: &str, value: T) {
        match self.0.iter_mut().find(|(k, _)| k.as_ref() == name) {
            Some(slot) => slot.1 = value,
            None => self.0.push((name.into(), value)),
        }
    }

    /// Look one up.
    pub fn get(&self, name: &str) -> Option<&T> {
        self.0
            .iter()
            .find(|(k, _)| k.as_ref() == name)
            .map(|(_, v)| v)
    }

    /// Entries in insertion order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &T)> {
        self.0.iter().map(|(k, v)| (k.as_ref(), v))
    }

    /// True when nothing is stored.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Entries stored.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Forget everything.
    pub fn clear(&mut self) {
        self.0.clear();
    }
}

/// One singleton's contents.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ResultSet {
    scalars: Ordered<f64>,
    macros: Ordered<String>,
    matrices: Ordered<Matrix>,
    functions: Vec<Box<str>>,
}

impl ResultSet {
    /// Empty.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// True when nothing is stored.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.scalars.is_empty()
            && self.macros.is_empty()
            && self.matrices.is_empty()
            && self.functions.is_empty()
    }

    /// Store a scalar, keeping insertion order on first write.
    pub fn set_scalar(&mut self, name: &str, v: f64) {
        self.scalars.insert(name, v);
    }

    /// Store a macro.
    pub fn set_macro(&mut self, name: &str, v: impl Into<String>) {
        self.macros.insert(name, v.into());
    }

    /// Store a matrix.
    pub fn set_matrix(&mut self, name: &str, m: Matrix) {
        self.matrices.insert(name, m);
    }

    /// Declare a function-valued result, e.g. `e(sample)`.
    pub fn set_function(&mut self, name: &str) {
        if !self.functions.iter().any(|f| f.as_ref() == name) {
            self.functions.push(name.into());
        }
    }

    /// A stored scalar.
    #[must_use]
    pub fn scalar(&self, name: &str) -> Option<f64> {
        self.scalars.get(name).copied()
    }

    /// A stored macro.
    #[must_use]
    pub fn get_macro(&self, name: &str) -> Option<&str> {
        self.macros.get(name).map(String::as_str)
    }

    /// A stored matrix.
    #[must_use]
    pub fn matrix(&self, name: &str) -> Option<&Matrix> {
        self.matrices.get(name)
    }

    /// Scalars in insertion order.
    pub fn scalars(&self) -> impl Iterator<Item = (&str, f64)> {
        self.scalars.iter().map(|(k, v)| (k, *v))
    }

    /// Macros in insertion order.
    pub fn macros(&self) -> impl Iterator<Item = (&str, &str)> {
        self.macros.iter().map(|(k, v)| (k, v.as_str()))
    }

    /// Matrices in insertion order.
    pub fn matrices(&self) -> impl Iterator<Item = (&str, &Matrix)> {
        self.matrices.iter()
    }

    /// Function-valued results in insertion order.
    pub fn functions(&self) -> impl Iterator<Item = &str> {
        self.functions.iter().map(Box::as_ref)
    }

    /// Forget everything.
    pub fn clear(&mut self) {
        self.scalars.clear();
        self.macros.clear();
        self.matrices.clear();
        self.functions.clear();
    }
}

/// The three singletons and their versions.
#[derive(Clone, Debug, Default)]
pub struct StoredResults {
    r: ResultSet,
    e: ResultSet,
    s: ResultSet,
    r_version: u64,
    e_version: u64,
    s_version: u64,
    sample: Option<Arc<stratum_data::BitSet>>,
}

impl StoredResults {
    /// Empty, as a fresh session has them.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Read one singleton.
    #[must_use]
    pub fn get(&self, class: Class) -> &ResultSet {
        match class {
            Class::R => &self.r,
            Class::E => &self.e,
            Class::S => &self.s,
        }
    }

    /// Write one singleton. Bumps its version, which is what
    /// `DepKey::RClass`/`Estimates`/`SClass` compare.
    pub fn get_mut(&mut self, class: Class) -> &mut ResultSet {
        self.bump(class);
        match class {
            Class::R => &mut self.r,
            Class::E => &mut self.e,
            Class::S => &mut self.s,
        }
    }

    /// The version of one singleton.
    #[must_use]
    pub fn version(&self, class: Class) -> u64 {
        match class {
            Class::R => self.r_version,
            Class::E => self.e_version,
            Class::S => self.s_version,
        }
    }

    /// Apply the entry-time clear rules for a command of this class.
    ///
    /// See the module header for the table and for which row the goldens pin.
    pub fn begin_command(&mut self, class: CommandClass) {
        match class {
            CommandClass::N => {}
            CommandClass::R => self.clear(Class::R),
            CommandClass::E => {
                self.clear(Class::E);
                // `[P] ereturn`: posting eliminates any existing e() results
                // AND any r() results. UNVERIFIED against a committed golden.
                self.clear(Class::R);
            }
            CommandClass::S => self.clear(Class::S),
        }
    }

    /// `return clear` / `ereturn clear` / `sreturn clear`.
    ///
    /// A clear **bumps** the version rather than resetting it. "Was set, now
    /// gone" and "never set" must not be the same version, or a block that read
    /// `r(mean)` before the clear would stay ✓ Current.
    pub fn clear(&mut self, class: Class) {
        let was_empty = self.get(class).is_empty();
        match class {
            Class::R => self.r.clear(),
            Class::E => {
                self.e.clear();
                self.sample = None;
            }
            Class::S => self.s.clear(),
        }
        if !was_empty {
            self.bump(class);
        }
    }

    /// Record the estimation sample behind `e(sample)`.
    pub fn set_sample(&mut self, sample: Arc<stratum_data::BitSet>) {
        self.sample = Some(sample);
        self.e.set_function("sample");
        self.bump(Class::E);
    }

    /// The estimation sample, if one is posted.
    #[must_use]
    pub fn sample(&self) -> Option<&Arc<stratum_data::BitSet>> {
        self.sample.as_ref()
    }

    /// Push the three versions into the session fingerprint.
    ///
    /// One call at commit, idempotent. Keeping the counters here and syncing
    /// rather than bumping the fingerprint from every setter is what stops the
    /// two drifting when a command forgets a call.
    pub fn sync_into(&self, fp: &mut StateFingerprint) {
        fp.set_ns(Ns::RClass, self.r_version);
        fp.set_ns(Ns::Estimates, self.e_version);
        fp.set_ns(Ns::SClass, self.s_version);
    }

    /// The read-only projection `SessionIntrospect::stored_results` returns.
    #[must_use]
    pub fn view(&self) -> StoredResultsView {
        let e_b_colnames = self
            .e
            .matrix("b")
            .map(|m| m.colnames.clone())
            .unwrap_or_default();
        StoredResultsView {
            r_scalars: self.r.scalars().map(|(k, v)| (k.to_owned(), v)).collect(),
            r_macros: self
                .r
                .macros()
                .map(|(k, v)| (k.to_owned(), v.to_owned()))
                .collect(),
            r_matrices: self
                .r
                .matrices()
                .map(|(k, m)| (k.to_owned(), m.meta()))
                .collect(),
            e_scalars: self.e.scalars().map(|(k, v)| (k.to_owned(), v)).collect(),
            e_macros: self
                .e
                .macros()
                .map(|(k, v)| (k.to_owned(), v.to_owned()))
                .collect(),
            e_matrices: self
                .e
                .matrices()
                .map(|(k, m)| (k.to_owned(), m.meta()))
                .collect(),
            s_macros: self
                .s
                .macros()
                .map(|(k, v)| (k.to_owned(), v.to_owned()))
                .collect(),
            e_b_colnames,
        }
    }

    fn bump(&mut self, class: Class) {
        match class {
            Class::R => self.r_version += 1,
            Class::E => self.e_version += 1,
            Class::S => self.s_version += 1,
        }
    }
}

/// The width `return list` right-aligns a name into, measured from
/// `tests/golden/stata18/core_surface.log`: `r(N)` sits at columns 19–22 and
/// `e(properties)` at 11–22.
const NAME_FIELD: usize = 22;

/// `return list` / `ereturn list` / `sreturn list`, as `Vec<StyledRun>` (A12).
///
/// Byte-exact against `tests/golden/stata18/core_surface.log`; `tests/smcl.rs`
/// holds that assertion. Styling follows the SMCL channels Stata itself uses —
/// the section headers and the names are `{txt}`, the values are `{res}` — so
/// the Classic pane can print result values in a distinct ink without any
/// regex-scraping of a rendered table.
#[must_use]
pub fn classic_list(class: Class, set: &ResultSet) -> Vec<StyledRun> {
    let mut runs: Vec<StyledRun> = Vec::new();
    let sigil = class.sigil();
    let section = |runs: &mut Vec<StyledRun>, title: &str| {
        runs.push(StyledRun {
            text: format!("\n{title}:\n"),
            style: StyleId::Text,
        });
    };

    if !set.scalars.is_empty() {
        section(&mut runs, "scalars");
        for (name, v) in set.scalars() {
            let label = format!("{sigil}({name})");
            runs.push(StyledRun {
                text: format!("{label:>NAME_FIELD$} = "),
                style: StyleId::Text,
            });
            runs.push(StyledRun {
                text: format!(" {}\n", fmt_g(v, 18).trim()),
                style: StyleId::Result,
            });
        }
    }
    if !set.macros.is_empty() {
        section(&mut runs, "macros");
        for (name, v) in set.macros() {
            let label = format!("{sigil}({name})");
            runs.push(StyledRun {
                text: format!("{label:>NAME_FIELD$} : "),
                style: StyleId::Text,
            });
            runs.push(StyledRun {
                text: format!("\"{v}\"\n"),
                style: StyleId::Result,
            });
        }
    }
    if !set.matrices.is_empty() {
        section(&mut runs, "matrices");
        for (name, m) in set.matrices() {
            let label = format!("{sigil}({name})");
            runs.push(StyledRun {
                text: format!("{label:>NAME_FIELD$} : "),
                style: StyleId::Text,
            });
            runs.push(StyledRun {
                // One literal space after the separator, not a right-aligned
                // field: `tests/golden/stata18/core_surface.log` shows the same
                // single extra space before a 2-character `74` and before a
                // 17-character `317252881.2439711`, so the gap is a constant.
                text: format!(" {} x {}\n", m.rows, m.cols),
                style: StyleId::Result,
            });
        }
    }
    if !set.functions.is_empty() {
        section(&mut runs, "functions");
        for name in set.functions() {
            let label = format!("{sigil}({name})");
            runs.push(StyledRun {
                text: format!("{label:>NAME_FIELD$}   \n"),
                style: StyleId::Text,
            });
        }
    }
    runs
}

#[cfg(test)]
mod tests {
    use super::*;
    use stratum_proto::styled::to_plain;

    #[test]
    fn summarize_return_list_is_byte_exact_against_the_golden() {
        // tests/golden/stata18/core_surface.log, `. return list` after
        // `summarize mpg` on auto.dta.
        let mut r = ResultSet::new();
        for (n, v) in [
            ("N", 74.0),
            ("sum_w", 74.0),
            ("mean", 21.297_297_297_297_3),
            ("Var", 33.472_047_389_855_61),
            ("sd", 5.785_503_209_735_141),
            ("min", 12.0),
            ("max", 41.0),
            ("sum", 1576.0),
        ] {
            r.set_scalar(n, v);
        }
        let expected = "\n\
            scalars:\n\
            \x20                 r(N) =  74\n\
            \x20             r(sum_w) =  74\n\
            \x20              r(mean) =  21.2972972972973\n\
            \x20               r(Var) =  33.47204738985561\n\
            \x20                r(sd) =  5.785503209735141\n\
            \x20               r(min) =  12\n\
            \x20               r(max) =  41\n\
            \x20               r(sum) =  1576\n";
        assert_eq!(to_plain(&classic_list(Class::R, &r)), expected);
    }

    #[test]
    fn an_estimation_command_clears_r_as_well_as_e() {
        let mut st = StoredResults::new();
        st.get_mut(Class::R).set_scalar("mean", 1.0);
        st.get_mut(Class::E).set_macro("cmd", "regress");
        let r_before = st.version(Class::R);
        st.begin_command(CommandClass::E);
        assert!(st.get(Class::R).is_empty());
        assert!(st.get(Class::E).is_empty());
        assert!(
            st.version(Class::R) > r_before,
            "a clear must move the version"
        );
    }

    #[test]
    fn a_general_command_leaves_every_singleton_alone() {
        let mut st = StoredResults::new();
        st.get_mut(Class::R).set_scalar("mean", 1.0);
        st.get_mut(Class::E).set_macro("cmd", "regress");
        let before = (
            st.version(Class::R),
            st.version(Class::E),
            st.version(Class::S),
        );
        st.begin_command(CommandClass::N);
        assert_eq!(st.get(Class::R).scalar("mean"), Some(1.0));
        assert_eq!(st.get(Class::E).get_macro("cmd"), Some("regress"));
        assert_eq!(
            (
                st.version(Class::R),
                st.version(Class::E),
                st.version(Class::S)
            ),
            before
        );
    }

    #[test]
    fn clearing_an_already_empty_singleton_is_not_a_change() {
        let mut st = StoredResults::new();
        let v = st.version(Class::R);
        st.clear(Class::R);
        assert_eq!(st.version(Class::R), v);
    }

    #[test]
    fn insertion_order_survives_a_rewrite() {
        let mut set = ResultSet::new();
        set.set_macro("cmdline", "regress price mpg");
        set.set_macro("title", "Linear regression");
        set.set_macro("cmdline", "regress price weight");
        let names: Vec<&str> = set.macros().map(|(k, _)| k).collect();
        assert_eq!(names, vec!["cmdline", "title"]);
    }
}
