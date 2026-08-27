//! The `r()` / `e()` model — `05` §14, and the insertion order of §§7.5, 8.7.
//!
//! **Order is output, not convenience.** `return list` and `ereturn list` print
//! in insertion order, and those two listings are part of the byte-exact
//! surface (`tests/golden/stata18/core_surface.log` prints `ereturn list`
//! verbatim). A `HashMap` here would have made the acceptance bullet
//! unprovable, so every container in this module is a `Vec` of pairs and
//! `push_*` overwrites **in place** rather than moving a key to the end.
//!
//! The runtime owns the *live* `r()`/`e()` singletons and the clear-on-next-
//! command rule (`05` §18). This module only produces values.

use serde::{Deserialize, Serialize};

/// Which stored-result class a command posts to.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultKind {
    /// `r()` — cleared by the next r-class command.
    RClass,
    /// `e()` — survives until the next e-class command.
    EClass,
    /// `s()`. No command in this crate posts one; present so the enum is the
    /// whole vocabulary rather than the part we happen to use.
    SClass,
}

/// A Stata matrix: values plus the row/column names `matrix list` prints.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct MatrixValue {
    /// Number of rows.
    pub rows: usize,
    /// Number of columns.
    pub cols: usize,
    /// Row-major, length `rows * cols`.
    pub data: Vec<f64>,
    /// Length `rows`.
    pub rownames: Vec<String>,
    /// Length `cols`.
    pub colnames: Vec<String>,
    /// Per-column operator stripe: `"o."` on a column omitted for
    /// collinearity, `""` otherwise. `matrix list e(b)` prints it above the
    /// column name, which is how a reader tells `_b[weight] == 0` apart from a
    /// coefficient that genuinely estimated to zero.
    pub colstripe: Vec<String>,
}

impl MatrixValue {
    /// A `1 x n` row vector.
    #[must_use]
    pub fn row_vector(name: &str, data: Vec<f64>, colnames: Vec<String>) -> Self {
        let cols = data.len();
        Self {
            rows: 1,
            cols,
            data,
            rownames: vec![name.to_owned()],
            colnames,
            colstripe: vec![String::new(); cols],
        }
    }

    /// `A[i][j]`.
    #[must_use]
    pub fn get(&self, i: usize, j: usize) -> f64 {
        self.data[i * self.cols + j]
    }
}

/// The estimation-sample bitmap behind `e(sample)`.
///
/// A newtype over words rather than `stratum_data::bitset::BitSet` for one
/// reason: this value is serialized into the sidecar and into the AI context,
/// and `BitSet` is not `Serialize`. The two convert in O(words).
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct RowSet {
    nbits: u64,
    words: Vec<u64>,
}

impl RowSet {
    /// An all-clear set over `nbits` observations.
    #[must_use]
    pub fn new(nbits: u64) -> Self {
        let n = usize::try_from(nbits.div_ceil(64)).unwrap_or(usize::MAX);
        Self {
            nbits,
            words: vec![0; n],
        }
    }

    /// Number of observations the set is defined over.
    #[must_use]
    pub fn len(&self) -> u64 {
        self.nbits
    }

    /// True when the set covers no observations at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nbits == 0
    }

    /// Mark observation `i`.
    pub fn set(&mut self, i: u64) {
        if i < self.nbits {
            self.words[(i / 64) as usize] |= 1u64 << (i % 64);
        }
    }

    /// Is observation `i` in the estimation sample?
    #[must_use]
    pub fn contains(&self, i: u64) -> bool {
        i < self.nbits && self.words[(i / 64) as usize] & (1u64 << (i % 64)) != 0
    }

    /// How many observations the estimation used.
    #[must_use]
    pub fn count(&self) -> u64 {
        self.words.iter().map(|w| u64::from(w.count_ones())).sum()
    }

    /// The comparability key of spec §19: blake3-128 of the bitmap, truncated.
    ///
    /// Two estimations with the same key were fitted on the same rows, which is
    /// the precondition "Compare models" checks before it puts two coefficient
    /// tables side by side.
    #[must_use]
    pub fn hash64(&self) -> u64 {
        let mut h = blake3::Hasher::new();
        h.update(&self.nbits.to_le_bytes());
        for w in &self.words {
            h.update(&w.to_le_bytes());
        }
        let d = h.finalize();
        u64::from_le_bytes(
            d.as_bytes()[..8]
                .try_into()
                .expect("blake3 digest is 32 bytes"),
        )
    }
}

/// An insertion-ordered `r()` or `e()`.
#[derive(Clone, Default, PartialEq, Debug, Serialize, Deserialize)]
pub struct ResultSet {
    scalars: Vec<(String, f64)>,
    macros: Vec<(String, String)>,
    matrices: Vec<(String, MatrixValue)>,
    functions: Vec<(String, RowSet)>,
}

impl ResultSet {
    /// An empty set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// `r(k)` / `e(k)` as a scalar.
    #[must_use]
    pub fn scalar(&self, k: &str) -> Option<f64> {
        self.scalars.iter().find(|(n, _)| n == k).map(|(_, v)| *v)
    }

    /// `r(k)` / `e(k)` as a macro.
    #[must_use]
    pub fn macro_(&self, k: &str) -> Option<&str> {
        self.macros
            .iter()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.as_str())
    }

    /// `r(k)` / `e(k)` as a matrix.
    #[must_use]
    pub fn matrix(&self, k: &str) -> Option<&MatrixValue> {
        self.matrices.iter().find(|(n, _)| n == k).map(|(_, v)| v)
    }

    /// `e(sample)`.
    #[must_use]
    pub fn function(&self, k: &str) -> Option<&RowSet> {
        self.functions.iter().find(|(n, _)| n == k).map(|(_, v)| v)
    }

    /// Insert or overwrite a scalar, keeping its original position.
    pub fn push_scalar(&mut self, k: &str, v: f64) {
        match self.scalars.iter_mut().find(|(n, _)| n == k) {
            Some(slot) => slot.1 = v,
            None => self.scalars.push((k.to_owned(), v)),
        }
    }

    /// Insert or overwrite a macro, keeping its original position.
    pub fn push_macro(&mut self, k: &str, v: impl Into<String>) {
        let v = v.into();
        match self.macros.iter_mut().find(|(n, _)| n == k) {
            Some(slot) => slot.1 = v,
            None => self.macros.push((k.to_owned(), v)),
        }
    }

    /// Insert or overwrite a matrix, keeping its original position.
    pub fn push_matrix(&mut self, k: &str, v: MatrixValue) {
        match self.matrices.iter_mut().find(|(n, _)| n == k) {
            Some(slot) => slot.1 = v,
            None => self.matrices.push((k.to_owned(), v)),
        }
    }

    /// Insert or overwrite a function, keeping its original position.
    pub fn push_function(&mut self, k: &str, v: RowSet) {
        match self.functions.iter_mut().find(|(n, _)| n == k) {
            Some(slot) => slot.1 = v,
            None => self.functions.push((k.to_owned(), v)),
        }
    }

    /// Scalar names, in insertion order. Backs `: e(scalars)`.
    #[must_use]
    pub fn scalar_names(&self) -> Vec<&str> {
        self.scalars.iter().map(|(n, _)| n.as_str()).collect()
    }

    /// Macro names, in insertion order.
    #[must_use]
    pub fn macro_names(&self) -> Vec<&str> {
        self.macros.iter().map(|(n, _)| n.as_str()).collect()
    }

    /// Matrix names, in insertion order.
    #[must_use]
    pub fn matrix_names(&self) -> Vec<&str> {
        self.matrices.iter().map(|(n, _)| n.as_str()).collect()
    }

    /// Function names, in insertion order.
    #[must_use]
    pub fn function_names(&self) -> Vec<&str> {
        self.functions.iter().map(|(n, _)| n.as_str()).collect()
    }

    /// Scalars in insertion order, for the card payload's `scalars` field.
    #[must_use]
    pub fn scalars(&self) -> &[(String, f64)] {
        &self.scalars
    }

    /// Macros in insertion order, for the card payload's `macros` field.
    #[must_use]
    pub fn macros(&self) -> &[(String, String)] {
        &self.macros
    }

    /// Every f64 this set carries, in insertion order: scalars first, then each
    /// matrix row-major. The determinism hash of `05` §17.5 is taken over
    /// exactly this sequence, so the order has to be a function of the data and
    /// nothing else.
    #[must_use]
    pub fn all_f64(&self) -> Vec<f64> {
        let mut out = Vec::with_capacity(self.scalars.len() + 16);
        for (_, v) in &self.scalars {
            out.push(*v);
        }
        for (_, m) in &self.matrices {
            out.extend_from_slice(&m.data);
        }
        out
    }
}
