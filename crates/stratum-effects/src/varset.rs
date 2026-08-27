//! `VarSet` — design 03 §5.1.
//!
//! A may-set of variable names. The one rule that matters:
//!
//! > **It is UNSOUND to say two sets do not intersect when you are not sure.**
//!
//! Every operation here is biased toward `true`. `unknown` and
//! [`VarPattern::All`] both mean "may be any variable", and a `Range` is treated
//! as overlapping everything unless the caller resolved it against a known
//! variable order first — a positional range `a-z` is a range in STORAGE order,
//! which the static analyser cannot know.

use smallvec::SmallVec;

/// Design 03 §5.1 spells these `CompactString`. `compact_str` is not in the
/// workspace dependency table (W00's root `Cargo.toml`), and a member crate
/// taking a dependency outside that table is how a workspace ends up resolving
/// two versions of one crate. `Box<str>` is the same 16 bytes on the heap path.
pub type Name = Box<str>;

/// A pattern from `inc*`, `a-z`, `~x` or `_all`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum VarPattern {
    /// `inc*` — names beginning with this.
    Prefix(Name),
    /// `*inc` — names ending with this.
    Suffix(Name),
    /// `a-z` — a range in STORAGE order, not alphabetical.
    Range(Name, Name),
    /// `_all`.
    All,
}

impl VarPattern {
    /// Does this pattern match a concrete name?
    ///
    /// A `Range` returns `true`: resolving it needs the dataset's storage order,
    /// which static analysis does not have, and answering `false` would drop a
    /// real dependency.
    pub fn matches(&self, name: &str) -> bool {
        match self {
            VarPattern::All => true,
            VarPattern::Prefix(p) => name.starts_with(p.as_ref()),
            VarPattern::Suffix(s) => name.ends_with(s.as_ref()),
            VarPattern::Range(..) => true,
        }
    }

    /// May two patterns describe a common name?
    pub fn may_overlap(&self, other: &VarPattern) -> bool {
        match (self, other) {
            (VarPattern::All, _) | (_, VarPattern::All) => true,
            (VarPattern::Range(..), _) | (_, VarPattern::Range(..)) => true,
            (VarPattern::Prefix(a), VarPattern::Prefix(b)) => {
                a.starts_with(b.as_ref()) || b.starts_with(a.as_ref())
            }
            (VarPattern::Suffix(a), VarPattern::Suffix(b)) => {
                a.ends_with(b.as_ref()) || b.ends_with(a.as_ref())
            }
            // A prefix and a suffix always admit a name carrying both.
            (VarPattern::Prefix(_), VarPattern::Suffix(_))
            | (VarPattern::Suffix(_), VarPattern::Prefix(_)) => true,
        }
    }
}

/// A may-set of variable names.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct VarSet {
    /// Sorted and deduplicated concrete names.
    pub named: SmallVec<[Name; 8]>,
    /// Patterns from `inc*`, `a-z` ranges, `_all`.
    pub patterns: SmallVec<[VarPattern; 2]>,
    /// `true` ⇒ may be ANY variable. Set whenever a macro, an unknown command,
    /// or a dynamic name reaches a varlist position.
    pub unknown: bool,
}

impl VarSet {
    /// The empty set.
    pub fn new() -> Self {
        Self::default()
    }

    /// "May be any variable."
    pub fn unknown() -> Self {
        Self {
            unknown: true,
            ..Self::default()
        }
    }

    /// True when this set is provably empty.
    pub fn is_empty(&self) -> bool {
        !self.unknown && self.named.is_empty() && self.patterns.is_empty()
    }

    /// Add a concrete name, keeping `named` sorted and deduplicated.
    pub fn insert(&mut self, name: &str) {
        match self.named.binary_search_by(|n| n.as_ref().cmp(name)) {
            Ok(_) => {}
            Err(at) => self.named.insert(at, name.into()),
        }
    }

    /// Add a pattern.
    pub fn insert_pattern(&mut self, p: VarPattern) {
        if !self.patterns.contains(&p) {
            self.patterns.push(p);
        }
    }

    /// Union in place.
    pub fn union(&mut self, other: &VarSet) {
        self.unknown |= other.unknown;
        for n in &other.named {
            self.insert(n);
        }
        for p in &other.patterns {
            self.insert_pattern(p.clone());
        }
    }

    /// May-intersect. Returning `false` when unsure is a soundness bug against
    /// INV-1, so every uncertain case answers `true`.
    pub fn may_intersect(&self, other: &VarSet) -> bool {
        if self.unknown || other.unknown {
            return true;
        }
        if self.has_all() || other.has_all() {
            return true;
        }
        if sorted_intersects(&self.named, &other.named) {
            return true;
        }
        self.patterns.iter().any(|p| {
            other.named.iter().any(|n| p.matches(n))
                || other.patterns.iter().any(|q| p.may_overlap(q))
        }) || other
            .patterns
            .iter()
            .any(|p| self.named.iter().any(|n| p.matches(n)))
    }

    /// Does this set contain a name it can prove?
    pub fn contains_name(&self, name: &str) -> bool {
        self.named
            .binary_search_by(|n| n.as_ref().cmp(name))
            .is_ok()
    }

    fn has_all(&self) -> bool {
        self.patterns.iter().any(|p| matches!(p, VarPattern::All))
    }
}

impl FromIterator<Name> for VarSet {
    fn from_iter<I: IntoIterator<Item = Name>>(iter: I) -> Self {
        let mut s = VarSet::new();
        for n in iter {
            s.insert(&n);
        }
        s
    }
}

/// Merge-walk of two sorted, deduplicated name lists.
fn sorted_intersects(a: &[Name], b: &[Name]) -> bool {
    let (mut i, mut j) = (0usize, 0usize);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => return true,
        }
    }
    false
}

/// A may-set of names in a namespace with no patterns — macros, scalars,
/// matrices, programs.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct NameSet {
    /// Sorted and deduplicated.
    pub names: SmallVec<[Name; 4]>,
    /// `true` ⇒ may be any name in this namespace.
    pub unknown: bool,
}

impl NameSet {
    /// The empty set.
    pub fn new() -> Self {
        Self::default()
    }

    /// "May be any name."
    pub fn unknown() -> Self {
        Self {
            unknown: true,
            ..Self::default()
        }
    }

    /// True when provably empty.
    pub fn is_empty(&self) -> bool {
        !self.unknown && self.names.is_empty()
    }

    /// Add a name, keeping the list sorted and deduplicated.
    pub fn insert(&mut self, name: &str) {
        if let Err(at) = self.names.binary_search_by(|n| n.as_ref().cmp(name)) {
            self.names.insert(at, name.into());
        }
    }

    /// Union in place.
    pub fn union(&mut self, other: &NameSet) {
        self.unknown |= other.unknown;
        for n in &other.names {
            self.insert(n);
        }
    }

    /// May-intersect.
    pub fn may_intersect(&self, other: &NameSet) -> bool {
        if self.unknown || other.unknown {
            return true;
        }
        sorted_intersects(&self.names, &other.names)
    }
}

/// A may-set of file paths.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct FileSet {
    /// Paths resolved to literals.
    pub paths: SmallVec<[camino::Utf8PathBuf; 2]>,
    /// `true` ⇒ a path was built from a macro and cannot be resolved statically.
    pub unknown: bool,
}

impl FileSet {
    /// The empty set.
    pub fn new() -> Self {
        Self::default()
    }

    /// "May be any path."
    pub fn unknown() -> Self {
        Self {
            unknown: true,
            ..Self::default()
        }
    }

    /// True when provably empty.
    pub fn is_empty(&self) -> bool {
        !self.unknown && self.paths.is_empty()
    }

    /// Add a resolved path.
    pub fn insert(&mut self, p: camino::Utf8PathBuf) {
        if !self.paths.contains(&p) {
            self.paths.push(p);
        }
    }

    /// Union in place.
    pub fn union(&mut self, other: &FileSet) {
        self.unknown |= other.unknown;
        for p in &other.paths {
            self.insert(p.clone());
        }
    }
}
