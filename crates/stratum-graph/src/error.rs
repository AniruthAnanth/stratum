//! What a render can refuse to do, and why.
//!
//! There is no `GraphError::Internal`. Every variant here is a thing a *user*
//! did — named a scheme that does not exist, asked for a histogram of a constant,
//! plotted `y` against an `x` of a different length — and each one carries the
//! Stata return code the runtime will report, because "the graph did not draw"
//! with no `r(198);` is not an error a Stata user can act on.

use core::fmt;

/// A refusal to draw, with the return code the runtime reports for it.
///
/// `PartialEq` but not `Eq`: two variants carry the offending number so the
/// message can print it, and an `f64` has no total equality. Making the error
/// `Eq` would mean not carrying the value, and "variable is constant" without
/// saying what it is constant at is the kind of diagnostic that sends a user
/// back to `summarize`.
#[derive(Clone, PartialEq, Debug)]
pub enum GraphError {
    /// `scheme(nosuchscheme)`. Deliberately NOT a fallback to the default:
    /// `stratum_tokens::scheme` returns `Option` for exactly this reason —
    /// papering over the name draws the wrong colours and says nothing.
    UnknownScheme(String),
    /// Two variables in one layer with different observation counts. The runtime
    /// gathers both from the same `Sample`, so this is an internal contract
    /// break surfaced as `r(198)` rather than a panic in a render.
    RaggedLayer {
        /// The layer's first variable length.
        expected: usize,
        /// The length that disagreed.
        found: usize,
    },
    /// Nothing left to plot after the missing-value rule.
    NoObservations,
    /// `histogram` of a variable with no variation, and no `width()` given.
    /// Stata's `r(198)`; a zero-width bin is not a bin.
    ZeroRange {
        /// The variable's single distinct value.
        value: f64,
    },
    /// `bin(0)`, `width(0)`, a negative width, or a non-finite one.
    BadBinning,
    /// A figure smaller than its own margins.
    FigureTooSmall {
        /// Requested width in points.
        width_pt: f32,
        /// Requested height in points.
        height_pt: f32,
    },
}

impl GraphError {
    /// The Stata return code this refusal reports as.
    #[must_use]
    pub fn rc(&self) -> u16 {
        match self {
            // 198 — "invalid syntax" / invalid option argument, which is what
            // Stata returns for a bad option value and for a graph it cannot
            // construct from the varlist it was handed.
            GraphError::UnknownScheme(_)
            | GraphError::RaggedLayer { .. }
            | GraphError::ZeroRange { .. }
            | GraphError::BadBinning
            | GraphError::FigureTooSmall { .. } => 198,
            // 2000 — "no observations".
            GraphError::NoObservations => 2000,
        }
    }
}

impl fmt::Display for GraphError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GraphError::UnknownScheme(s) => write!(f, "scheme {s} not found"),
            GraphError::RaggedLayer { expected, found } => {
                write!(f, "plot variables differ in length ({expected} vs {found})")
            }
            GraphError::NoObservations => f.write_str("no observations"),
            GraphError::ZeroRange { value } => {
                // Not `{value}`: §8.7 bans `format!("{}", f64)` for a
                // user-visible number outside `stratum_core::fmt`, and a message
                // a user reads is exactly that.
                let shown = stratum_core::fmt::fmt_g(*value, 9);
                write!(f, "variable is constant at {}", shown.trim())
            }
            GraphError::BadBinning => f.write_str("bin width must be positive and finite"),
            GraphError::FigureTooSmall {
                width_pt,
                height_pt,
            } => {
                let w = stratum_core::fmt::fmt_g(f64::from(*width_pt), 9);
                let h = stratum_core::fmt::fmt_g(f64::from(*height_pt), 9);
                write!(
                    f,
                    "figure {}x{} pt is smaller than its margins",
                    w.trim(),
                    h.trim()
                )
            }
        }
    }
}

impl core::error::Error for GraphError {}
