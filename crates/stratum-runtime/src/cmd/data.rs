//! Describing and reordering the dataset: `describe`, `list`, `count`, `ds`,
//! `clear`, `sort`, `gsort`, `label`, `format`.
//!
//! # These renderers are byte-exact, and the goldens say so
//!
//! Every layout here is transcribed from `tests/golden/stata18/*.log` captured
//! from StataMP 18.5, down to the trailing spaces on a `describe` row with no
//! variable label and the two spaces between sort keys in `Sorted by:`. If this
//! code disagrees with a golden, this code is wrong.
//!
//! The goldens were captured at `set linesize 100`, so every renderer takes the
//! line width as a parameter rather than reading [`LINESIZE`](super::settings::LINESIZE)
//! itself. The engine only ever passes `c(linesize)`, which A16 pins at 80; the
//! parameter exists so the 100-column goldens remain checkable without
//! weakening the rejection. Same shape as `stratum_stats::*::classic_text(linesize)`.

use stratum_core::fmt::StataFormat;
use stratum_data::{variable::format_string, Frame, Sample, StorageType};
use stratum_parse::ast::CommandAst;
use stratum_parse::{StataError, VarlistMode};
use stratum_proto::{ScalarValue, SortDir, VarIdx};

use super::{
    build_sample, err, has_option, resolve_varlist, rest, rest_span, slots, CmdHost, CmdOutcome,
    CmdResult, Out, VarMetaEdit,
};

/// Column at which `describe`'s right-hand column starts: the data label, the
/// timestamp, `(_dta has notes)`, and every variable label.
const LABEL_COL: usize = 46;

/// `describe`'s field widths, summing to [`LABEL_COL`].
const W_NAME: usize = 16;
const W_TYPE: usize = 8;
const W_FMT: usize = 11;
const W_VALLAB: usize = 11;

/// `list`'s default `abbreviate()`.
const LIST_ABBREV: usize = 8;

/// `list`'s default `separator()`.
const LIST_SEPARATOR: u64 = 5;

/// How many listed rows are buffered before they are handed to the host.
///
/// `list` on a 10 M-row dataset must not build a 10 M-row `String` first
/// (design 03 §9.4 — output streams as it is produced). 256 rows is ~16 KB,
/// which is under the 64 KB coalescing granule and above the point where the
/// per-emit cost matters.
const EMIT_ROWS: u64 = 256;

// ---------------------------------------------------------------------------
// describe
// ---------------------------------------------------------------------------

/// `describe [varlist]`.
pub fn describe(host: &mut dyn CmdHost, ast: &CommandAst) -> CmdResult {
    let s = slots(ast).ok_or_else(|| err::invalid("describe"))?;
    if s.using.is_some() {
        // `describe using file.dta` needs the header-only .dta reader (W03).
        return Err(err::unsupported("describe using").at(ast.span));
    }
    let selected = resolve_varlist(host, s.varlist.as_ref(), VarlistMode::Existing, false)?;
    let whole_dataset = selected.is_empty();
    let idxs: Vec<u32> = if whole_dataset {
        (0..host.frames().current().n_vars()).collect()
    } else {
        selected
    };
    let linesize = usize::from(host.settings().linesize());

    let source = host.data_source().map(str::to_owned);
    let timestamp = host.data_timestamp().unwrap_or("").to_owned();

    let mut out = Out::new();
    out.nl();
    if whole_dataset {
        header_block(
            &mut out,
            host.frames().current(),
            source.as_deref(),
            &timestamp,
            linesize,
        );
    }
    column_headings(&mut out);
    if !whole_dataset {
        rule(&mut out, linesize);
    }
    let frame = host.frames().current();
    for &i in &idxs {
        let Some(v) = frame.var(VarIdx(i)) else {
            continue;
        };
        var_row(
            &mut out,
            &v.name,
            v.ty,
            &format_string(&v.format),
            v.value_label.as_deref().unwrap_or(""),
            &v.label,
        );
    }
    if whole_dataset {
        rule(&mut out, linesize);
        sorted_by(&mut out, frame);
        if frame.changed() {
            out.txt("     Note: Dataset has changed since last saved.");
            out.nl();
        }
    }

    let n = frame.n_obs();
    let k = frame.n_vars();
    host.emit(out.runs());
    host.clear_r();
    host.set_r("N", num(n as f64));
    host.set_r("k", num(f64::from(k)));
    Ok(CmdOutcome::text_only())
}

fn header_block(
    out: &mut Out,
    frame: &Frame,
    source: Option<&str>,
    timestamp: &str,
    linesize: usize,
) {
    match source {
        Some(path) => {
            out.txt("Contains data from ");
            out.res(path);
        }
        None => out.txt("Contains data"),
    }
    out.nl();
    // ` Observations:` and `    Variables:` are both 14 columns, the count is
    // right-aligned in the next 14, and the right column starts at 46.
    labelled_count(out, " Observations:", frame.n_obs(), frame.label());
    labelled_count(out, "    Variables:", u64::from(frame.n_vars()), timestamp);
    if !frame.chars().notes("_dta").is_empty() {
        out.spaces(LABEL_COL);
        out.txt("(_dta has notes)");
        out.nl();
    }
    rule(out, linesize);
}

fn labelled_count(out: &mut Out, caption: &str, n: u64, right: &str) {
    out.txt(caption);
    let text = commas(n);
    out.spaces(14usize.saturating_sub(text.len()));
    out.res(&text);
    // Padded to column 46 even when the right-hand text is empty — verified,
    // `semantics.log` line 205 carries the trailing spaces.
    let used = caption.len() + 14usize.max(text.len());
    out.spaces(LABEL_COL.saturating_sub(used));
    if !right.is_empty() {
        out.res(right);
    }
    out.nl();
}

/// The two heading lines, verbatim from the golden.
fn column_headings(out: &mut Out) {
    out.txt("Variable      Storage   Display    Value");
    out.nl();
    out.txt("    name         type    format    label      Variable label");
    out.nl();
}

fn var_row(out: &mut Out, name: &str, ty: StorageType, fmt: &str, vallab: &str, label: &str) {
    let shown = abbrev(name, W_NAME - 1);
    out.res(&shown);
    out.spaces(W_NAME.saturating_sub(shown.len()));
    let tyname = type_name(ty);
    out.txt(&tyname);
    out.spaces(W_TYPE.saturating_sub(tyname.len()));
    out.txt(fmt);
    out.spaces(W_FMT.saturating_sub(fmt.len()));
    out.txt(vallab);
    out.spaces(W_VALLAB.saturating_sub(vallab.len()));
    out.txt(label);
    out.nl();
}

fn sorted_by(out: &mut Out, frame: &Frame) {
    out.txt("Sorted by: ");
    // Two spaces between keys — `Sorted by: foreign  price`, verified.
    let mut first = true;
    let state = frame.sort_state();
    for key in state
        .keys
        .iter()
        .take(if state.valid { usize::MAX } else { 0 })
    {
        if !first {
            out.txt("  ");
        }
        first = false;
        if let Some(v) = frame.var(*key) {
            out.res(&v.name);
        }
    }
    out.nl();
}

/// Stata's spelling of a storage type, as `describe` prints it.
#[must_use]
pub fn type_name(ty: StorageType) -> String {
    match ty {
        StorageType::Byte => "byte".to_owned(),
        StorageType::Int => "int".to_owned(),
        StorageType::Long => "long".to_owned(),
        StorageType::Float => "float".to_owned(),
        StorageType::Double => "double".to_owned(),
        StorageType::Str { width } => format!("str{width}"),
        StorageType::StrL => "strL".to_owned(),
    }
}

// ---------------------------------------------------------------------------
// list
// ---------------------------------------------------------------------------

/// `list [varlist] [if] [in] [, noobs separator(#) abbreviate(#) ...]`.
pub fn list(host: &mut dyn CmdHost, ast: &CommandAst) -> CmdResult {
    let s = slots(ast).ok_or_else(|| err::invalid("list"))?;
    let opts = super::take_options(
        s,
        &[
            "abbreviate",
            "clean",
            "compress",
            "divider",
            "noobs",
            "separator",
            "string",
            "table",
        ],
    )?;
    let abbrev_to = opt_usize(&opts, "abbreviate").unwrap_or(LIST_ABBREV).max(2);
    let sep_every = opt_usize(&opts, "separator")
        .map(|n| n as u64)
        .unwrap_or(LIST_SEPARATOR);
    let noobs = has_option(s, "noobs");

    let idxs = resolve_varlist(host, s.varlist.as_ref(), VarlistMode::Existing, true)?;
    let sample = build_sample(host, s.if_.as_ref(), s.in_.as_ref())?;
    let linesize = usize::from(host.settings().linesize());

    let frame = host.frames().current();
    if idxs.is_empty() || sample.is_empty() {
        // Stata prints nothing at all for an empty selection.
        return Ok(CmdOutcome::text_only());
    }
    let cols: Vec<Col> = idxs
        .iter()
        .map(|&i| measure(frame, VarIdx(i), &sample, abbrev_to))
        .collect();

    let gutter = if noobs {
        0
    } else {
        3.max(digits(sample_max(&sample)))
    };
    let mut blocks = split_to_width(&cols, gutter, linesize);
    if blocks.is_empty() {
        blocks.push(0..cols.len());
    }

    // The frame is re-borrowed per BATCH rather than held across the whole
    // loop. `host.emit` needs `&mut host`, so a `&Frame` taken once from
    // `host.frames()` would keep a shared borrow alive across it and this would
    // not compile — but the batching is not a borrow-checker concession. It is
    // what makes `list` stream: design 03 §9.4 requires output to reach the
    // card as it is produced, and a 10 M-row `list` must never build a 10 M-row
    // buffer first. The scratch vector is [`EMIT_ROWS`] long, never `_N`.
    let mut out = Out::new();
    for block in blocks {
        let group = &cols[block];
        let inner: usize =
            1 + group.iter().map(|c| c.width).sum::<usize>() + 3 * (group.len() - 1) + 1;
        out.nl();
        border(&mut out, gutter, inner, '+');
        head_row(&mut out, gutter, group);
        border(&mut out, gutter, inner, '|');
        let total = sample.len();
        let mut shown = 0u64;
        let mut rows = sample.runs().flat_map(|r| r.start..r.start + r.len);
        let mut batch: Vec<u64> = Vec::with_capacity(EMIT_ROWS as usize);
        loop {
            batch.clear();
            batch.extend(rows.by_ref().take(EMIT_ROWS as usize));
            if batch.is_empty() {
                break;
            }
            {
                let frame = host.frames().current();
                for &obs in &batch {
                    data_row(&mut out, gutter, group, frame, obs);
                    shown += 1;
                    if sep_every > 0 && shown.is_multiple_of(sep_every) && shown < total {
                        border(&mut out, gutter, inner, '|');
                    }
                }
            }
            host.emit(out.runs());
            out = Out::new();
        }
        border(&mut out, gutter, inner, '+');
    }
    host.emit(out.runs());
    Ok(CmdOutcome::text_only())
}

/// One rendered column: its header text, its width, and how to draw a cell.
struct Col {
    idx: VarIdx,
    header: String,
    width: usize,
    left: bool,
}

fn measure(frame: &Frame, idx: VarIdx, sample: &Sample, abbrev_to: usize) -> Col {
    let var = frame.var(idx).expect("resolved varlist indexes exist");
    let header = abbrev(&var.name, abbrev_to);
    let left = matches!(var.ty, StorageType::Str { .. } | StorageType::StrL);
    let mut width = header.len();
    for obs in sample.runs().flat_map(|r| r.start..r.start + r.len) {
        width = width.max(cell(frame, idx, obs).len());
    }
    Col {
        idx,
        header,
        width,
        left,
    }
}

/// One cell's text, with no padding.
fn cell(frame: &Frame, idx: VarIdx, obs: u64) -> String {
    let var = frame.var(idx).expect("caller resolved the index");
    let col = frame.col(idx).expect("a variable has a column");
    match var.ty {
        StorageType::Str { .. } | StorageType::StrL => {
            let raw = col.get_bytes(obs).unwrap_or_default();
            String::from_utf8_lossy(raw).into_owned()
        }
        _ => {
            let v = col.get_f64(obs).unwrap_or(stratum_core::SYSMISS);
            if let Some(name) = var.value_label.as_deref() {
                if let Some(text) = frame.labels().get(name).and_then(|t| t.get(v)) {
                    return text.to_owned();
                }
            }
            var.format.format_f64(v).trim_start().to_owned()
        }
    }
}

fn border(out: &mut Out, gutter: usize, inner: usize, corner: char) {
    out.spaces(gutter + 2);
    out.txt(&corner.to_string());
    out.txt(&"-".repeat(inner));
    out.txt(&corner.to_string());
    out.nl();
}

fn head_row(out: &mut Out, gutter: usize, cols: &[Col]) {
    out.spaces(gutter + 2);
    out.txt("| ");
    for (n, c) in cols.iter().enumerate() {
        if n > 0 {
            out.txt("   ");
        }
        pad(out, &c.header, c.width, c.left, false);
    }
    out.txt(" |");
    out.nl();
}

fn data_row(out: &mut Out, gutter: usize, cols: &[Col], frame: &Frame, obs: u64) {
    if gutter > 0 {
        let n = (obs + 1).to_string();
        out.spaces(gutter.saturating_sub(n.len()));
        out.txt(&n);
        out.txt(". ");
    }
    out.txt("| ");
    for (n, c) in cols.iter().enumerate() {
        if n > 0 {
            out.txt("   ");
        }
        pad(out, &cell(frame, c.idx, obs), c.width, c.left, true);
    }
    out.txt(" |");
    out.nl();
}

fn pad(out: &mut Out, text: &str, width: usize, left: bool, value: bool) {
    let fill = width.saturating_sub(text.chars().count());
    if !left {
        out.spaces(fill);
    }
    if value {
        out.res(text);
    } else {
        out.txt(text);
    }
    if left {
        out.spaces(fill);
    }
}

/// Split columns into consecutive groups that fit inside `linesize`.
///
/// Stata renders a too-wide `list` as successive tables over the same
/// observations rather than truncating; a group is never empty, so a single
/// column wider than the line still renders (over-wide, like Stata).
fn split_to_width(cols: &[Col], gutter: usize, linesize: usize) -> Vec<core::ops::Range<usize>> {
    let mut blocks = Vec::new();
    let mut start = 0;
    let mut used = 0usize;
    for (i, c) in cols.iter().enumerate() {
        let add = if i == start { c.width } else { c.width + 3 };
        // gutter + 2 for "n. ", "| " and " |" is 4 more.
        if i > start && gutter + 2 + 4 + used + add > linesize {
            blocks.push(start..i);
            start = i;
            used = c.width;
        } else {
            used += add;
        }
    }
    if start < cols.len() {
        blocks.push(start..cols.len());
    }
    blocks
}

fn sample_max(sample: &Sample) -> u64 {
    sample.runs().map(|r| r.start + r.len).max().unwrap_or(0)
}

fn digits(n: u64) -> usize {
    n.to_string().len()
}

// ---------------------------------------------------------------------------
// count
// ---------------------------------------------------------------------------

/// `count [if] [in]`. Sets `r(N)`.
pub fn count(host: &mut dyn CmdHost, ast: &CommandAst) -> CmdResult {
    let s = slots(ast).ok_or_else(|| err::invalid("count"))?;
    let sample = build_sample(host, s.if_.as_ref(), s.in_.as_ref())?;
    let n = sample.len();
    let mut out = Out::new();
    out.txt("  ");
    out.res(&commas(n));
    out.nl();
    host.emit(out.runs());
    host.clear_r();
    host.set_r("N", num(n as f64));
    Ok(CmdOutcome::text_only())
}

// ---------------------------------------------------------------------------
// ds
// ---------------------------------------------------------------------------

/// `ds [varlist]` — variable names in storage order, laid out COLUMN-major.
///
/// Column-major is not a detail: `ds` on `auto.dta` prints `make` then `mpg`
/// across the first row and `price` then `rep78` on the second, which only
/// makes sense if the layout fills down each column first (verified,
/// `semantics.log`).
pub fn ds(host: &mut dyn CmdHost, ast: &CommandAst) -> CmdResult {
    let s = slots(ast).ok_or_else(|| err::invalid("ds"))?;
    let idxs = resolve_varlist(host, s.varlist.as_ref(), VarlistMode::Existing, true)?;
    let frame = host.frames().current();
    let names: Vec<&str> = idxs
        .iter()
        .filter_map(|&i| frame.var(VarIdx(i)).map(|v| &*v.name))
        .collect();
    if names.is_empty() {
        return Ok(CmdOutcome::text_only());
    }
    let linesize = usize::from(host.settings().linesize());
    let w = names.iter().map(|n| n.len()).max().unwrap_or(1) + 2;
    let max_cols = (linesize / w).max(1);
    // Balance: pick the row count the widest layout implies, then use only as
    // many columns as that many rows actually needs. 12 names at 7 columns is
    // 2 rows, and 2 rows of 12 names is 6 columns — which is what Stata prints.
    let rows = names.len().div_ceil(max_cols).max(1);
    let ncols = names.len().div_ceil(rows);

    let mut out = Out::new();
    for r in 0..rows {
        // The last column this row actually reaches. A short final row must not
        // carry the pad of a column it has no name in, or the line ends in
        // trailing blanks that the byte comparison would catch.
        let last = (0..ncols)
            .rfind(|c| names.len() > c * rows + r)
            .unwrap_or(0);
        for c in 0..=last {
            let Some(n) = names.get(c * rows + r) else {
                continue;
            };
            out.res(n);
            if c < last {
                out.spaces(w - n.len());
            }
        }
        out.nl();
    }
    host.emit(out.runs());
    Ok(CmdOutcome::text_only())
}

// ---------------------------------------------------------------------------
// clear, sort, gsort
// ---------------------------------------------------------------------------

/// `clear` — drop the data in memory. Silent, like Stata.
pub fn clear(host: &mut dyn CmdHost, _ast: &CommandAst) -> CmdResult {
    let name = host.frames().current_name().to_string();
    *host.frames_mut().current_mut() = Frame::new(&name);
    // The data in memory no longer came from anywhere: `describe` prints the
    // bare "Contains data" until the next load.
    host.clear_data_source();
    host.clear_r();
    Ok(CmdOutcome::text_only())
}

/// `sort varlist [in]` — ascending, stable, silent.
pub fn sort(host: &mut dyn CmdHost, ast: &CommandAst) -> CmdResult {
    let s = slots(ast).ok_or_else(|| err::invalid("sort"))?;
    let idxs = resolve_varlist(host, s.varlist.as_ref(), VarlistMode::Existing, false)?;
    if idxs.is_empty() {
        return Err(err::required("varlist"));
    }
    let keys: Vec<(VarIdx, SortDir)> = idxs.iter().map(|&i| (VarIdx(i), SortDir::Asc)).collect();
    apply_sort(host, &keys)
}

/// `gsort [+|-]var [[+|-]var ...]` — the `-` prefix is descending.
pub fn gsort(host: &mut dyn CmdHost, ast: &CommandAst) -> CmdResult {
    let text = rest(ast).trim();
    let span = rest_span(ast);
    if text.is_empty() {
        return Err(err::required("varlist").at(span));
    }
    let mut keys = Vec::new();
    {
        let frame = host.frames().current();
        for word in text.split_whitespace() {
            let (dir, name) = match word.as_bytes().first() {
                Some(b'-') => (SortDir::Desc, &word[1..]),
                Some(b'+') => (SortDir::Asc, &word[1..]),
                _ => (SortDir::Asc, word),
            };
            let idx = frame
                .index_of(name)
                .ok_or_else(|| err::var_not_found(name).at(span))?;
            keys.push((idx, dir));
        }
    }
    apply_sort(host, &keys)
}

fn apply_sort(host: &mut dyn CmdHost, keys: &[(VarIdx, SortDir)]) -> CmdResult {
    let frame = host.frames_mut().current_mut();
    frame.begin_command();
    match frame.sort_by(keys) {
        Ok(()) => {
            frame.commit();
            Ok(CmdOutcome::text_only())
        }
        Err(e) => {
            frame.rollback();
            Err(StataError::new(u32::from(e.rc()), format!("{e}")))
        }
    }
}

// ---------------------------------------------------------------------------
// label, format
// ---------------------------------------------------------------------------

/// `label variable|data|define|values|list|drop …`.
pub fn label(host: &mut dyn CmdHost, ast: &CommandAst) -> CmdResult {
    let text = rest(ast).trim();
    let span = rest_span(ast);
    let mut words = Words::new(text);
    let sub = words
        .word()
        .ok_or_else(|| err::invalid("label").at(span))?
        .to_owned();
    match sub.as_str() {
        "variable" | "var" | "vari" | "varia" | "variab" | "variabl" => {
            let name = words.word().ok_or_else(|| err::too_few_vars().at(span))?;
            let idx = host
                .frames()
                .current()
                .index_of(name)
                .ok_or_else(|| err::var_not_found(name).at(span))?;
            let text = words.quoted_rest();
            host.edit_var_meta(idx, VarMetaEdit::Label(text))?;
            Ok(CmdOutcome::text_only())
        }
        "data" => {
            let text = words.quoted_rest();
            let frame = host.frames_mut().current_mut();
            frame.begin_command();
            frame.set_label(&text);
            frame.commit();
            Ok(CmdOutcome::text_only())
        }
        "define" | "def" | "defi" | "defin" => label_define(host, &mut words, span),
        "values" | "val" | "valu" | "value" => {
            let name = words.word().ok_or_else(|| err::too_few_vars().at(span))?;
            let idx = host
                .frames()
                .current()
                .index_of(name)
                .ok_or_else(|| err::var_not_found(name).at(span))?;
            // `label values price nosuchlabel` is rc 0 in Stata — the
            // attachment is allowed to name a table that does not exist yet
            // (verified, `errors.log`). `.` detaches.
            let table = words.word().map(str::to_owned);
            let attach = match table.as_deref() {
                None | Some(".") => None,
                Some(t) => Some(t.to_owned()),
            };
            host.edit_var_meta(idx, VarMetaEdit::ValueLabel(attach))?;
            Ok(CmdOutcome::text_only())
        }
        "list" => label_list(host, &mut words),
        "drop" => {
            let name = words.word().ok_or_else(|| err::too_few_vars().at(span))?;
            let frame = host.frames_mut().current_mut();
            frame.begin_command();
            frame.labels_mut().drop_table(name);
            frame.commit();
            Ok(CmdOutcome::text_only())
        }
        other => Err(err::invalid(other).at(span)),
    }
}

fn label_define(
    host: &mut dyn CmdHost,
    words: &mut Words<'_>,
    span: stratum_proto::Span,
) -> CmdResult {
    let name = words
        .word()
        .ok_or_else(|| err::too_few_vars().at(span))?
        .to_owned();
    let mut pairs = Vec::new();
    while let Some(k) = words.word() {
        if k.starts_with(',') {
            break;
        }
        let key: i32 = k.parse().map_err(|_| err::invalid(k).at(span))?;
        let text = words
            .quoted_word()
            .ok_or_else(|| err::invalid(&name).at(span))?;
        pairs.push((key, text));
    }
    let frame = host.frames_mut().current_mut();
    frame.begin_command();
    let table = frame.labels_mut().entry(&name);
    for (k, t) in pairs {
        table.insert(k, t);
    }
    frame.commit();
    Ok(CmdOutcome::text_only())
}

fn label_list(host: &mut dyn CmdHost, words: &mut Words<'_>) -> CmdResult {
    let wanted: Vec<String> = core::iter::from_fn(|| words.word().map(str::to_owned)).collect();
    let frame = host.frames().current();
    let names: Vec<_> = if wanted.is_empty() {
        frame.labels().names()
    } else {
        wanted.iter().map(|s| s.as_str().into()).collect()
    };
    let mut out = Out::new();
    for name in names {
        let Some(table) = frame.labels().get(&name) else {
            continue;
        };
        out.txt(&name);
        out.txt(":");
        out.nl();
        for (k, t) in table.iter() {
            out.spaces(11usize.saturating_sub(k.to_string().len()));
            out.res(&k.to_string());
            out.txt(" ");
            out.res(t);
            out.nl();
        }
    }
    host.emit(out.runs());
    Ok(CmdOutcome::text_only())
}

/// `format [%fmt] varlist` or `format varlist %fmt`.
pub fn format(host: &mut dyn CmdHost, ast: &CommandAst) -> CmdResult {
    let text = rest(ast).trim();
    let span = rest_span(ast);
    let mut fmt: Option<String> = None;
    let mut names: Vec<&str> = Vec::new();
    for word in text.split_whitespace() {
        if word.starts_with('%') {
            fmt = Some(word.to_owned());
        } else {
            names.push(word);
        }
    }
    let Some(fmt) = fmt else {
        return Err(err::invalid("format").at(span));
    };
    let parsed = StataFormat::parse(&fmt).map_err(|_| err::invalid(&fmt).at(span))?;
    let mut idxs = Vec::with_capacity(names.len());
    {
        let frame = host.frames().current();
        for n in &names {
            idxs.push(
                frame
                    .index_of(n)
                    .ok_or_else(|| err::var_not_found(n).at(span))?,
            );
        }
    }
    for idx in idxs {
        host.edit_var_meta(idx, VarMetaEdit::Format(parsed))?;
    }
    Ok(CmdOutcome::text_only())
}

// ---------------------------------------------------------------------------
// shared helpers
// ---------------------------------------------------------------------------

/// Stata's `abbrev()`: `abbrev("leadprice", 8)` is `"leadpr~e"` — the first
/// `n-2` characters, a tilde, and the last character (verified,
/// `semantics.log`).
#[must_use]
pub fn abbrev(name: &str, n: usize) -> String {
    if name.chars().count() <= n || n < 3 {
        return name.to_owned();
    }
    let head: String = name.chars().take(n - 2).collect();
    let last = name.chars().last().expect("non-empty");
    format!("{head}~{last}")
}

fn rule(out: &mut Out, linesize: usize) {
    out.txt(&"-".repeat(linesize));
    out.nl();
}

/// An integer with Stata's thousands separators.
fn commas(n: u64) -> String {
    stratum_core::fmt::fmt_fc(n as f64, 21, 0)
        .trim_start()
        .to_owned()
}

fn num(v: f64) -> ScalarValue {
    ScalarValue::Num {
        value: v,
        display: stratum_core::fmt::fmt_g(v, 10).trim_start().to_owned(),
    }
}

fn opt_usize(opts: &[(String, Option<String>)], name: &str) -> Option<usize> {
    opts.iter()
        .find(|(k, _)| k == name)
        .and_then(|(_, v)| v.as_ref())
        .and_then(|v| v.trim().parse().ok())
}

/// A whitespace/quote-aware word reader for the `REST`-slot commands.
///
/// `label variable mileage "Miles per gallon"` cannot be `split_whitespace`:
/// the label is one word with spaces in it. This is the mini-parser design 02
/// §6.2 says each command runs over its own `rest` slot.
pub struct Words<'a> {
    src: &'a str,
    pos: usize,
}

impl<'a> Words<'a> {
    /// Read over `src`.
    #[must_use]
    pub fn new(src: &'a str) -> Self {
        Self { src, pos: 0 }
    }

    fn skip_ws(&mut self) {
        while self.src[self.pos..].starts_with(' ') || self.src[self.pos..].starts_with('\t') {
            self.pos += 1;
        }
    }

    /// The next whitespace-delimited word.
    pub fn word(&mut self) -> Option<&'a str> {
        self.skip_ws();
        if self.pos >= self.src.len() {
            return None;
        }
        let start = self.pos;
        while self.pos < self.src.len()
            && !self.src[self.pos..].starts_with(' ')
            && !self.src[self.pos..].starts_with('\t')
        {
            self.pos += 1;
        }
        Some(&self.src[start..self.pos])
    }

    /// The next word, with surrounding `"…"` removed if present.
    pub fn quoted_word(&mut self) -> Option<String> {
        self.skip_ws();
        if self.src[self.pos..].starts_with('"') {
            self.pos += 1;
            let start = self.pos;
            while self.pos < self.src.len() && !self.src[self.pos..].starts_with('"') {
                self.pos += 1;
            }
            let text = self.src[start..self.pos].to_owned();
            if self.pos < self.src.len() {
                self.pos += 1;
            }
            return Some(text);
        }
        self.word().map(str::to_owned)
    }

    /// Everything left, with one layer of `"…"` removed.
    pub fn quoted_rest(&mut self) -> String {
        self.skip_ws();
        let rest = self.src[self.pos..].trim();
        self.pos = self.src.len();
        rest.strip_prefix('"')
            .and_then(|r| r.strip_suffix('"'))
            .unwrap_or(rest)
            .to_owned()
    }
}
