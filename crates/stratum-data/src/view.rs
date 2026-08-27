//! The windowed view the Data Editor reads (`04` §13, CONTRACTS §8.1).
//!
//! **The core never serialises a whole frame to the UI.** A [`DataPage`] holds
//! exactly the cells one viewport asked for, in three column shapes, and
//! [`crate::page`] turns it into the `SDP1` bytes. This module is where the
//! *rendering* decisions live; that module is where the *bytes* live.
//!
//! # Cost is O(page), never O(rows)
//!
//! Every loop here runs over the requested rows, so a 40 × 30 page costs 1 200
//! cell reads whether the frame has 74 observations or ten million.
//! [`crate::perf::Counters::rows_touched`] is bumped by exactly `nrows` per
//! column, which is the assertion `tests/page.rs` makes in place of the plan's
//! wall-clock budget (ADR-017).
//!
//! # What each render mode means, decided by the fixture
//!
//! `tests/fixtures/sdp1/README.md` §2.4 settled the two questions
//! `CONTRACTS.md` §8.1 leaves open, and this is the implementation of its
//! ruling:
//!
//! * [`RenderMode::Display`] cells carry **no format padding**. A cell holds
//!   what Stata's own `string(x, "%fmt")` returns — already trimmed — or the
//!   value label when the variable has one with an entry for that value, or, for
//!   a string variable, the stored bytes verbatim. Column alignment is
//!   `design/tokens.json`'s job (`text-align: right` plus a per-column `ch`
//!   padding computed from the display format); padding inside the cell would
//!   fight it.
//! * [`RenderMode::Edit`] gives raw values: `f64` with Stata's sentinel bit
//!   pattern intact plus a per-cell tag, and strings as their stored bytes with
//!   no format applied and no label substituted.
//!
//! A `str#` column therefore renders **identically** in both modes, which is why
//! `auto_40x12.bin` and `auto_40x12_edit.bin` share their `make` column
//! byte-for-byte.
//!
//! # `strL` is `blob` in both modes
//!
//! A `strL` may hold arbitrary bytes (GSO type 129). Rendering it as `text` in
//! `Display` would hand the webview a `TextDecoder` job that can fail on data
//! the user legitimately stored, so a `strL` is always a [`ColumnBlock::Blob`]
//! and the accompanying bitmap says which cells are binary.
//!
//! # Two entry points, and why
//!
//! [`DataPage::build`] is the production path: a [`FrameSnapshot`] plus the
//! [`PageRequest`] the asset handler parsed. [`PageView`] is the same work with
//! the inputs named individually — the metadata that decides rendering (a
//! variable's [`StataFormat`](stratum_core::fmt::StataFormat) and its value
//! label) travels beside the storage rather than inside it. That is what lets a
//! caller render a page for a frame it assembled column by column, and it is how
//! `tests/page.rs` reproduces bytes captured at a `DatasetStateId` no synthetic
//! frame can be driven to.

use stratum_proto::{DatasetStateId, PageRequest, RenderMode, VarIdx};

use crate::chunk::{chunk_of, offset_in_chunk};
use crate::column::Column;
use crate::frame::FrameSnapshot;
use crate::labels::ValueLabelSet;
use crate::order::{OrderError, OrderRegistry, ViewOrder};
use crate::perf::{bump, counters};
use crate::variable::Variable;

/// The largest page this engine will build.
///
/// A viewport is tens of rows and a prefetch is a few hundred; a request for
/// more than a million is a bug or a hostile caller, and answering it would mean
/// allocating on its behalf. `04` §13's "the UI requests `rows` ±1 viewport" is
/// the shape this bound protects.
pub const MAX_PAGE_ROWS: u32 = 1 << 20;

/// The `num` tag for "not missing" (CONTRACTS §8.1).
pub const NOT_MISSING: u8 = 255;

/// One column's cells, in the shape `SDP1` will carry them.
#[derive(Clone, Debug, PartialEq)]
pub enum ColumnBlock {
    /// `kind: "text"` — a UTF-8 arena plus `nrows + 1` ascending offsets.
    Text {
        /// Which variable.
        idx: VarIdx,
        /// `len == nrows + 1`; cell `i` is `bytes[offsets[i]..offsets[i + 1]]`.
        offsets: Vec<u32>,
        /// The arena. No terminators, no padding.
        bytes: Vec<u8>,
    },
    /// `kind: "num"` — raw `f64` plus a per-cell missing tag.
    Num {
        /// Which variable.
        idx: VarIdx,
        /// Stata's sentinel bit patterns preserved exactly.
        values: Vec<f64>,
        /// `255` not missing, `0` for `.`, `1..=26` for `.a`..`.z`.
        tags: Vec<u8>,
    },
    /// `kind: "blob"` — a byte arena, offsets, and the binary bitmap.
    Blob {
        /// Which variable.
        idx: VarIdx,
        /// `len == nrows + 1`.
        offsets: Vec<u32>,
        /// Arbitrary bytes.
        bytes: Vec<u8>,
        /// `ceil(nrows / 8)` bytes, LSB first: a set bit means GSO type 129.
        binary: Vec<u8>,
    },
}

impl ColumnBlock {
    /// Which variable this block carries.
    #[must_use]
    pub fn idx(&self) -> VarIdx {
        match self {
            ColumnBlock::Text { idx, .. }
            | ColumnBlock::Num { idx, .. }
            | ColumnBlock::Blob { idx, .. } => *idx,
        }
    }

    /// The `SDP1` `kind` string.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            ColumnBlock::Text { .. } => "text",
            ColumnBlock::Num { .. } => "num",
            ColumnBlock::Blob { .. } => "blob",
        }
    }

    /// How many cells.
    #[must_use]
    pub fn nrows(&self) -> u32 {
        match self {
            ColumnBlock::Text { offsets, .. } | ColumnBlock::Blob { offsets, .. } => {
                offsets.len() as u32 - 1
            }
            ColumnBlock::Num { values, .. } => values.len() as u32,
        }
    }
}

/// One column's storage together with the metadata that decides how it renders.
#[derive(Clone, Copy, Debug)]
pub struct ColumnSpec<'a> {
    /// Position in storage order, echoed into the `SDP1` header.
    pub idx: VarIdx,
    /// Supplies the display format and the value-label name.
    pub var: &'a Variable,
    /// The cells.
    pub col: &'a Column,
}

/// Everything one page needs, with nothing inferred.
#[derive(Clone, Debug)]
pub struct PageView<'a> {
    /// The state the response will report.
    pub state: DatasetStateId,
    /// First **view** row.
    pub row0: u64,
    /// How many rows to try for; clipped at the end of the view by
    /// [`render`](Self::render).
    pub nrows: u32,
    /// Echoed so a stale response can be dropped.
    pub seq: u32,
    /// Which rendering.
    pub render: RenderMode,
    /// The frame's value-label tables.
    pub labels: &'a ValueLabelSet,
    /// `None` ⇒ dataset order.
    pub order: Option<&'a ViewOrder>,
    /// The columns, in the order the page will carry them.
    pub cols: Vec<ColumnSpec<'a>>,
}

/// Why a page could not be built.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ViewError {
    /// The request names a variable the frame does not have.
    #[error("variable {0} not found")]
    NoSuchVar(VarIdx),
    /// More rows than [`MAX_PAGE_ROWS`].
    #[error("page of {got} rows exceeds the {max}-row limit")]
    TooManyRows {
        /// What was asked for.
        got: u32,
        /// [`MAX_PAGE_ROWS`].
        max: u32,
    },
    /// One column's arena would exceed the `u32` offsets `SDP1` declares.
    #[error("column {0} needs an arena larger than 4 GiB")]
    ArenaTooLarge(VarIdx),
    /// The view order is unusable.
    #[error(transparent)]
    Order(#[from] OrderError),
}

impl ViewError {
    /// Stata's return code, for the cases that surface as an error rather than
    /// as a UI invalidation.
    #[must_use]
    pub fn rc(&self) -> u16 {
        match self {
            ViewError::NoSuchVar(_) => 111,
            ViewError::Order(e) => e.rc(),
            _ => 198,
        }
    }
}

/// One viewport's worth of a frame.
///
/// `state` is a public field because the encoder is a pure function of this
/// value: [`DataPage::build`] fills it from the snapshot, and a caller
/// reproducing bytes captured at some other state sets it directly.
#[derive(Clone, Debug, PartialEq)]
pub struct DataPage {
    /// The snapshot these cells came from. The UI invalidates when it differs
    /// from what its request carried.
    pub state: DatasetStateId,
    /// First **view** row, echoed from the request.
    pub row0: u64,
    /// How many rows this page actually holds — clipped at the end of the view.
    pub nrows: u32,
    /// Echoed from the request so a stale response can be dropped.
    pub seq: u32,
    /// One block per requested column, in request order.
    pub cols: Vec<ColumnBlock>,
}

impl DataPage {
    /// Build the page a [`PageRequest`] names.
    ///
    /// `orders` resolves [`PageRequest::order`]; pass an empty registry when the
    /// caller never issues `data_order_set`. The response's `state` is the
    /// snapshot's own version, **not** the request's — that difference is how
    /// the UI learns its page is stale.
    ///
    /// # Errors
    ///
    /// [`ViewError`].
    pub fn build(
        snap: &FrameSnapshot,
        req: &PageRequest,
        orders: &OrderRegistry,
    ) -> Result<DataPage, ViewError> {
        let state = DatasetStateId::from(snap.version());
        let order = match req.order {
            None => None,
            Some(id) => Some(orders.get_for_state(id, state)?),
        };
        let mut cols = Vec::with_capacity(req.cols.len());
        for &idx in &req.cols {
            let var = snap.var(idx).ok_or(ViewError::NoSuchVar(idx))?;
            let col = snap.col(idx).ok_or(ViewError::NoSuchVar(idx))?;
            cols.push(ColumnSpec { idx, var, col });
        }
        PageView {
            state,
            row0: req.row0,
            nrows: req.nrows,
            seq: req.seq,
            render: req.render,
            labels: snap.labels(),
            order: order.as_deref(),
            cols,
        }
        .render()
    }

    /// Total cells, the number the paging counter is asserted against.
    #[must_use]
    pub fn cells(&self) -> u64 {
        u64::from(self.nrows) * self.cols.len() as u64
    }
}

impl PageView<'_> {
    /// How many view rows exist: the order's length, or the shortest column.
    fn view_len(&self) -> u64 {
        match self.order {
            Some(o) => o.len(),
            None => self.cols.iter().map(|c| c.col.len()).min().unwrap_or(0),
        }
    }

    /// Materialise the page.
    ///
    /// `nrows` is clipped at the end of the view rather than refused: the UI
    /// scrolls optimistically and `04` §13's prefetch deliberately overshoots.
    ///
    /// # Errors
    ///
    /// [`ViewError`].
    pub fn render(&self) -> Result<DataPage, ViewError> {
        if self.nrows > MAX_PAGE_ROWS {
            return Err(ViewError::TooManyRows {
                got: self.nrows,
                max: MAX_PAGE_ROWS,
            });
        }
        let avail = self.view_len().saturating_sub(self.row0);
        let nrows = u32::try_from(avail.min(u64::from(self.nrows))).unwrap_or(self.nrows);

        let mut cols = Vec::with_capacity(self.cols.len());
        for spec in &self.cols {
            cols.push(self.block(*spec, nrows)?);
            // Once per column, so a 40 × 30 page reports 1 200 cells whatever
            // `_N` is. This is ADR-017's counter for the 12 ms budget.
            bump(&counters().rows_touched, u64::from(nrows));
        }
        Ok(DataPage {
            state: self.state,
            row0: self.row0,
            nrows,
            seq: self.seq,
            cols,
        })
    }

    /// The dataset observation shown at view row `row0 + i`.
    #[inline]
    fn dataset_row(&self, i: u32) -> u64 {
        let v = self.row0 + u64::from(i);
        match self.order {
            None => v,
            // `nrows` was clipped to the view length, so this is always in
            // range; falling back keeps the function total rather than
            // panicking on a state that cannot occur against a snapshot.
            Some(o) => o.row(v).unwrap_or(v),
        }
    }

    fn block(&self, spec: ColumnSpec<'_>, nrows: u32) -> Result<ColumnBlock, ViewError> {
        let ColumnSpec { idx, var, col } = spec;
        match col {
            Column::StrL(s) => {
                let mut offsets = Vec::with_capacity(nrows as usize + 1);
                let mut bytes = Vec::new();
                let mut binary = vec![0u8; (nrows as usize).div_ceil(8)];
                offsets.push(0u32);
                for i in 0..nrows {
                    let row = self.dataset_row(i);
                    bytes.extend_from_slice(s.get(row));
                    offsets.push(
                        u32::try_from(bytes.len()).map_err(|_| ViewError::ArenaTooLarge(idx))?,
                    );
                    if s.chunk(chunk_of(row)).is_binary(offset_in_chunk(row)) {
                        // LSB first, row 0 is bit 0 of byte 0 (README §2.3).
                        binary[i as usize >> 3] |= 1 << (i & 7);
                    }
                }
                Ok(ColumnBlock::Blob {
                    idx,
                    offsets,
                    bytes,
                    binary,
                })
            }
            // A `str#` renders the same in both modes: `Display` is "the stored
            // value verbatim" and `Edit` is "strings as bytes".
            Column::Str(_) => {
                let mut offsets = Vec::with_capacity(nrows as usize + 1);
                let mut bytes = Vec::new();
                offsets.push(0u32);
                for i in 0..nrows {
                    bytes.extend_from_slice(col.get_bytes(self.dataset_row(i)).unwrap_or_default());
                    offsets.push(
                        u32::try_from(bytes.len()).map_err(|_| ViewError::ArenaTooLarge(idx))?,
                    );
                }
                Ok(ColumnBlock::Text {
                    idx,
                    offsets,
                    bytes,
                })
            }
            _ => match self.render {
                RenderMode::Edit => {
                    let mut values = Vec::with_capacity(nrows as usize);
                    let mut tags = Vec::with_capacity(nrows as usize);
                    for i in 0..nrows {
                        let v = col
                            .get_f64(self.dataset_row(i))
                            .unwrap_or(stratum_core::missing::SYSMISS);
                        values.push(v);
                        tags.push(stratum_core::tag_of(v).unwrap_or(NOT_MISSING));
                    }
                    Ok(ColumnBlock::Num { idx, values, tags })
                }
                RenderMode::Display => {
                    let table = var
                        .value_label
                        .as_deref()
                        .and_then(|name| self.labels.get(name));
                    let mut offsets = Vec::with_capacity(nrows as usize + 1);
                    let mut bytes = Vec::new();
                    offsets.push(0u32);
                    for i in 0..nrows {
                        let v = col
                            .get_f64(self.dataset_row(i))
                            .unwrap_or(stratum_core::missing::SYSMISS);
                        match table.and_then(|t| t.get(v)) {
                            // A labelled value with a matching entry is the
                            // label text; one without falls back to the number.
                            Some(text) => bytes.extend_from_slice(text.as_bytes()),
                            None => bytes.extend_from_slice(display_number(var, v).as_bytes()),
                        }
                        offsets.push(
                            u32::try_from(bytes.len())
                                .map_err(|_| ViewError::ArenaTooLarge(idx))?,
                        );
                    }
                    Ok(ColumnBlock::Text {
                        idx,
                        offsets,
                        bytes,
                    })
                }
            },
        }
    }
}

/// `string(x, "%fmt")`: the format applied, then trimmed.
///
/// [`StataFormat::format_f64`](stratum_core::fmt::StataFormat::format_f64)
/// justifies into the format's field, because `list` and the classic log need
/// the field. A Data-Editor cell does not: fixture README §2.4 rules that
/// `price` observation 1 is `4,099` and not `   4,099`. Trimming here rather
/// than teaching `stratum_core` a second mode keeps one formatter for both
/// callers.
fn display_number(var: &Variable, v: f64) -> String {
    let s = var.format.format_f64(v);
    let t = s.trim();
    if t.len() == s.len() {
        s
    } else {
        t.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::Frame;
    use crate::labels::ValueLabel;
    use stratum_core::fmt::StataFormat;
    use stratum_core::missing::{missing_f64, SYSMISS};
    use stratum_proto::StorageType;

    fn request(cols: &[u32], render: RenderMode, nrows: u32) -> PageRequest {
        PageRequest {
            frame: "default".into(),
            state: DatasetStateId(0),
            row0: 0,
            nrows,
            cols: cols.iter().map(|&i| VarIdx(i)).collect(),
            order: None,
            render,
            seq: 1,
        }
    }

    fn one_double(vals: &[f64]) -> Frame {
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

    #[test]
    fn edit_mode_preserves_the_sentinel_and_its_tag() {
        let f = one_double(&[1.5, SYSMISS, missing_f64(26)]);
        let p = DataPage::build(
            &f.snapshot(),
            &request(&[0], RenderMode::Edit, 3),
            &OrderRegistry::new(),
        )
        .expect("valid");
        let ColumnBlock::Num { values, tags, .. } = &p.cols[0] else {
            panic!("a double in Edit mode is `num`");
        };
        assert_eq!(tags, &[NOT_MISSING, 0, 26]);
        assert_eq!(values[1].to_bits(), SYSMISS.to_bits());
        assert_eq!(values[2].to_bits(), missing_f64(26).to_bits());
    }

    #[test]
    fn display_mode_trims_the_format_field() {
        let f = one_double(&[4099.0, SYSMISS]);
        let mut var = f.vars()[0].clone();
        var.format = StataFormat::parse("%8.0gc").expect("valid format");
        let labels = ValueLabelSet::new();
        let p = PageView {
            state: DatasetStateId(1),
            row0: 0,
            nrows: 2,
            seq: 1,
            render: RenderMode::Display,
            labels: &labels,
            order: None,
            cols: vec![ColumnSpec {
                idx: VarIdx(0),
                var: &var,
                col: f.col(VarIdx(0)).expect("exists"),
            }],
        }
        .render()
        .expect("valid");
        let ColumnBlock::Text { offsets, bytes, .. } = &p.cols[0] else {
            panic!("Display renders a numeric as `text`");
        };
        assert_eq!(&bytes[offsets[0] as usize..offsets[1] as usize], b"4,099");
        assert_eq!(&bytes[offsets[1] as usize..offsets[2] as usize], b".");
    }

    #[test]
    fn a_value_label_wins_and_an_unlabelled_value_falls_back() {
        let f = one_double(&[0.0, 1.0]);
        let mut var = f.vars()[0].clone();
        var.value_label = Some(std::sync::Arc::from("origin"));
        let mut t = ValueLabel::new();
        t.insert(0, "Domestic".to_owned());
        let mut labels = ValueLabelSet::new();
        labels.insert("origin", t);
        let p = PageView {
            state: DatasetStateId(1),
            row0: 0,
            nrows: 2,
            seq: 1,
            render: RenderMode::Display,
            labels: &labels,
            order: None,
            cols: vec![ColumnSpec {
                idx: VarIdx(0),
                var: &var,
                col: f.col(VarIdx(0)).expect("exists"),
            }],
        }
        .render()
        .expect("valid");
        let ColumnBlock::Text { offsets, bytes, .. } = &p.cols[0] else {
            panic!("Display renders a numeric as `text`");
        };
        assert_eq!(
            &bytes[offsets[0] as usize..offsets[1] as usize],
            b"Domestic"
        );
        assert_eq!(&bytes[offsets[1] as usize..offsets[2] as usize], b"1");
    }

    #[test]
    fn a_page_past_the_end_is_clipped_rather_than_refused() {
        let f = one_double(&[1.0, 2.0, 3.0]);
        let mut req = request(&[0], RenderMode::Edit, 40);
        req.row0 = 2;
        let p = DataPage::build(&f.snapshot(), &req, &OrderRegistry::new()).expect("valid");
        assert_eq!(p.nrows, 1);
        assert_eq!(p.cells(), 1);
    }

    #[test]
    fn an_absurd_page_is_refused_before_it_allocates() {
        let f = one_double(&[1.0]);
        let e = DataPage::build(
            &f.snapshot(),
            &request(&[0], RenderMode::Edit, u32::MAX),
            &OrderRegistry::new(),
        )
        .expect_err("a page is a viewport, not a dataset");
        assert!(matches!(e, ViewError::TooManyRows { .. }));
    }

    #[test]
    fn a_missing_variable_is_named_rather_than_silently_dropped() {
        let f = one_double(&[1.0]);
        let e = DataPage::build(
            &f.snapshot(),
            &request(&[7], RenderMode::Edit, 1),
            &OrderRegistry::new(),
        )
        .expect_err("no such column");
        assert_eq!(e, ViewError::NoSuchVar(VarIdx(7)));
        assert_eq!(e.rc(), 111);
    }
}
