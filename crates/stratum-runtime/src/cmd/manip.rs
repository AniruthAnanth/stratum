//! Creating and changing variables: `generate`, `replace`, `drop`, `keep`,
//! `rename`.
//!
//! # One `gen` bump per command, and how that is achieved
//!
//! `Frame::col_mut` advances the frame's [`DataVersion`](stratum_data::DataVersion)
//! on **every call**, so "a `replace x = x+1` over 10 M rows performs exactly
//! one bump" (the plan's write-barrier acceptance bullet) forces every command
//! here to acquire `col_mut` exactly **once** per column it writes.
//!
//! That is why the values are evaluated into a buffer first and written second,
//! rather than evaluated and written chunk by chunk. The alternative —
//! `col_mut` per chunk — would allocate no buffer and bump 153 times on 10 M
//! rows, which is precisely the "bumps per element" failure the invariant
//! names. The buffer costs 8 bytes per **selected** row (`replace x = 1 in 1`
//! buffers one value, not `_N`), and spec §0a's ruling is explicit that a
//! larger, faster shape beats a smaller, slower one.
//!
//! The write itself still goes chunk-wise: a `double` target takes
//! [`ColMut::with_double_chunk`](stratum_data::ColMut::with_double_chunk), so
//! the inner loop is a contiguous `&mut [f64]` and the journal retains one
//! 512 KiB chunk at a time (A18).

use stratum_core::missing::{
    canon, is_missing, narrow_byte, narrow_float, narrow_int, narrow_long, Narrowed,
};
use stratum_data::{column::NumCol, Column, Sample, StorageType};
use stratum_parse::ast::command::Command;
use stratum_parse::ast::varlist::{VarItemKind, VarList, VarPattern};
use stratum_parse::ast::CommandAst;
use stratum_parse::{StataError, VarlistMode};
use stratum_proto::{DataChangeSummary, ResultPayload, VarIdx};

use super::{
    build_sample, err, resolve_varlist, slots, CmdHost, CmdOutcome, CmdResult, EvalType, Out,
};

/// How many rows are evaluated per call into the host's evaluator.
///
/// The storage granule, the reduction granule and the undo-journal granule are
/// all [`stratum_data::CHUNK_ROWS`] (C35); making this the evaluation granule
/// too means a fold boundary, a memory boundary and a retention boundary are
/// the same boundary.
const EVAL_CHUNK: usize = stratum_data::CHUNK_ROWS;

// ---------------------------------------------------------------------------
// generate
// ---------------------------------------------------------------------------

/// `generate [type] newvar = exp [if] [in]`.
pub fn generate(host: &mut dyn CmdHost, ast: &CommandAst) -> CmdResult {
    let s = slots(ast).ok_or_else(|| err::invalid("generate"))?;
    let expr = s
        .assign
        .as_ref()
        .ok_or_else(|| StataError::new(198, "invalid syntax").at(ast.span))?;
    let (ty, name) = new_var_spec(s.varlist.as_ref(), ast)?;
    if !stratum_data::variable::is_valid_name(&name) {
        return Err(err::invalid_name(&name).at(ast.span));
    }
    if stratum_parse::varlist::is_reserved(&name) {
        return Err(err::invalid_name(&name).at(ast.span));
    }
    if host.frames().current().index_of(&name).is_some() {
        return Err(err::already_defined(&name).at(ast.span));
    }
    let sample = build_sample(host, s.if_.as_ref(), s.in_.as_ref())?;
    let nobs = host.frames().current().n_obs();
    if nobs == 0 {
        return Err(err::no_observations().at(ast.span));
    }

    let before = summary_before(host);
    match host.expr_type(expr)? {
        EvalType::Numeric => {
            let values = eval_over(host, expr, &sample, nobs)?;
            let missing = values.iter().copied().filter(|v| is_missing(*v)).count() as u64;
            let ty = ty.unwrap_or(host.settings().gen_type);
            let col = build_column(ty, &values);
            let frame = host.frames_mut().current_mut();
            frame.begin_command();
            match frame.add_column(&name, col) {
                Ok(_) => frame.commit(),
                Err(e) => {
                    frame.rollback();
                    return Err(frame_error(e, &name));
                }
            }
            note_missing(host, missing);
            Ok(changed(host, before, DataChange::Created(name)))
        }
        EvalType::Str => {
            let values = eval_str_over(host, expr, &sample, nobs)?;
            let width = values
                .iter()
                .map(|s| s.len())
                .max()
                .unwrap_or(1)
                .clamp(1, 2045) as u16;
            let ty = ty.unwrap_or(StorageType::Str { width });
            let frame = host.frames_mut().current_mut();
            frame.begin_command();
            let idx = match frame.add_column(&name, Column::new_missing(ty, nobs)) {
                Ok(i) => i,
                Err(e) => {
                    frame.rollback();
                    return Err(frame_error(e, &name));
                }
            };
            // Two version bumps for a string `generate` — `add_column` and one
            // `col_mut` — because `FixedStrCol` has no public constructor from
            // a value list the way `NumCol::from_slice` does. Noted for W02;
            // the numeric path, which is the one the 10 M-row acceptance
            // measures, is a single bump.
            let mut cm = frame.col_mut(idx).map_err(|e| frame_error(e, &name))?;
            for (row, v) in values.iter().enumerate() {
                if let Err(e) = cm.set_bytes(row as u64, v.as_bytes()) {
                    frame.rollback();
                    return Err(write_error(e, &name));
                }
            }
            frame.commit();
            Ok(changed(host, before, DataChange::Created(name)))
        }
    }
}

// ---------------------------------------------------------------------------
// replace
// ---------------------------------------------------------------------------

/// `replace var = exp [if] [in]`.
pub fn replace(host: &mut dyn CmdHost, ast: &CommandAst) -> CmdResult {
    let s = slots(ast).ok_or_else(|| err::invalid("replace"))?;
    let expr = s
        .assign
        .as_ref()
        .ok_or_else(|| StataError::new(198, "invalid syntax").at(ast.span))?;
    let idxs = resolve_varlist(host, s.varlist.as_ref(), VarlistMode::Existing, false)?;
    let [idx] = idxs[..] else {
        return Err(if idxs.is_empty() {
            err::too_few_vars().at(ast.span)
        } else {
            err::too_many_vars().at(ast.span)
        });
    };
    let idx = VarIdx(idx);
    let sample = build_sample(host, s.if_.as_ref(), s.in_.as_ref())?;
    let ty = host.frames().current().var(idx).expect("resolved index").ty;
    let name = host
        .frames()
        .current()
        .var(idx)
        .expect("resolved index")
        .name
        .to_string();

    let before = summary_before(host);
    if matches!(ty, StorageType::Str { .. } | StorageType::StrL) {
        return replace_str(host, ast, idx, &name, expr, &sample, before);
    }
    if host.expr_type(expr)? == EvalType::Str {
        return Err(err::type_mismatch().at(ast.span));
    }

    // Phase 1 — evaluate, touching no column.
    let rows: Vec<u64> = sample
        .runs()
        .flat_map(|r| r.start..r.start + r.len)
        .collect();
    let mut values = Vec::with_capacity(rows.len());
    for run in sample.runs() {
        let mut row = run.start;
        let end = run.start + run.len;
        while row < end {
            let len = usize::try_from((end - row).min(EVAL_CHUNK as u64)).expect("bounded");
            host.eval_num_rows(expr, row, len, &mut values)?;
            row += len as u64;
        }
    }
    debug_assert_eq!(values.len(), rows.len());

    // Phase 2 — promote once if any value needs it, then write once.
    let mut target = ty;
    for v in &values {
        if let Narrowed::NeedsPromotion(to) = narrow_to(target, *v) {
            target = to;
        }
    }
    let frame = host.frames_mut().current_mut();
    frame.begin_command();
    if target != ty {
        if let Err(e) = frame.recast_var(idx, target) {
            frame.rollback();
            return Err(frame_error(e, &name));
        }
    }
    let old: Vec<f64> = {
        let col = frame.col(idx).expect("resolved index");
        rows.iter()
            .map(|r| col.get_f64(*r).unwrap_or(stratum_core::SYSMISS))
            .collect()
    };
    let mut cm = match frame.col_mut(idx) {
        Ok(c) => c,
        Err(e) => {
            frame.rollback();
            return Err(frame_error(e, &name));
        }
    };
    for (row, v) in rows.iter().zip(values.iter()) {
        if let Err(e) = cm.set_f64(*row, *v) {
            frame.rollback();
            return Err(write_error(e, &name));
        }
    }
    // Count against what actually landed, not against what was asked for: a
    // `float` target rounds, and Stata's counter reports stored changes.
    let mut real = 0u64;
    let mut to_missing = 0u64;
    {
        let col = frame.col(idx).expect("resolved index");
        for (i, row) in rows.iter().enumerate() {
            let now = col.get_f64(*row).unwrap_or(stratum_core::SYSMISS);
            if canon(now).to_bits() != canon(old[i]).to_bits() {
                real += 1;
                if is_missing(now) {
                    to_missing += 1;
                }
            }
        }
    }
    frame.commit();
    note_changes(host, real, to_missing);
    Ok(changed(host, before, DataChange::Modified(name)))
}

fn replace_str(
    host: &mut dyn CmdHost,
    ast: &CommandAst,
    idx: VarIdx,
    name: &str,
    expr: &stratum_parse::ast::expr::Expr,
    sample: &Sample,
    before: (u64, u32),
) -> CmdResult {
    if host.expr_type(expr)? != EvalType::Str {
        return Err(err::type_mismatch().at(ast.span));
    }
    let rows: Vec<u64> = sample
        .runs()
        .flat_map(|r| r.start..r.start + r.len)
        .collect();
    let mut values: Vec<String> = Vec::with_capacity(rows.len());
    for run in sample.runs() {
        let mut row = run.start;
        let end = run.start + run.len;
        while row < end {
            let len = usize::try_from((end - row).min(EVAL_CHUNK as u64)).expect("bounded");
            host.eval_str_rows(expr, row, len, &mut values)?;
            row += len as u64;
        }
    }
    let frame = host.frames_mut().current_mut();
    frame.begin_command();
    let old: Vec<Vec<u8>> = {
        let col = frame.col(idx).expect("resolved index");
        rows.iter()
            .map(|r| col.get_bytes(*r).unwrap_or_default().to_vec())
            .collect()
    };
    let mut cm = match frame.col_mut(idx) {
        Ok(c) => c,
        Err(e) => {
            frame.rollback();
            return Err(frame_error(e, name));
        }
    };
    for (row, v) in rows.iter().zip(values.iter()) {
        if let Err(e) = cm.set_bytes(*row, v.as_bytes()) {
            frame.rollback();
            return Err(write_error(e, name));
        }
    }
    let mut real = 0u64;
    let mut to_missing = 0u64;
    for (i, v) in values.iter().enumerate() {
        if v.as_bytes() != trim_nul(&old[i]) {
            real += 1;
            if v.is_empty() {
                to_missing += 1;
            }
        }
    }
    frame.commit();
    note_changes(host, real, to_missing);
    Ok(changed(host, before, DataChange::Modified(name.to_owned())))
}

fn trim_nul(b: &[u8]) -> &[u8] {
    let end = b.iter().position(|c| *c == 0).unwrap_or(b.len());
    &b[..end]
}

// ---------------------------------------------------------------------------
// drop / keep
// ---------------------------------------------------------------------------

/// `drop varlist` or `drop if/in`.
pub fn drop(host: &mut dyn CmdHost, ast: &CommandAst) -> CmdResult {
    let s = slots(ast).ok_or_else(|| err::invalid("drop"))?;
    if s.if_.is_some() || s.in_.is_some() {
        if s.varlist.is_some() {
            // `drop x if y` is Stata's r(198): a varlist and a qualifier
            // select different things and cannot be combined.
            return Err(StataError::new(198, "invalid syntax").at(ast.span));
        }
        let sample = build_sample(host, s.if_.as_ref(), s.in_.as_ref())?;
        return delete_obs(host, &sample, true);
    }
    let idxs = resolve_varlist(host, s.varlist.as_ref(), VarlistMode::Existing, false)?;
    if idxs.is_empty() {
        return Err(err::too_few_vars().at(ast.span));
    }
    drop_vars(host, idxs)
}

/// `keep varlist` or `keep if/in`.
pub fn keep(host: &mut dyn CmdHost, ast: &CommandAst) -> CmdResult {
    let s = slots(ast).ok_or_else(|| err::invalid("keep"))?;
    if s.if_.is_some() || s.in_.is_some() {
        let sample = build_sample(host, s.if_.as_ref(), s.in_.as_ref())?;
        return delete_obs(host, &sample, false);
    }
    let keep_idxs = resolve_varlist(host, s.varlist.as_ref(), VarlistMode::Existing, false)?;
    if keep_idxs.is_empty() {
        return Err(err::too_few_vars().at(ast.span));
    }
    let all = host.frames().current().n_vars();
    let doomed: Vec<u32> = (0..all).filter(|i| !keep_idxs.contains(i)).collect();
    drop_vars(host, doomed)
}

fn drop_vars(host: &mut dyn CmdHost, mut idxs: Vec<u32>) -> CmdResult {
    let before = summary_before(host);
    // Descending, so each removal leaves the not-yet-removed positions valid.
    idxs.sort_unstable();
    idxs.dedup();
    let names: Vec<String> = idxs
        .iter()
        .filter_map(|i| {
            host.frames()
                .current()
                .var(VarIdx(*i))
                .map(|v| v.name.to_string())
        })
        .collect();
    let frame = host.frames_mut().current_mut();
    frame.begin_command();
    for i in idxs.iter().rev() {
        if let Err(e) = frame.drop_var(VarIdx(*i)) {
            frame.rollback();
            return Err(frame_error(e, ""));
        }
    }
    frame.commit();
    Ok(changed(host, before, DataChange::Dropped(names)))
}

/// Delete the observations `sample` selects (`keep_selected == false` keeps
/// them instead).
///
/// The kept rows are shifted down in place through the write barrier and the
/// frame is then truncated. `stratum_data` has no row-deletion primitive — a
/// native gather would be one pass per column instead of one write per kept
/// cell — so this is the correct-but-slower shape. Flagged for W02 in W06c's
/// return.
fn delete_obs(host: &mut dyn CmdHost, sample: &Sample, delete_selected: bool) -> CmdResult {
    let before = summary_before(host);
    let nobs = host.frames().current().n_obs();
    let kept: Vec<u64> = (0..nobs)
        .filter(|r| sample.contains(*r) != delete_selected)
        .collect();
    let deleted = nobs - kept.len() as u64;
    if deleted == 0 {
        note_deleted(host, 0);
        return Ok(changed(host, before, DataChange::None));
    }
    let nvars = host.frames().current().n_vars();
    let frame = host.frames_mut().current_mut();
    frame.begin_command();
    for v in 0..nvars {
        let idx = VarIdx(v);
        let is_str = matches!(
            frame.var(idx).map(|x| x.ty),
            Some(StorageType::Str { .. }) | Some(StorageType::StrL)
        );
        if is_str {
            let vals: Vec<Vec<u8>> = kept
                .iter()
                .map(|r| {
                    frame
                        .col(idx)
                        .and_then(|c| c.get_bytes(*r))
                        .unwrap_or_default()
                        .to_vec()
                })
                .collect();
            let mut cm = match frame.col_mut(idx) {
                Ok(c) => c,
                Err(e) => {
                    frame.rollback();
                    return Err(frame_error(e, ""));
                }
            };
            for (dst, v) in vals.iter().enumerate() {
                let _ = cm.set_bytes(dst as u64, trim_nul(v));
            }
        } else {
            let vals: Vec<f64> = kept
                .iter()
                .map(|r| {
                    frame
                        .col(idx)
                        .and_then(|c| c.get_f64(*r))
                        .unwrap_or(stratum_core::SYSMISS)
                })
                .collect();
            let mut cm = match frame.col_mut(idx) {
                Ok(c) => c,
                Err(e) => {
                    frame.rollback();
                    return Err(frame_error(e, ""));
                }
            };
            for (dst, v) in vals.iter().enumerate() {
                let _ = cm.set_f64(dst as u64, *v);
            }
        }
    }
    frame.set_n_obs(kept.len() as u64);
    frame.commit();
    note_deleted(host, deleted);
    Ok(changed(host, before, DataChange::None))
}

// ---------------------------------------------------------------------------
// rename
// ---------------------------------------------------------------------------

/// `rename old new`.
///
/// Keeps the `VarId` and the column's `gen`, and bumps `var_layout` — which is
/// what makes a downstream block reading the new name stay Current while one
/// reading the old name goes `Broken` (the plan's rename acceptance bullet).
/// `Frame::rename_var` is where that happens.
pub fn rename(host: &mut dyn CmdHost, ast: &CommandAst) -> CmdResult {
    let s = slots(ast).ok_or_else(|| err::invalid("rename"))?;
    let names = varlist_names(s.varlist.as_ref());
    let [old, new] = names.as_slice() else {
        return Err(if names.len() < 2 {
            err::too_few_vars().at(ast.span)
        } else {
            err::too_many_vars().at(ast.span)
        });
    };
    let before = summary_before(host);
    let idx = host
        .frames()
        .current()
        .index_of(old)
        .ok_or_else(|| err::var_not_found(old).at(ast.span))?;
    if !stratum_data::variable::is_valid_name(new) {
        return Err(err::invalid_name(new).at(ast.span));
    }
    if host.frames().current().index_of(new).is_some() {
        return Err(err::already_defined(new).at(ast.span));
    }
    let frame = host.frames_mut().current_mut();
    frame.begin_command();
    match frame.rename_var(idx, new) {
        Ok(()) => frame.commit(),
        Err(e) => {
            frame.rollback();
            return Err(frame_error(e, new));
        }
    }
    Ok(changed(
        host,
        before,
        DataChange::Renamed(old.clone(), new.clone()),
    ))
}

// ---------------------------------------------------------------------------
// shared
// ---------------------------------------------------------------------------

/// Evaluate a numeric expression over the whole column: `sample` rows get the
/// expression's value, everything else gets `.`.
fn eval_over(
    host: &mut dyn CmdHost,
    expr: &stratum_parse::ast::expr::Expr,
    sample: &Sample,
    nobs: u64,
) -> Result<Vec<f64>, StataError> {
    let mut all = vec![stratum_core::SYSMISS; usize::try_from(nobs).unwrap_or(usize::MAX)];
    let mut buf = Vec::with_capacity(EVAL_CHUNK);
    for run in sample.runs() {
        let mut row = run.start;
        let end = run.start + run.len;
        while row < end {
            let len = usize::try_from((end - row).min(EVAL_CHUNK as u64)).expect("bounded");
            buf.clear();
            host.eval_num_rows(expr, row, len, &mut buf)?;
            for (i, v) in buf.iter().enumerate() {
                all[usize::try_from(row).unwrap_or(usize::MAX) + i] = *v;
            }
            row += len as u64;
        }
    }
    Ok(all)
}

fn eval_str_over(
    host: &mut dyn CmdHost,
    expr: &stratum_parse::ast::expr::Expr,
    sample: &Sample,
    nobs: u64,
) -> Result<Vec<String>, StataError> {
    let mut all = vec![String::new(); usize::try_from(nobs).unwrap_or(usize::MAX)];
    let mut buf: Vec<String> = Vec::with_capacity(EVAL_CHUNK);
    for run in sample.runs() {
        let mut row = run.start;
        let end = run.start + run.len;
        while row < end {
            let len = usize::try_from((end - row).min(EVAL_CHUNK as u64)).expect("bounded");
            buf.clear();
            host.eval_str_rows(expr, row, len, &mut buf)?;
            for (i, v) in buf.iter().enumerate() {
                all[usize::try_from(row).unwrap_or(usize::MAX) + i] = v.clone();
            }
            row += len as u64;
        }
    }
    Ok(all)
}

/// Build a typed column from doubles, promoting the type as far as the values
/// require. One allocation of the target type; the frame is not touched at all
/// until `add_column`, which is what keeps `generate` to a single `gen` bump.
fn build_column(requested: StorageType, values: &[f64]) -> Column {
    let mut ty = requested;
    // At most three steps up the ladder (byte → int → long → double), so the
    // loop is bounded by the ladder and not by the data.
    for _ in 0..4 {
        let mut promote = None;
        for v in values {
            if let Narrowed::NeedsPromotion(to) = narrow_to(ty, *v) {
                promote = Some(to);
                break;
            }
        }
        match promote {
            Some(to) if to != ty => ty = to,
            _ => break,
        }
    }
    match ty {
        StorageType::Byte => Column::Byte(NumCol::from_slice(
            &values
                .iter()
                .map(|v| narrow_or_missing(narrow_byte(*v)))
                .collect::<Vec<i8>>(),
        )),
        StorageType::Int => Column::Int(NumCol::from_slice(
            &values
                .iter()
                .map(|v| narrow_or_missing(narrow_int(*v)))
                .collect::<Vec<i16>>(),
        )),
        StorageType::Long => Column::Long(NumCol::from_slice(
            &values
                .iter()
                .map(|v| narrow_or_missing(narrow_long(*v)))
                .collect::<Vec<i32>>(),
        )),
        StorageType::Float => Column::Float(NumCol::from_slice(
            &values
                .iter()
                .map(|v| narrow_or_missing(narrow_float(*v)))
                .collect::<Vec<f32>>(),
        )),
        _ => Column::Double(NumCol::from_slice(
            &values.iter().map(|v| canon(*v)).collect::<Vec<f64>>(),
        )),
    }
}

/// The promotion the storage type would need for this value.
fn narrow_to(ty: StorageType, v: f64) -> Narrowed<()> {
    match ty {
        StorageType::Byte => narrow_byte(v).map_unit(),
        StorageType::Int => narrow_int(v).map_unit(),
        StorageType::Long => narrow_long(v).map_unit(),
        StorageType::Float => narrow_float(v).map_unit(),
        // `double` is the widest rung; nothing promotes past it.
        _ => Narrowed::Ok(()),
    }
}

trait MapUnit {
    fn map_unit(self) -> Narrowed<()>;
}

impl<T> MapUnit for Narrowed<T> {
    fn map_unit(self) -> Narrowed<()> {
        match self {
            Narrowed::Ok(_) => Narrowed::Ok(()),
            Narrowed::NeedsPromotion(t) => Narrowed::NeedsPromotion(t),
        }
    }
}

/// After the type has been chosen, nothing should need promoting; a value that
/// still does is stored as `.` rather than panicking, because a panic here is
/// reported as an internal error and hides the arithmetic that produced it.
fn narrow_or_missing<T: MissingOf>(n: Narrowed<T>) -> T {
    match n {
        Narrowed::Ok(v) => v,
        Narrowed::NeedsPromotion(_) => T::missing(),
    }
}

/// The storage-type-specific spelling of `.`.
trait MissingOf {
    fn missing() -> Self;
}

impl MissingOf for i8 {
    fn missing() -> Self {
        stratum_core::missing::BYTE_MISS
    }
}
impl MissingOf for i16 {
    fn missing() -> Self {
        stratum_core::missing::INT_MISS
    }
}
impl MissingOf for i32 {
    fn missing() -> Self {
        stratum_core::missing::LONG_MISS
    }
}
impl MissingOf for f32 {
    fn missing() -> Self {
        stratum_core::missing::SYSMISS_F32
    }
}

// A `widen(ty, raw)` helper used to sit here so the `replace` counter could
// compare stored bits with stored bits. It is gone, and deliberately: `replace`
// re-reads the cell through `Column::get_f64` AFTER the write, which is already
// the stored value widened by the column itself. Re-deriving the widening here
// would be a second answer to "what did that cell become", and the count of
// "real changes" — a number the user reads — must come from the column, not
// from this module's model of the column.

/// `(N missing values generated)`, only when N > 0.
fn note_missing(host: &mut dyn CmdHost, n: u64) {
    if n == 0 {
        return;
    }
    let mut out = Out::new();
    out.txt(&format!(
        "({n} missing value{} generated)",
        if n == 1 { "" } else { "s" }
    ));
    out.nl();
    host.emit(out.runs());
}

/// `(N real changes made[, M to missing])`. Always printed, even for zero —
/// `replace hi = 0 if mpg > 30` prints `(0 real changes made)` (verified).
fn note_changes(host: &mut dyn CmdHost, real: u64, to_missing: u64) {
    let mut out = Out::new();
    out.txt(&format!(
        "({real} real change{} made",
        if real == 1 { "" } else { "s" }
    ));
    if to_missing > 0 {
        out.txt(&format!(", {to_missing} to missing"));
    }
    out.txt(")");
    out.nl();
    host.emit(out.runs());
}

/// `(N observations deleted)`.
fn note_deleted(host: &mut dyn CmdHost, n: u64) {
    if n == 0 {
        return;
    }
    let mut out = Out::new();
    out.txt(&format!(
        "({n} observation{} deleted)",
        if n == 1 { "" } else { "s" }
    ));
    out.nl();
    host.emit(out.runs());
}

/// What changed, for the `DataChanged` payload the "✓ 0.08s · +1 var" chip
/// renders from.
enum DataChange {
    Created(String),
    Modified(String),
    Dropped(Vec<String>),
    Renamed(String, String),
    None,
}

fn summary_before(host: &dyn CmdHost) -> (u64, u32) {
    let f = host.frames().current();
    (f.n_obs(), f.n_vars())
}

fn changed(host: &dyn CmdHost, before: (u64, u32), what: DataChange) -> CmdOutcome {
    let f = host.frames().current();
    let mut s = DataChangeSummary {
        frame: f.name().to_string(),
        obs_before: before.0,
        obs_after: f.n_obs(),
        vars_before: before.1,
        vars_after: f.n_vars(),
        created: Vec::new(),
        modified: Vec::new(),
        dropped: Vec::new(),
        renamed: Vec::new(),
        notes: Vec::new(),
    };
    match what {
        DataChange::Created(n) => s.created.push(n),
        DataChange::Modified(n) => s.modified.push(n),
        DataChange::Dropped(ns) => s.dropped = ns,
        DataChange::Renamed(a, b) => s.renamed.push((a, b)),
        DataChange::None => {}
    }
    CmdOutcome::one(ResultPayload::DataChanged(s))
}

/// The `[type] newvar` head of a `generate`.
fn new_var_spec(
    vl: Option<&VarList>,
    ast: &CommandAst,
) -> Result<(Option<StorageType>, String), StataError> {
    let vl = vl.ok_or_else(|| err::too_few_vars().at(ast.span))?;
    match vl.items.as_slice() {
        [] => Err(err::too_few_vars().at(ast.span)),
        [item] => one_new_var(item, None),
        // `generate [type] newvar = exp`. The type arrives as its OWN varlist
        // item, not as a `VarPattern::Typed`: `stratum_parse`'s `typed_prefix`
        // forms that variant only for the parenthesised filter spelling
        // (`double(x)` in an existing varlist), so `gen byte hi = price > 6000`
        // — GOLDEN core_surface.log — is two items and the first is the type.
        [ty_item, item] => {
            let word = name_of(ty_item)?;
            let ty = storage_type_word(&word)
                // Two names and the first is not a type: `gen a b = 1`.
                .ok_or_else(|| err::too_many_vars().at(ast.span))?;
            one_new_var(item, Some(ty))
        }
        _ => Err(err::too_many_vars().at(ast.span)),
    }
}

/// One `newvar` varlist item, with an optional type already taken off the
/// front.
fn one_new_var(
    item: &stratum_parse::ast::varlist::VarItem,
    ty: Option<StorageType>,
) -> Result<(Option<StorageType>, String), StataError> {
    let VarItemKind::Single(atom) = &item.kind else {
        return Err(err::invalid("varlist").at(item.span));
    };
    match &atom.base {
        VarPattern::Name(n) => Ok((ty, n.clone())),
        // `gen double(x) = …` is not Stata syntax, but the parser can produce
        // it, and a type given twice is a user error rather than a panic.
        VarPattern::Typed {
            ty: inner_ty,
            inner,
        } if ty.is_none() => match inner.as_slice() {
            [VarPattern::Name(n)] => Ok((Some(*inner_ty), n.clone())),
            _ => Err(err::too_many_vars().at(item.span)),
        },
        other => Err(err::invalid_name(other.as_text()).at(item.span)),
    }
}

/// The single bare name a varlist item spells, for the `[type]` head.
fn name_of(item: &stratum_parse::ast::varlist::VarItem) -> Result<String, StataError> {
    match &item.kind {
        VarItemKind::Single(a) => Ok(a.base.as_text().to_owned()),
        VarItemKind::Interact { .. } => Err(err::invalid("varlist").at(item.span)),
    }
}

/// A Stata storage-type keyword, as `generate`/`egen` accept it.
///
/// The inverse of [`super::data::type_name`]. `str0` and anything wider than
/// `str2045` are not types, so they fall through to "this is a variable name",
/// which is what makes `gen str = 1` an ordinary r(198) rather than a panic.
fn storage_type_word(w: &str) -> Option<StorageType> {
    Some(match w {
        "byte" => StorageType::Byte,
        "int" => StorageType::Int,
        "long" => StorageType::Long,
        "float" => StorageType::Float,
        "double" => StorageType::Double,
        "strL" => StorageType::StrL,
        other => {
            let width: u16 = other.strip_prefix("str")?.parse().ok()?;
            if !(1..=2045).contains(&width) {
                return None;
            }
            StorageType::Str { width }
        }
    })
}

/// The bare names in a varlist, for commands that take names rather than
/// resolved positions (`rename`).
fn varlist_names(vl: Option<&VarList>) -> Vec<String> {
    let Some(vl) = vl else {
        return Vec::new();
    };
    vl.items
        .iter()
        .filter_map(|i| match &i.kind {
            VarItemKind::Single(a) => Some(a.base.as_text().to_owned()),
            VarItemKind::Interact { .. } => None,
        })
        .collect()
}

fn frame_error(e: stratum_data::FrameError, name: &str) -> StataError {
    let rc = u32::from(e.rc());
    let msg = format!("{e}");
    let err = StataError::new(rc, msg);
    if name.is_empty() {
        err
    } else {
        err.token(name)
    }
}

/// A barrier write refusal, as a Stata return code.
///
/// [`WriteError`](stratum_data::WriteError) has no `rc()` of its own because
/// only one of its two variants is a user-visible failure: `TypeMismatch` is
/// r(109), and `NeedsPromotion` is the barrier telling the caller which rung to
/// recast to. Every writer in this module recasts BEFORE it opens `col_mut`, so
/// reaching the promotion arm here means this module miscomputed the target
/// type — its `Display` says which rung was wanted, which is the useful thing
/// to put in front of whoever has to fix it.
fn write_error(e: stratum_data::WriteError, name: &str) -> StataError {
    let rc = u32::from(stratum_data::WriteError::RC_TYPE_MISMATCH);
    StataError::new(rc, format!("{e}")).token(name)
}

/// `Command::Known` guard used by the tests to build an AST by hand.
#[must_use]
pub fn is_known(ast: &CommandAst) -> bool {
    matches!(ast.cmd, Command::Known(_))
}
