//! Engine-side Data-Editor view orders (A13).
//!
//! # What the amendment actually changed
//!
//! `PageRequest.order` used to be `Option<Vec<u64>>` — "a permutation of
//! observation indices". Its only sender is the webview, which serialises its
//! request arguments as JSON. A sorted 10 M-row view therefore meant **80 MB of
//! JSON per 40-row scroll frame**, against `06` §15's 12 ms budget, produced by
//! a sender that could not have built the permutation in the first place
//! (`06` §15.3 simultaneously requires that sorting happen in Rust and never in
//! the frontend).
//!
//! So the frontend declares *intent* once — an [`OrderSpec`]: sort keys plus an
//! optional `if` filter — and receives an [`OrderId`], a `u32`. Every subsequent
//! page request carries that `u32`. The permutation is computed here, stays
//! here, and never crosses the wire in either direction.
//!
//! # The filter is evaluated by the caller
//!
//! [`OrderSpec::filter`] is "an ordinary Stata `if` expression evaluated by the
//! engine". Evaluating one needs the parser and the expression evaluator, which
//! are *above* this crate (ARCHITECTURE §8.1: `stratum-data` depends on nothing
//! but `stratum-core` and `stratum-proto`). The caller — the session layer that
//! already owns an evaluator — passes the result down as a [`Sample`], and
//! [`OrderRegistry::set`] refuses a spec that names a filter without one rather
//! than quietly ignoring it.
//!
//! # Identity is not materialised
//!
//! `keys` empty and no filter is the Data Editor's opening state and its most
//! common one. That order stores no permutation at all: [`Rows::Identity`] is
//! `nobs`, so registering it costs 24 bytes rather than 80 MB, and
//! [`ViewOrder::row`] on it is the identity function. Under §0a we would pay the
//! 80 MB for speed if it bought any; it buys nothing.

use std::sync::{Arc, Mutex};

use rustc_hash::FxHashMap;
use stratum_proto::{DatasetStateId, OrderId, OrderSpec};

use crate::frame::FrameSnapshot;
use crate::perf::{bump, counters};
use crate::sample::Sample;
use crate::sort::{self, SortError, Strategy};

/// How a view order stores its row mapping.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Rows {
    /// Dataset order, unfiltered. No permutation exists.
    Identity {
        /// `_N` at the moment the order was computed.
        nobs: u64,
    },
    /// View row `i` shows dataset row `perm[i]`.
    Perm(Arc<[u64]>),
}

/// A computed view order, held behind an [`OrderId`].
#[derive(Clone, Debug)]
pub struct ViewOrder {
    /// The handle this order answers to.
    pub id: OrderId,
    /// The snapshot it was computed against. A page request naming a different
    /// state is stale, and [`OrderRegistry::get_for_state`] says so.
    pub state: DatasetStateId,
    /// The spec that produced it, kept so `data_order_set` with the same intent
    /// can be answered from the registry instead of recomputed.
    pub spec: OrderSpec,
    /// The mapping.
    pub rows: Rows,
}

impl ViewOrder {
    /// How many rows the view has. With a filter this is smaller than `_N`.
    #[must_use]
    pub fn len(&self) -> u64 {
        match &self.rows {
            Rows::Identity { nobs } => *nobs,
            Rows::Perm(p) => p.len() as u64,
        }
    }

    /// True when the view shows nothing — every observation filtered out.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The dataset observation shown at view row `i`, or `None` past the end.
    #[inline]
    #[must_use]
    pub fn row(&self, i: u64) -> Option<u64> {
        match &self.rows {
            Rows::Identity { nobs } => (i < *nobs).then_some(i),
            Rows::Perm(p) => p.get(usize::try_from(i).ok()?).copied(),
        }
    }

    /// True when no permutation was materialised.
    #[must_use]
    pub fn is_identity(&self) -> bool {
        matches!(self.rows, Rows::Identity { .. })
    }

    /// Bytes the permutation retains, for the Q9 accounting.
    #[must_use]
    pub fn heap_bytes(&self) -> u64 {
        match &self.rows {
            Rows::Identity { .. } => 0,
            Rows::Perm(p) => (p.len() * 8) as u64,
        }
    }
}

/// Why an order could not be established or used.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum OrderError {
    /// A sort key names a variable the snapshot does not have.
    #[error("variable {0} not found")]
    NoSuchVar(stratum_proto::VarIdx),
    /// The spec carries an `if` expression but no evaluated sample came with it.
    /// See this module's header: the evaluator lives above this crate.
    #[error("order spec names filter {0:?} but no evaluated sample was supplied")]
    FilterNotEvaluated(String),
    /// A sample was supplied for a different frame length.
    #[error("filter covers {got} observations, the frame has {want}")]
    FilterLengthMismatch {
        /// `Sample::nobs` of what was passed.
        got: u64,
        /// `_N` of the snapshot.
        want: u64,
    },
    /// The spec was computed against a snapshot that has since moved.
    #[error("order was computed against {had} and the frame is now at {now}")]
    Stale {
        /// The state the order holds.
        had: DatasetStateId,
        /// The state the caller asked about.
        now: DatasetStateId,
    },
    /// No such handle: never allocated, or already dropped.
    #[error("no such view order: {0}")]
    NoSuchOrder(OrderId),
    /// The sort itself refused.
    #[error(transparent)]
    Sort(#[from] SortError),
}

impl OrderError {
    /// Stata's return code, for the rare case one of these surfaces as an error
    /// message rather than as a UI invalidation.
    #[must_use]
    pub fn rc(&self) -> u16 {
        match self {
            OrderError::NoSuchVar(_) => 111,
            OrderError::Stale { .. } | OrderError::NoSuchOrder(_) => 459,
            OrderError::Sort(_) => 900,
            _ => 198,
        }
    }
}

/// The session's live view orders.
///
/// One per session (`OrderId` is "scoped to one session" — CONTRACTS §1). Shared
/// behind a `Mutex` rather than `&mut` because the asset-protocol handler that
/// serves `stratum-asset://…/page` runs off the control thread and only ever
/// takes the lock to clone one `Arc`.
#[derive(Debug, Default)]
pub struct OrderRegistry {
    inner: Mutex<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    next: u32,
    live: FxHashMap<OrderId, Arc<ViewOrder>>,
}

impl OrderRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Compute an order and hand back its handle — `data_order_set`.
    ///
    /// `filter` is the evaluated form of [`OrderSpec::filter`]; pass `None` when
    /// the spec names none. The permutation is computed here and is not
    /// reachable through any serialisable type.
    ///
    /// # Errors
    ///
    /// [`OrderError`].
    pub fn set(
        &self,
        snap: &FrameSnapshot,
        spec: &OrderSpec,
        filter: Option<&Sample>,
    ) -> Result<OrderId, OrderError> {
        let rows = plan(snap, spec, filter)?;
        let mut inner = self.inner.lock().expect("order registry mutex poisoned");
        inner.next += 1;
        let id = OrderId(inner.next);
        let order = Arc::new(ViewOrder {
            id,
            state: DatasetStateId::from(snap.version()),
            spec: spec.clone(),
            rows,
        });
        inner.live.insert(id, order);
        Ok(id)
    }

    /// Look one up — `None` once it has been dropped.
    #[must_use]
    pub fn get(&self, id: OrderId) -> Option<Arc<ViewOrder>> {
        self.inner
            .lock()
            .expect("order registry mutex poisoned")
            .live
            .get(&id)
            .cloned()
    }

    /// Look one up and check it against the state a request believes it is
    /// showing.
    ///
    /// # Errors
    ///
    /// [`OrderError::NoSuchOrder`] or [`OrderError::Stale`].
    pub fn get_for_state(
        &self,
        id: OrderId,
        state: DatasetStateId,
    ) -> Result<Arc<ViewOrder>, OrderError> {
        let o = self.get(id).ok_or(OrderError::NoSuchOrder(id))?;
        if o.state != state {
            return Err(OrderError::Stale {
                had: o.state,
                now: state,
            });
        }
        Ok(o)
    }

    /// `data_order_drop`. Answers whether the handle was live.
    pub fn drop_order(&self, id: OrderId) -> bool {
        self.inner
            .lock()
            .expect("order registry mutex poisoned")
            .live
            .remove(&id)
            .is_some()
    }

    /// Drop every order not computed against `state`.
    ///
    /// The frame moved, so their permutations describe rows that may no longer
    /// exist. Returns how many were released — 80 MB apiece for a 10 M-row
    /// order, which is why this is called on `FrameChanged` rather than left to
    /// the session's teardown.
    pub fn invalidate_except(&self, state: DatasetStateId) -> usize {
        let mut inner = self.inner.lock().expect("order registry mutex poisoned");
        let before = inner.live.len();
        inner.live.retain(|_, o| o.state == state);
        before - inner.live.len()
    }

    /// How many orders are live.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner
            .lock()
            .expect("order registry mutex poisoned")
            .live
            .len()
    }

    /// True when nothing is registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Bytes every live permutation retains.
    #[must_use]
    pub fn heap_bytes(&self) -> u64 {
        self.inner
            .lock()
            .expect("order registry mutex poisoned")
            .live
            .values()
            .map(|o| o.heap_bytes())
            .sum()
    }
}

/// The plan's own spelling of `data_order_set`: `order::set(frame, spec) -> OrderId`.
///
/// # Errors
///
/// [`OrderError`].
pub fn set(
    reg: &OrderRegistry,
    snap: &FrameSnapshot,
    spec: &OrderSpec,
    filter: Option<&Sample>,
) -> Result<OrderId, OrderError> {
    reg.set(snap, spec, filter)
}

/// Compute the row mapping for a spec. Separated from registration so a test can
/// assert the mapping without an `OrderId` in the way.
///
/// # Errors
///
/// [`OrderError`].
pub fn plan(
    snap: &FrameSnapshot,
    spec: &OrderSpec,
    filter: Option<&Sample>,
) -> Result<Rows, OrderError> {
    let nobs = snap.n_obs();
    match (spec.filter.as_deref(), filter) {
        (Some(expr), None) => return Err(OrderError::FilterNotEvaluated(expr.to_owned())),
        (_, Some(s)) if s.nobs() != nobs => {
            return Err(OrderError::FilterLengthMismatch {
                got: s.nobs(),
                want: nobs,
            })
        }
        _ => {}
    }
    // A filter that selects everything is not a filter. Collapsing it here is
    // what keeps `if 1` from materialising 80 MB.
    let filter = filter.filter(|s| s.len() != nobs);

    if spec.keys.is_empty() {
        let Some(sample) = filter else {
            return Ok(Rows::Identity { nobs });
        };
        // Dataset order, filtered: the sample is already ascending, so walking
        // its runs *is* the view order and no sort happens at all.
        let mut out: Vec<u64> = Vec::with_capacity(sample.len() as usize);
        for run in sample.runs() {
            out.extend(run.start..run.start + run.len);
        }
        bump(&counters().rows_touched, nobs);
        return Ok(Rows::Perm(out.into()));
    }

    let mut cols = Vec::with_capacity(spec.keys.len());
    for &(idx, dir) in &spec.keys {
        cols.push((snap.col(idx).ok_or(OrderError::NoSuchVar(idx))?, dir));
    }
    let perm = sort::permutation(&cols, nobs, Strategy::Auto)?;
    let out: Vec<u64> = match filter {
        None => perm.into_iter().map(u64::from).collect(),
        Some(s) => perm
            .into_iter()
            .map(u64::from)
            .filter(|&r| s.contains(r))
            .collect(),
    };
    Ok(Rows::Perm(out.into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::Frame;
    use stratum_proto::{SortDir, StorageType, VarIdx};

    fn frame(vals: &[f64]) -> Frame {
        let mut f = Frame::new("default");
        f.set_n_obs(vals.len() as u64);
        f.add_var("x", StorageType::Double).expect("fresh");
        f.begin_command();
        {
            let mut c = f.col_mut(VarIdx(0)).expect("just added");
            for (i, &v) in vals.iter().enumerate() {
                c.set_f64(i as u64, v).expect("double takes anything");
            }
        }
        f.commit();
        f
    }

    fn spec(keys: Vec<(VarIdx, SortDir)>, filter: Option<&str>) -> OrderSpec {
        OrderSpec {
            keys,
            filter: filter.map(str::to_owned),
            state: DatasetStateId(0),
        }
    }

    #[test]
    fn no_keys_and_no_filter_materialises_nothing() {
        let f = frame(&[3.0, 1.0, 2.0]);
        let reg = OrderRegistry::new();
        let id = reg.set(&f.snapshot(), &spec(vec![], None), None).unwrap();
        let o = reg.get(id).expect("live");
        assert!(o.is_identity());
        assert_eq!(o.heap_bytes(), 0);
        assert_eq!(o.row(2), Some(2));
        assert_eq!(o.row(3), None);
    }

    #[test]
    fn a_descending_key_reverses_the_view_without_touching_the_frame() {
        let f = frame(&[3.0, 1.0, 2.0]);
        let snap = f.snapshot();
        let reg = OrderRegistry::new();
        let id = reg
            .set(&snap, &spec(vec![(VarIdx(0), SortDir::Desc)], None), None)
            .unwrap();
        let o = reg.get(id).unwrap();
        assert_eq!((o.row(0), o.row(1), o.row(2)), (Some(0), Some(2), Some(1)));
        // The frame is untouched: `04` §13 — "the core NEVER mutates the frame
        // to satisfy a UI sort".
        assert_eq!(f.col(VarIdx(0)).unwrap().get_f64(0), Some(3.0));
    }

    #[test]
    fn a_filter_without_an_evaluated_sample_is_refused() {
        let f = frame(&[1.0, 2.0]);
        let reg = OrderRegistry::new();
        let e = reg
            .set(&f.snapshot(), &spec(vec![], Some("x > 1")), None)
            .expect_err("the evaluator lives above this crate");
        assert!(matches!(e, OrderError::FilterNotEvaluated(_)));
    }

    #[test]
    fn a_filtered_order_holds_only_the_surviving_rows_in_key_order() {
        let f = frame(&[5.0, 1.0, 9.0, 3.0]);
        let snap = f.snapshot();
        let mut bits = crate::bitset::BitSet::new(4);
        bits.set(0, true);
        bits.set(2, true);
        bits.set(3, true);
        let s = Sample::mask(4, bits);
        let reg = OrderRegistry::new();
        let id = reg
            .set(
                &snap,
                &spec(vec![(VarIdx(0), SortDir::Asc)], Some("x > 2")),
                Some(&s),
            )
            .unwrap();
        let o = reg.get(id).unwrap();
        assert_eq!(o.len(), 3);
        // values 3, 5, 9 -> rows 3, 0, 2
        assert_eq!(
            (o.row(0), o.row(1), o.row(2), o.row(3)),
            (Some(3), Some(0), Some(2), None)
        );
    }

    #[test]
    fn a_filter_that_selects_everything_stays_identity() {
        let f = frame(&[1.0, 2.0, 3.0]);
        let reg = OrderRegistry::new();
        let id = reg
            .set(
                &f.snapshot(),
                &spec(vec![], Some("1")),
                Some(&Sample::all(3)),
            )
            .unwrap();
        assert!(reg.get(id).unwrap().is_identity());
    }

    #[test]
    fn a_stale_order_is_named_as_stale_rather_than_served() {
        let f = frame(&[1.0, 2.0]);
        let reg = OrderRegistry::new();
        let id = reg.set(&f.snapshot(), &spec(vec![], None), None).unwrap();
        let now = DatasetStateId(9999);
        assert!(matches!(
            reg.get_for_state(id, now),
            Err(OrderError::Stale { .. })
        ));
        assert_eq!(reg.invalidate_except(now), 1);
        assert!(reg.is_empty());
        assert!(matches!(
            reg.get_for_state(id, now),
            Err(OrderError::NoSuchOrder(_))
        ));
    }

    #[test]
    fn handles_are_never_reused_after_a_drop() {
        let f = frame(&[1.0]);
        let reg = OrderRegistry::new();
        let a = reg.set(&f.snapshot(), &spec(vec![], None), None).unwrap();
        assert!(reg.drop_order(a));
        assert!(!reg.drop_order(a));
        let b = reg.set(&f.snapshot(), &spec(vec![], None), None).unwrap();
        assert_ne!(
            a, b,
            "a reused handle would serve one view's rows to another"
        );
    }
}
