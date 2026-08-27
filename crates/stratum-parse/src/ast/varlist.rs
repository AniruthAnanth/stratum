//! The varlist AST — design 02 §7 / [U] 11.4.
//!
//! This file is the SHAPE of a varlist; [`crate::varlist`] is the resolution of
//! one against a live dataset. They are separate because the shape is needed by
//! the speculative parser with no dataset in sight, and the resolution needs a
//! `VarIndex` the editor process does not have.

use stratum_proto::{Span, StorageType};

use crate::ast::expr::NumList;

/// A varlist as typed. Not deduplicated and not sorted: repetition is legal and
/// meaningful in an existing-varlist ([U] 11.4.1), and order is the output
/// order of every command that takes one.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct VarList {
    /// The items, in the order typed.
    pub items: Vec<VarItem>,
    /// Extent in the text this was parsed from.
    pub span: Span,
}

impl VarList {
    /// True when any atom is a [`VarPattern::Hole`].
    pub fn has_hole(&self) -> bool {
        self.items.iter().any(|i| match &i.kind {
            VarItemKind::Single(a) => matches!(a.base, VarPattern::Hole { .. }),
            VarItemKind::Interact { atoms, .. } => atoms
                .iter()
                .any(|a| matches!(a.base, VarPattern::Hole { .. })),
        })
    }
}

/// One item of a varlist: a single atom, or an interaction of several.
#[derive(Clone, PartialEq, Debug)]
pub struct VarItem {
    /// Extent of the item.
    pub span: Span,
    /// What kind of item.
    pub kind: VarItemKind,
}

/// A varlist item.
#[derive(Clone, PartialEq, Debug)]
pub enum VarItemKind {
    /// One atom.
    Single(VarAtom),
    /// `a#b` (`full = false`) or `a##b` (`full = true`) — [U] 11.4.3.
    Interact {
        /// The interacted atoms, in order.
        atoms: Vec<VarAtom>,
        /// `##` — include every lower-order term.
        full: bool,
    },
}

/// One variable reference, with its optional time-series and factor-variable
/// operators.
#[derive(Clone, PartialEq, Debug)]
pub struct VarAtom {
    /// `L.`, `F2.`, `D.`, `S.`, or a chain of them.
    pub ts: Option<TsOp>,
    /// `i.`, `c.`, `o.`.
    pub fv: Option<FvOp>,
    /// The name or pattern itself.
    pub base: VarPattern,
    /// Extent of the whole atom, operators included.
    pub span: Span,
}

/// How a varlist atom names variables.
#[derive(Clone, PartialEq, Debug)]
pub enum VarPattern {
    /// A bare name: exact match first, then unique-prefix abbreviation unless
    /// `set varabbrev off` ([U] 11.2.3).
    Name(String),
    /// Contains `*` and/or `?`. Zero or more matches, in storage order.
    Glob(String),
    /// Contains `~`. Must match exactly ONE variable, else r(111).
    Tilde(String),
    /// `a-b`. For existing variables this is a STORAGE-order range; for new
    /// variables it is a numeric-suffix range on a common alphabetic stub.
    Range {
        /// Left endpoint as typed.
        lo: String,
        /// Right endpoint as typed.
        hi: String,
    },
    /// `_all`, or a bare `*`.
    All,
    /// `str8 name`, `int(a b)` — a storage-type filter over an inner list.
    Typed {
        /// The storage type named.
        ty: StorageType,
        /// The inner patterns.
        inner: Vec<VarPattern>,
    },
    /// `v:lblname` — variables carrying a given value label.
    Labeled {
        /// Variable name or pattern.
        name: String,
        /// Value-label name.
        label: String,
    },
    /// Speculative parse only: an unexpanded macro reference where a name
    /// belongs.
    Hole {
        /// Extent of the macro reference.
        src: Span,
    },
}

impl VarPattern {
    /// The text this pattern was written as, for diagnostics.
    pub fn as_text(&self) -> &str {
        match self {
            VarPattern::Name(s) | VarPattern::Glob(s) | VarPattern::Tilde(s) => s,
            VarPattern::Labeled { name, .. } => name,
            VarPattern::Range { lo, .. } => lo,
            VarPattern::All => "_all",
            VarPattern::Typed { .. } => "",
            VarPattern::Hole { .. } => "",
        }
    }
}

/// A time-series operator ([U] 11.4.4). Parsed in v1, evaluated in v1.5 by
/// `stratum-stats` once `tsset` exists (ARCHITECTURE C43).
#[derive(Clone, PartialEq, Debug)]
pub enum TsOp {
    /// `L.`, `L2.`, `L(1/4).`.
    L(TsLag),
    /// `F.`, `F2.`, `F(1/4).`.
    F(TsLag),
    /// `D.`, `D2.`.
    D(u32),
    /// `S.`, `S2.`.
    S(u32),
    /// `L.D.x` — applied right to left.
    Chain(Vec<TsOp>),
}

/// The lag/lead count of a time-series operator.
#[derive(Clone, PartialEq, Debug)]
pub enum TsLag {
    /// `L2.`.
    Fixed(i32),
    /// `L(1/4).`.
    List(NumList),
}

/// A factor-variable operator ([U] 11.4.3).
#[derive(Clone, PartialEq, Debug)]
pub enum FvOp {
    /// `i.`, `ib2.`, `i(1 3).`.
    I {
        /// Which level is the base.
        base: Option<BaseLevel>,
        /// An explicit level restriction.
        levels: Option<NumList>,
    },
    /// `c.` — treat as continuous.
    C,
    /// `o.` — omit.
    O,
}

/// The base level of an `i.` operator.
#[derive(Clone, PartialEq, Debug)]
pub enum BaseLevel {
    /// `ib0.`, `ib2.` — a named level.
    Value(f64),
    /// `ibn.` — no base level.
    None,
    /// `ib(first).`.
    First,
    /// `ib(last).`.
    Last,
    /// `ib(freq).`.
    Freq,
}
