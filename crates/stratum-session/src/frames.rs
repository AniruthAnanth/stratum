//! Frame lifecycle, and clean-state items 1 and 2.
//!
//! `stratum-data` owns a `FrameSet` — the frames themselves, their columns and
//! their write barrier. What it does *not* own is everything a frame is declared
//! to be: `tsset`, `xtset`, `svyset`, `mi set` and the `frlink` graph are session
//! state that happens to be keyed by frame name, and they have to travel with the
//! frame through `frame rename`, `frame copy` and `frame drop` or they become a
//! binding that outlives its data.
//!
//! [`Frames`] is that pairing. Every lifecycle verb goes through it rather than
//! through `FrameSet` directly, which is what makes "drop the frame, keep its
//! `tsset`" unrepresentable rather than merely unlikely.

use std::cmp::Ordering;
use std::sync::Arc;

use indexmap::IndexMap;
use stratum_data::{FrameSet, FrameSetError};

/// How a frame was declared to `tsset` / `xtset`.
///
/// One type for both because they are the same declaration with a different
/// default: `xtset` requires a panel variable, `tsset` allows one.
#[derive(Clone, PartialEq, Debug)]
pub struct TimeSeriesSet {
    /// `xtset panelvar timevar` — `None` for a plain `tsset timevar`.
    pub panelvar: Option<String>,
    /// The time variable. `None` for `xtset panelvar` with no time dimension.
    pub timevar: Option<String>,
    /// `, delta()`. One unless the declaration said otherwise.
    pub delta: f64,
    /// The `%t*` format the time variable carries, recorded so `tsset` can
    /// report it back without re-reading the frame.
    pub format: Option<String>,
}

/// How a frame was declared to `svyset`.
#[derive(Clone, PartialEq, Debug)]
pub struct SurveySet {
    /// The primary sampling unit varlist, `_n` for observation-level.
    pub psu: Option<String>,
    /// `[pweight=...]`.
    pub weight: Option<String>,
    /// `, strata()`.
    pub strata: Option<String>,
}

/// The `mi set` style a frame is in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MiStyle {
    /// `mi set wide`.
    Wide,
    /// `mi set mlong`.
    Mlong,
    /// `mi set flong`.
    Flong,
    /// `mi set flongsep`.
    Flongsep,
}

/// Everything declared *about* a frame that is not stored *in* it.
///
/// Item 2 of the checklist is "no `sortedby`, no `tsset`/`xtset`/`svyset`/`mi
/// set`". `sortedby` lives on the frame (`stratum_data::SortState`) because a
/// write to a key column has to invalidate it; the other four live here.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct DatasetBindings {
    /// `tsset`.
    pub tsset: Option<TimeSeriesSet>,
    /// `xtset`.
    pub xtset: Option<TimeSeriesSet>,
    /// `svyset`.
    pub svyset: Option<SurveySet>,
    /// `mi set`.
    pub mi: Option<MiStyle>,
}

impl DatasetBindings {
    /// True when nothing has been declared. Item 2's half of the audit.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        *self == DatasetBindings::default()
    }
}

/// One `frlink` edge. **Never populated in v1** — `frlink`/`frget`/`frval` need
/// `.dta` format 120's alias variables, and `stratum-data` says so — but item 1
/// of the checklist reads "no `frlink`s", and an item that cannot be represented
/// cannot be asserted. The vector exists so the assertion is about state rather
/// than about the absence of a feature.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FrameLink {
    /// The frame holding the link.
    pub from: String,
    /// The frame linked to.
    pub to: String,
    /// The match variables, `from`-side then `to`-side.
    pub keys: Vec<(String, String)>,
}

/// The session's frames, their declarations, and the link graph.
#[derive(Debug)]
pub struct Frames {
    set: FrameSet,
    /// Keyed by frame name and kept in step with every lifecycle verb.
    /// Insertion-ordered so that a diagnostic listing declarations prints in
    /// `frames dir` order rather than in hash order.
    bindings: IndexMap<Arc<str>, DatasetBindings>,
    links: Vec<FrameLink>,
}

impl Default for Frames {
    fn default() -> Self {
        Self::fresh()
    }
}

impl Frames {
    /// Checklist items 1 and 2: one frame called `default`, 0 obs, 0 vars,
    /// current, with nothing declared about it and no links.
    ///
    /// Constructed, not cleared. `FrameSet::new` is itself a constructor, so
    /// there is no path here that could drop fifteen frames and forget the
    /// sixteenth.
    #[must_use]
    pub fn fresh() -> Self {
        let set = FrameSet::new();
        let mut bindings = IndexMap::with_capacity(1);
        bindings.insert(Arc::clone(set.current_name()), DatasetBindings::default());
        Self {
            set,
            bindings,
            links: Vec::new(),
        }
    }

    /// The frames themselves.
    #[must_use]
    pub fn set(&self) -> &FrameSet {
        &self.set
    }

    /// The frames themselves, mutably. This is the write path for the data;
    /// the lifecycle verbs below are the write path for the *set*.
    pub fn set_mut(&mut self) -> &mut FrameSet {
        &mut self.set
    }

    /// What is declared about the current frame.
    #[must_use]
    pub fn bindings(&self) -> &DatasetBindings {
        self.bindings
            .get(self.set.current_name())
            .expect("every frame in the set has a bindings entry")
    }

    /// What is declared about the current frame, mutably.
    pub fn bindings_mut(&mut self) -> &mut DatasetBindings {
        let key = Arc::clone(self.set.current_name());
        self.bindings
            .get_mut(&key)
            .expect("every frame in the set has a bindings entry")
    }

    /// What is declared about `name`.
    #[must_use]
    pub fn bindings_of(&self, name: &str) -> Option<&DatasetBindings> {
        self.bindings.get(name)
    }

    /// The `frlink` graph. Empty in v1.
    #[must_use]
    pub fn links(&self) -> &[FrameLink] {
        &self.links
    }

    /// `frame create`.
    ///
    /// # Errors
    ///
    /// Whatever [`FrameSet::create`] refuses.
    pub fn create(&mut self, name: &str) -> Result<(), FrameSetError> {
        self.set.create(name)?;
        self.bindings
            .insert(Arc::from(name), DatasetBindings::default());
        Ok(())
    }

    /// `frame change` / `cwf`.
    ///
    /// # Errors
    ///
    /// Whatever [`FrameSet::change`] refuses.
    pub fn change(&mut self, name: &str) -> Result<(), FrameSetError> {
        self.set.change(name)
    }

    /// `frame drop`. The declarations and every link touching the frame go with
    /// it — a `tsset` that outlived its data would be read back by the next
    /// `frame create` of the same name.
    ///
    /// # Errors
    ///
    /// Whatever [`FrameSet::drop_frame`] refuses.
    pub fn drop_frame(&mut self, name: &str) -> Result<(), FrameSetError> {
        self.set.drop_frame(name)?;
        self.bindings.shift_remove(name);
        self.links.retain(|l| l.from != name && l.to != name);
        Ok(())
    }

    /// `frame rename`.
    ///
    /// # Errors
    ///
    /// Whatever [`FrameSet::rename`] refuses.
    pub fn rename(&mut self, from: &str, to: &str) -> Result<(), FrameSetError> {
        self.set.rename(from, to)?;
        // Rebuilt rather than removed-and-reinserted, for the reason
        // `FrameSet::rename` gives: `frames dir` order is user-visible.
        let old = std::mem::take(&mut self.bindings);
        self.bindings = old
            .into_iter()
            .map(|(k, v)| {
                if &*k == from {
                    (Arc::from(to), v)
                } else {
                    (k, v)
                }
            })
            .collect();
        for l in &mut self.links {
            if l.from == from {
                l.from = to.to_owned();
            }
            if l.to == from {
                l.to = to.to_owned();
            }
        }
        Ok(())
    }

    /// `frame copy`. The copy inherits the declarations, because the data it
    /// inherits is what they were declared over.
    ///
    /// # Errors
    ///
    /// Whatever [`FrameSet::copy`] refuses.
    pub fn copy(&mut self, from: &str, to: &str) -> Result<(), FrameSetError> {
        self.set.copy(from, to)?;
        let b = self.bindings.get(from).cloned().unwrap_or_default();
        self.bindings.insert(Arc::from(to), b);
        Ok(())
    }

    /// Checklist item 1 alone: exactly one frame, called `default`, current,
    /// and no `frlink`s.
    #[must_use]
    pub fn is_clean_frames(&self) -> bool {
        self.set.len() == 1
            && self.links.is_empty()
            && &**self.set.current_name() == stratum_data::frames::DEFAULT
    }

    /// Checklist item 2 alone: every frame in the set is empty, unsorted,
    /// unlabelled, has no notes or characteristics and no value-label tables,
    /// and has nothing declared about it.
    ///
    /// Every frame, not just the current one. Item 2 is written in the singular
    /// because item 1 has already established there is only one — but a
    /// predicate that only looked at the current frame would pass on a session
    /// holding a loaded `aux` frame, and the two items are asserted separately.
    #[must_use]
    pub fn is_clean_dataset(&self) -> bool {
        self.set.names().iter().all(|name| {
            let Some(f) = self.set.get(name) else {
                return false;
            };
            f.n_obs() == 0
                && f.n_vars() == 0
                && f.label().is_empty()
                && f.chars().is_empty()
                && f.labels().is_empty()
                && f.sort_state().keys.is_empty()
                && self
                    .bindings
                    .get(name)
                    .is_some_and(DatasetBindings::is_clean)
        })
    }

    /// Items 1 and 2 together.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.is_clean_frames() && self.is_clean_dataset()
    }
}

/// Deep structural equality, because `stratum_data::Frame` deliberately has no
/// `PartialEq`.
///
/// A frame's identity for this purpose is what a user can observe: which frames
/// exist, in what order, which is current, and for each — shape, variable
/// metadata, value labels, characteristics, sort state, dataset label, and the
/// content digest of every column. The digests are what make this an equality
/// over *data* rather than over *shape*; `Frame::digest` is blake3-128 per
/// column, so two frames that differ in one cell compare unequal.
///
/// This is the comparison the `fresh_checklist` construct-don't-reset test runs
/// on, so it is deliberately expensive and deliberately total.
impl PartialEq for Frames {
    fn eq(&self, other: &Self) -> bool {
        if self.links != other.links
            || self.set.current_name() != other.set.current_name()
            || self.set.len() != other.set.len()
        {
            return false;
        }
        let (mine, theirs) = (self.set.names(), other.set.names());
        if mine != theirs {
            return false;
        }
        for name in &mine {
            if self.bindings.get(name) != other.bindings.get(name) {
                return false;
            }
            let (a, b) = match (self.set.get(name), other.set.get(name)) {
                (Some(a), Some(b)) => (a, b),
                _ => return false,
            };
            if a.n_obs() != b.n_obs()
                || a.n_vars() != b.n_vars()
                || a.label() != b.label()
                || a.chars() != b.chars()
                || a.labels() != b.labels()
                || a.sort_state() != b.sort_state()
                || a.vars() != b.vars()
            {
                return false;
            }
            for i in 0..a.n_vars() {
                let idx = stratum_proto::VarIdx(i);
                if a.digest(idx) != b.digest(idx) {
                    return false;
                }
            }
        }
        true
    }
}

/// Checklist item 16, as a function.
///
/// Byte-wise UTF-8 order, never the OS locale's. `Ord for str` is already
/// byte-wise, so this is a one-liner — but it is a *named* one-liner, because
/// the alternative (every call site writing `a.cmp(b)` and one of them reaching
/// for `to_lowercase()` on a rainy afternoon) is exactly how a collation rule
/// stops being a rule. `docs/design/03` §8 item 16 accepts that this can differ
/// from Stata under a non-C locale and flags it for differential testing; being
/// identical on macOS, Windows and Linux is the property we chose.
#[must_use]
pub fn collate(a: &str, b: &str) -> Ordering {
    a.as_bytes().cmp(b.as_bytes())
}
