//! The expression AST — design 02 §8.4, and the precedence contract of §8.1.
//!
//! # The precedence table is the whole point of this file
//!
//! Stata's operators are **not** grouped into the familiar `* /` and `+ -`
//! levels. Every one of them is its own level, and all of them — `^` included —
//! are LEFT-associative. [`BinOp::prec`] is the machine-readable form of 02
//! §8.1's verified table, and `tests/expr.rs` re-derives every row of it from a
//! parsed answer rather than trusting the numbers here.
//!
//! Two rows disagree with [U] 13.2.5 and follow the machine instead:
//!
//! * `^` is **left**-associative: `2^3^2` is `64`, not `512` [V].
//! * `^` binds TIGHTER than `!`: `!2^0` is `0`, i.e. `!(2^0)` [V]. The manual
//!   lists `!` above `^`.

use stratum_proto::Span;

use crate::ast::varlist::VarAtom;

/// One parsed Stata expression. Design 02 §8.4.
///
/// Design 02 spells the owned strings `compact_str::CompactString`; they are
/// `String` here for the reason `ast/command.rs` records — `compact_str` is not
/// in the workspace dependency table, and a member crate reaching outside that
/// table is how a workspace ends up resolving two versions of one crate.
#[derive(Clone, PartialEq, Debug)]
pub enum Expr {
    /// A numeric literal.
    Num(f64, Span),
    /// `.` (tag 0) or `.a`..=`.z` (tags 1..=26). Stored as the TAG and not as
    /// the `f64`: `stratum_core::missing::missing_f64` is the one place the bit
    /// pattern is built, and an AST that carried the double would be a second.
    Missing(u8, Span),
    /// A string literal, with the quoting already removed.
    Str(String, Span),
    /// A bare name. Resolution order at eval time: variable → scalar → error.
    Name(String, Span),
    /// `_n`, `_N`, `_pi`, `_rc`.
    Sys(SysVar, Span),
    /// `x[exp]` — observation subscript. Out of range or missing subscript
    /// yields `.` / `""`, never an error (02 §8.4).
    Index {
        /// The subscripted expression.
        base: Box<Expr>,
        /// The subscript.
        idx: Box<Expr>,
        /// Extent of the whole `base[idx]`.
        span: Span,
    },
    /// `-x`, `+x`, `!x`.
    Unary {
        /// Which prefix operator.
        op: UnOp,
        /// Operand.
        rhs: Box<Expr>,
        /// Extent of operator and operand.
        span: Span,
    },
    /// `a + b`.
    Binary {
        /// Which infix operator.
        op: BinOp,
        /// Left operand.
        lhs: Box<Expr>,
        /// Right operand.
        rhs: Box<Expr>,
        /// Extent of the whole application.
        span: Span,
    },
    /// `f(a, b)`.
    Call {
        /// Function name as typed.
        name: String,
        /// Arguments in order.
        args: Vec<Expr>,
        /// Extent of the whole call.
        span: Span,
    },
    /// `( e )`. Kept in the tree so a quick fix can rewrite the source without
    /// re-deriving where the user's own parentheses were.
    Paren(Box<Expr>, Span),
    /// `r(mean)`, `e(N)`, `c(k)`, `s(x)`.
    Stored {
        /// Which class.
        class: StoredClass,
        /// The key expression — it may itself be a macro-expanded name.
        key: Box<Expr>,
        /// Extent of the whole reference.
        span: Span,
    },
    /// `_b[price]`, `_se[_cons]`, `_coef[price]`.
    Coef {
        /// Which family.
        kind: CoefKind,
        /// The key expression.
        key: Box<Expr>,
        /// Extent of the whole reference.
        span: Span,
    },
    /// `M[i,j]`.
    MatElem {
        /// Matrix name.
        name: String,
        /// Row subscript.
        i: Box<Expr>,
        /// Column subscript.
        j: Box<Expr>,
        /// Extent of the whole reference.
        span: Span,
    },
    /// `i.rep78` / `L.gnp` appearing inside an expression.
    Term(Box<VarAtom>, Span),
    /// Speculative parse only: an unexpanded `` `x' `` or `$x` standing in for
    /// anything at all. Never produced by [`crate::parse::ParseMode::Execute`].
    Hole {
        /// Extent of the macro reference in the text being parsed.
        src: Span,
    },
}

impl Expr {
    /// Extent of this expression in the text it was parsed from.
    pub fn span(&self) -> Span {
        match self {
            Expr::Num(_, s)
            | Expr::Missing(_, s)
            | Expr::Str(_, s)
            | Expr::Name(_, s)
            | Expr::Sys(_, s)
            | Expr::Term(_, s)
            | Expr::Paren(_, s)
            | Expr::Hole { src: s } => *s,
            Expr::Index { span, .. }
            | Expr::Unary { span, .. }
            | Expr::Binary { span, .. }
            | Expr::Call { span, .. }
            | Expr::Stored { span, .. }
            | Expr::Coef { span, .. }
            | Expr::MatElem { span, .. } => *span,
        }
    }

    /// Visit every node, parents before children.
    ///
    /// The "Used by" column of spec §20 and lint `L002` are both a walk of this
    /// shape, and writing it twice is how they end up disagreeing about whether
    /// a name inside a function argument counts.
    pub fn walk(&self, f: &mut impl FnMut(&Expr)) {
        f(self);
        match self {
            Expr::Index { base, idx, .. } => {
                base.walk(f);
                idx.walk(f);
            }
            Expr::Unary { rhs, .. } => rhs.walk(f),
            Expr::Binary { lhs, rhs, .. } => {
                lhs.walk(f);
                rhs.walk(f);
            }
            Expr::Call { args, .. } => {
                for a in args {
                    a.walk(f);
                }
            }
            Expr::Paren(inner, _) => inner.walk(f),
            Expr::Stored { key, .. } | Expr::Coef { key, .. } => key.walk(f),
            Expr::MatElem { i, j, .. } => {
                i.walk(f);
                j.walk(f);
            }
            Expr::Num(..)
            | Expr::Missing(..)
            | Expr::Str(..)
            | Expr::Name(..)
            | Expr::Sys(..)
            | Expr::Term(..)
            | Expr::Hole { .. } => {}
        }
    }

    /// True when any node is a [`Expr::Hole`] — the speculative parser could not
    /// see through a macro. Completion and the "Used by" sidebar use it to
    /// downgrade their confidence instead of asserting something wrong.
    pub fn has_hole(&self) -> bool {
        let mut found = false;
        self.walk(&mut |e| found |= matches!(e, Expr::Hole { .. }));
        found
    }
}

/// The prefix operators.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum UnOp {
    /// Unary `-`.
    Neg,
    /// Unary `+`.
    Pos,
    /// `!` or `~`.
    Not,
}

/// The infix operators, in no particular order — [`BinOp::prec`] is the order.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum BinOp {
    /// `^`.
    Pow,
    /// `/`.
    Div,
    /// `*`.
    Mul,
    /// Binary `-`.
    Sub,
    /// Binary `+`.
    Add,
    /// `!=` or `~=`.
    Ne,
    /// `>`.
    Gt,
    /// `<`.
    Lt,
    /// `<=`.
    Le,
    /// `>=`.
    Ge,
    /// `==`.
    Eq,
    /// `&`.
    And,
    /// `|`.
    Or,
}

impl BinOp {
    /// Binding power. Design 02 §8.1, verified row by row against StataMP 18.5.
    ///
    /// Every level is distinct and every operator is left-associative, so the
    /// Pratt loop recurses at `prec() + 1` on the right with no associativity
    /// table at all. The gaps between levels exist so that a v1.5 operator can
    /// be slotted in without renumbering and invalidating the test corpus.
    pub const fn prec(self) -> u8 {
        match self {
            BinOp::Pow => 90,
            BinOp::Div => 70,
            BinOp::Mul => 65,
            BinOp::Sub => 60,
            BinOp::Add => 55,
            BinOp::Ne => 50,
            BinOp::Gt => 45,
            BinOp::Lt => 40,
            BinOp::Le => 35,
            BinOp::Ge => 30,
            BinOp::Eq => 25,
            BinOp::And => 20,
            BinOp::Or => 15,
        }
    }

    /// Canonical spelling, for diagnostics and for the round-trip test.
    pub const fn as_str(self) -> &'static str {
        match self {
            BinOp::Pow => "^",
            BinOp::Div => "/",
            BinOp::Mul => "*",
            BinOp::Sub => "-",
            BinOp::Add => "+",
            BinOp::Ne => "!=",
            BinOp::Gt => ">",
            BinOp::Lt => "<",
            BinOp::Le => "<=",
            BinOp::Ge => ">=",
            BinOp::Eq => "==",
            BinOp::And => "&",
            BinOp::Or => "|",
        }
    }

    /// Every infix operator, for the exhaustiveness test that keeps `prec` and
    /// this list from drifting apart.
    pub const ALL: [BinOp; 13] = [
        BinOp::Pow,
        BinOp::Div,
        BinOp::Mul,
        BinOp::Sub,
        BinOp::Add,
        BinOp::Ne,
        BinOp::Gt,
        BinOp::Lt,
        BinOp::Le,
        BinOp::Ge,
        BinOp::Eq,
        BinOp::And,
        BinOp::Or,
    ];
}

/// The binding power of the prefix operators, on the same scale as
/// [`BinOp::prec`].
///
/// `!` at 85 sits between `^` (90) and unary `-` (80). Both boundaries are
/// verified: `!2^0` is `0` so `^` is tighter [V], and `!0/2` is `.5` so `!` is
/// tighter than `/` [V].
pub const NOT_PREC: u8 = 85;

/// Binding power of unary `-` / `+`. `-2^2` is `-4` [V], so it is looser than
/// `^`; `-2^-2` is `-.25` [V], so it is tighter than everything below.
pub const SIGN_PREC: u8 = 80;

impl UnOp {
    /// Binding power on [`BinOp::prec`]'s scale.
    pub const fn prec(self) -> u8 {
        match self {
            UnOp::Not => NOT_PREC,
            UnOp::Neg | UnOp::Pos => SIGN_PREC,
        }
    }
}

/// The system variables ([U] 13.4).
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum SysVar {
    /// `_n` — current observation, group-relative under `by`.
    NLower,
    /// `_N` — observation count, group-relative under `by`.
    NUpper,
    /// `_pi`.
    Pi,
    /// `_rc`.
    Rc,
}

/// The stored-result classes.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum StoredClass {
    /// `r()`.
    R,
    /// `e()`.
    E,
    /// `c()`.
    C,
    /// `s()`.
    S,
}

/// The coefficient-reference families.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum CoefKind {
    /// `_b[]`.
    B,
    /// `_se[]`.
    Se,
    /// `_coef[]`.
    Coef,
}

/// A Stata numlist: `1/10`, `1 3 to 9`, `1(2)11`, `5`.
///
/// Kept as items rather than as an expanded `Vec<f64>` because `forvalues
/// i = 1/1000000` must not materialise a million doubles to run, and because
/// the source text has to be recoverable for a quick fix.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct NumList {
    /// The pieces, in the order typed.
    pub items: Vec<NumListItem>,
    /// Extent in the text this was parsed from.
    pub span: Span,
}

/// One piece of a [`NumList`].
#[derive(Copy, Clone, PartialEq, Debug)]
pub enum NumListItem {
    /// A bare value.
    Single(f64),
    /// `from(step)to`, `from/to`, `from to next to last`.
    Range {
        /// First value.
        from: f64,
        /// Increment. Always non-zero; a zero step is rejected at parse time.
        step: f64,
        /// Inclusive bound. The last emitted value is the last one that has not
        /// passed it, so `1(2)10` is `1 3 5 7 9`.
        to: f64,
    },
}

impl NumList {
    /// Number of values this list expands to, without expanding it.
    ///
    /// This is the counter the `forvalues` executor checks BEFORE materialising
    /// anything, and it is why a numlist is stored as ranges: a loop over
    /// `1/10000000` costs one multiply here, not eighty megabytes.
    pub fn count(&self) -> u64 {
        self.items.iter().map(NumListItem::count).sum()
    }

    /// Expand to values. Callers on an interaction path must check
    /// [`NumList::count`] first.
    pub fn expand(&self) -> Vec<f64> {
        let mut out = Vec::with_capacity(self.count().min(1 << 20) as usize);
        for it in &self.items {
            it.each(&mut |v| out.push(v));
        }
        out
    }
}

impl NumListItem {
    /// Values this item expands to.
    pub fn count(&self) -> u64 {
        match *self {
            NumListItem::Single(_) => 1,
            NumListItem::Range { from, step, to } => {
                if step == 0.0 || !(from.is_finite() && step.is_finite() && to.is_finite()) {
                    return 0;
                }
                let n = ((to - from) / step).floor();
                if n < 0.0 {
                    0
                } else {
                    // `floor` already rounded down; +1 counts `from` itself.
                    (n as u64).saturating_add(1)
                }
            }
        }
    }

    /// Call `f` once per value, in order, without allocating.
    pub fn each(&self, f: &mut impl FnMut(f64)) {
        match *self {
            NumListItem::Single(v) => f(v),
            NumListItem::Range { from, step, .. } => {
                // Accumulating `v += step` drifts: `forvalues x = 0(.1)1` must
                // produce the same doubles Stata does, and Stata recomputes from
                // the base. `from + i*step` is one rounding, not `i` of them.
                for i in 0..self.count() {
                    f(from + (i as f64) * step);
                }
            }
        }
    }
}

/// A Stata display format (`%9.2f`, `%td`, `%-10s`).
///
/// Held as text: `stratum_core::fmt::StataFormat::parse` is the one parser for
/// these, and duplicating it here would be a second definition of `%g`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Format {
    /// The format string including its leading `%`.
    pub text: String,
    /// Extent in the text this was parsed from.
    pub span: Span,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_binop_has_a_distinct_level() {
        // 02 §8.1's central claim: the levels are all distinct. A duplicate here
        // would silently make two operators associate with each other.
        let mut seen = Vec::new();
        for op in BinOp::ALL {
            assert!(
                !seen.contains(&op.prec()),
                "{} duplicates a level",
                op.as_str()
            );
            seen.push(op.prec());
        }
    }

    #[test]
    fn not_sits_between_pow_and_sign() {
        // `!2^0` is 0 [V] so `^` wins; `!0/2` is .5 [V] so `!` beats `/`.
        assert!(BinOp::Pow.prec() > NOT_PREC);
        const _: () = assert!(NOT_PREC > SIGN_PREC);
        assert!(SIGN_PREC > BinOp::Div.prec());
    }

    #[test]
    fn numlist_ranges_are_counted_not_materialised() {
        let big = NumListItem::Range {
            from: 1.0,
            step: 1.0,
            to: 10_000_000.0,
        };
        assert_eq!(big.count(), 10_000_000);
        assert_eq!(
            NumListItem::Range {
                from: 1.0,
                step: 2.0,
                to: 10.0
            }
            .count(),
            5
        );
        // A descending range with a positive step is empty, not infinite.
        assert_eq!(
            NumListItem::Range {
                from: 10.0,
                step: 1.0,
                to: 1.0
            }
            .count(),
            0
        );
    }
}
