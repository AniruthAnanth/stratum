//! Checklist item 9 — programs, the ado path, and the compiled-command cache.
//!
//! The interesting half is the exclusion. A do-file that only works because of
//! an ado in your home directory is not reproducible, and it fails on your
//! colleague's machine with `command whatever is unrecognized` in a way that
//! looks like a bug in the file rather than a bug in the environment. So a clean
//! run's path is [`AdoPath::clean`], which contains no [`AdoDir::Personal`] and
//! no [`AdoDir::Plus`] entry, and the override that puts them back is a
//! *recorded* one: [`AdoPath::personal_included`] is what the reproducibility
//! report reads to say so out loud.
//!
//! `PERSONAL` and `PLUS` are not merely absent from the default vector — they
//! are unrepresentable in a clean path, because [`AdoPath::clean`] filters by
//! [`AdoDir`] rather than by string, so a caller that passes
//! `~/Library/Application Support/Stata/ado/personal` in the project list still
//! gets it in, but tagged, and [`AdoPath::is_clean`] still says no.

use camino::{Utf8Path, Utf8PathBuf};
use indexmap::IndexMap;

/// Which of Stata's `sysdir` slots a path came from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AdoDir {
    /// Shipped commands. Always searched.
    Base,
    /// The site-wide directory. Always searched.
    Site,
    /// The project's own `ado/` directory, and the current directory. Always
    /// searched — this is the one that makes a project's ado reproducible,
    /// because it travels in the repository.
    Project,
    /// `PERSONAL`. Excluded from clean runs.
    Personal,
    /// `PLUS` — where `ssc install` puts things. Excluded from clean runs.
    Plus,
    /// `OLDPLACE`. Excluded from clean runs for the same reason as `PERSONAL`.
    Oldplace,
}

impl AdoDir {
    /// True for the slots a clean run keeps.
    #[must_use]
    pub fn survives_clean(self) -> bool {
        matches!(self, AdoDir::Base | AdoDir::Site | AdoDir::Project)
    }
}

/// One entry on the search path.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct AdoEntry {
    /// Which slot it came from.
    pub kind: AdoDir,
    /// The directory.
    pub dir: Utf8PathBuf,
}

/// The ado search path, in search order.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct AdoPath {
    entries: Vec<AdoEntry>,
    personal_included: bool,
}

impl AdoPath {
    /// The clean path: every entry whose slot [`AdoDir::survives_clean`], in the
    /// order given, and nothing else.
    ///
    /// Entries are deduplicated by directory, first occurrence winning, because
    /// a duplicate changes nothing about which file is found and would make the
    /// path a poor equality key for the clean-state audit.
    #[must_use]
    pub fn clean(entries: impl IntoIterator<Item = AdoEntry>) -> Self {
        let mut out = Self {
            entries: Vec::new(),
            personal_included: false,
        };
        for e in entries {
            if e.kind.survives_clean() {
                out.push_unique(e);
            }
        }
        out
    }

    /// The path with the user's `PERSONAL`/`PLUS`/`OLDPLACE` directories put
    /// back — item 9's documented override.
    ///
    /// Sets [`AdoPath::personal_included`], which the reproducibility report
    /// surfaces. A clean run that used this path is not a clean run, and the
    /// report is where that is said.
    #[must_use]
    pub fn with_personal(entries: impl IntoIterator<Item = AdoEntry>) -> Self {
        let mut out = Self {
            entries: Vec::new(),
            personal_included: false,
        };
        for e in entries {
            if !e.kind.survives_clean() {
                out.personal_included = true;
            }
            out.push_unique(e);
        }
        out
    }

    fn push_unique(&mut self, e: AdoEntry) {
        if !self.entries.iter().any(|x| x.dir == e.dir) {
            self.entries.push(e);
        }
    }

    /// The entries, in search order.
    #[must_use]
    pub fn entries(&self) -> &[AdoEntry] {
        &self.entries
    }

    /// The directories, in search order.
    pub fn dirs(&self) -> impl Iterator<Item = &Utf8Path> {
        self.entries.iter().map(|e| e.dir.as_path())
    }

    /// True when a `PERSONAL`, `PLUS` or `OLDPLACE` directory is on the path.
    #[must_use]
    pub fn personal_included(&self) -> bool {
        self.personal_included
    }

    /// Item 9's audit: no user directory anywhere on the path.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        !self.personal_included && self.entries.iter().all(|e| e.kind.survives_clean())
    }
}

/// A `program define`d in this session.
///
/// The body is kept verbatim: `program list` prints it back, and the canonical
/// token stream behind a block's `CodeHash` is computed from source, so a
/// normalised body would make a program that was defined and then redefined
/// identically look like a change.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ProgramDef {
    /// The name as `program define` gave it.
    pub name: String,
    /// The source between `program` and `end`, verbatim.
    pub body: String,
    /// `program define foo, rclass|eclass|sclass|nclass`.
    pub class: ProgramClass,
    /// `, sortpreserve`.
    pub sortpreserve: bool,
    /// The `version` in force where it was defined.
    pub version: crate::config::StataVersion,
}

/// What a program returns.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ProgramClass {
    /// The default: returns nothing.
    #[default]
    Nclass,
    /// Sets `r()`.
    Rclass,
    /// Sets `e()`.
    Eclass,
    /// Sets `s()`.
    Sclass,
}

/// Item 9's state: the path, the programs, and the cache.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct AdoState {
    /// The search path.
    pub path: AdoPath,
    /// Programs defined in this session, in definition order — `program dir`
    /// prints it that way.
    pub programs: IndexMap<String, ProgramDef>,
    /// Command name → the ado file it resolved to, so a second call does not
    /// walk the path again. Discarded wholesale by a fresh session: a cache that
    /// survived would let a `PERSONAL` ado found by an earlier interactive run
    /// answer a clean run's lookup, which is item 9's exclusion defeated by its
    /// own optimisation.
    pub resolved: IndexMap<String, Utf8PathBuf>,
}

impl AdoState {
    /// A fresh ado state over `path`.
    #[must_use]
    pub fn fresh(path: AdoPath) -> Self {
        Self {
            path,
            programs: IndexMap::new(),
            resolved: IndexMap::new(),
        }
    }

    /// Item 9's audit.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.path.is_clean() && self.programs.is_empty() && self.resolved.is_empty()
    }
}
