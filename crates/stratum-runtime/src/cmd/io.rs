//! File-facing commands: `use`, `sysuse`, `save`, `cd`, `pwd`, `erase`.
//!
//! # Nothing here touches the filesystem
//!
//! Every path in this module goes through [`CmdHost`], which is `ExecCtx` —
//! the ONE ambient access to env, clock and fs, and the thing that records the
//! read so the `FileStamp` half of a block's dependency footprint exists
//! (design 03 §4.6). A `std::fs::File::open` in this directory would make a
//! block's staleness silently wrong, which is why the seam is shaped as
//! "ask the host to load" rather than "give me a reader".
//!
//! # Atomicity
//!
//! `save` is `Atomicity::External`: the state rolls back, the written file does
//! not, and the caller records `rolled_back: false` and invalidates the
//! `FileStamp` for the path (ARCHITECTURE §7.6). This module reports the write
//! and does not pretend it can be undone.

use camino::{Utf8Path, Utf8PathBuf};
use stratum_parse::ast::CommandAst;
use stratum_parse::StataError;

use super::{err, has_option, rest, rest_span, slots, CmdHost, CmdOutcome, CmdResult, Out};

/// `use filename [, clear]`.
///
/// The `_` suffix is Rust's, not Stata's: `use` is a keyword.
pub fn use_(host: &mut dyn CmdHost, ast: &CommandAst) -> CmdResult {
    let s = slots(ast).ok_or_else(|| err::invalid("use"))?;
    let span = rest_span(ast);
    let clear = has_option(s, "clear");
    let raw = s
        .using
        .as_ref()
        .map(|f| f.raw.clone())
        .or_else(|| first_word(rest(ast)))
        .ok_or_else(|| err::invalid("filename").at(span))?;
    let path = resolve(host, &raw);
    let report = host.load_dta(&path, clear).map_err(|e| located(e, span))?;
    announce(host, &report.label);
    Ok(CmdOutcome::text_only())
}

/// `sysuse name [, clear]` — a shipped example dataset.
pub fn sysuse(host: &mut dyn CmdHost, ast: &CommandAst) -> CmdResult {
    let s = slots(ast).ok_or_else(|| err::invalid("sysuse"))?;
    let span = rest_span(ast);
    let clear = has_option(s, "clear");
    let name = first_word(rest(ast)).ok_or_else(|| err::invalid("filename").at(span))?;
    if name == "dir" {
        return Err(err::unsupported("sysuse dir").at(span));
    }
    let path = host.sysuse_path(&name).map_err(|e| located(e, span))?;
    let report = host.load_dta(&path, clear).map_err(|e| located(e, span))?;
    announce(host, &report.label);
    Ok(CmdOutcome::text_only())
}

/// `(1978 automobile data)` — the dataset label in parentheses, and nothing at
/// all when the file has no label.
fn announce(host: &mut dyn CmdHost, label: &str) {
    if label.is_empty() {
        return;
    }
    let mut out = Out::new();
    out.txt("(");
    out.res(label);
    out.txt(")");
    out.nl();
    host.emit(out.runs());
}

/// `save [filename] [, replace]`.
pub fn save(host: &mut dyn CmdHost, ast: &CommandAst) -> CmdResult {
    let s = slots(ast).ok_or_else(|| err::invalid("save"))?;
    let span = rest_span(ast);
    let replace = has_option(s, "replace");
    let raw = first_word(rest(ast))
        .or_else(|| host.data_source().map(str::to_owned))
        .ok_or_else(|| err::invalid("filename").at(span))?;
    let mut path = resolve(host, &raw);
    if path.extension().is_none() {
        path.set_extension("dta");
    }
    let existed = host.file_exists(&path);
    if existed && !replace {
        return Err(err::file_already_exists(path.as_str()).at(span));
    }
    let mut out = Out::new();
    if replace && !existed {
        // Stata says so out loud: `(file … not found)` precedes the save when
        // `replace` was given and there was nothing to replace (verified,
        // `core_surface.log`).
        out.txt("(file ");
        out.res(path.as_str());
        out.txt(" not found)");
        out.nl();
    }
    host.save_dta(&path, replace)
        .map_err(|e| located(e, span))?;
    out.txt("file ");
    out.res(path.as_str());
    out.txt(" saved as .dta format");
    out.nl();
    host.emit(out.runs());
    Ok(CmdOutcome::text_only())
}

/// `cd [path]`.
pub fn cd(host: &mut dyn CmdHost, ast: &CommandAst) -> CmdResult {
    let text = rest(ast).trim();
    if text.is_empty() {
        return pwd(host, ast);
    }
    let path = resolve(host, &unquote(text));
    host.set_cwd(&path)
        .map_err(|e| located(e, rest_span(ast)))?;
    let now = host.cwd().to_string();
    let mut out = Out::new();
    out.res(&now);
    out.nl();
    host.emit(out.runs());
    Ok(CmdOutcome::text_only())
}

/// `pwd`.
pub fn pwd(host: &mut dyn CmdHost, _ast: &CommandAst) -> CmdResult {
    let now = host.cwd().to_string();
    let mut out = Out::new();
    out.res(&now);
    out.nl();
    host.emit(out.runs());
    Ok(CmdOutcome::text_only())
}

/// `erase filename`. Silent on success, like Stata.
pub fn erase(host: &mut dyn CmdHost, ast: &CommandAst) -> CmdResult {
    let span = rest_span(ast);
    let raw = first_word(rest(ast)).ok_or_else(|| err::invalid("filename").at(span))?;
    let path = resolve(host, &raw);
    host.erase_file(&path).map_err(|e| located(e, span))?;
    Ok(CmdOutcome::text_only())
}

/// Give a host error the command's span when it has none.
///
/// `load_dta`, `save_dta` and `erase_file` are the host's, and the host does
/// not know where in the do-file the path was written. Every r(601)/r(602) this
/// module returns must be underlineable in the editor, so the span is attached
/// on the way out rather than left to whichever host raised it.
fn located(e: StataError, span: stratum_proto::Span) -> StataError {
    match e.span {
        Some(_) => e,
        None => e.at(span),
    }
}

/// Make a user-written path absolute against the recorded cwd.
///
/// `ExecCtx::cwd` and not `std::env::current_dir`: a clean run sets cwd to the
/// entry `.do`'s directory (ARCHITECTURE §7.7) and the process cwd is not it.
fn resolve(host: &dyn CmdHost, raw: &str) -> Utf8PathBuf {
    let p = Utf8Path::new(unquote(raw).as_str()).to_owned();
    if p.is_absolute() {
        p
    } else {
        host.cwd().join(p)
    }
}

fn unquote(s: &str) -> String {
    let t = s.trim();
    t.strip_prefix('"')
        .and_then(|x| x.strip_suffix('"'))
        .unwrap_or(t)
        .to_owned()
}

/// The first word of a `REST` tail, stopping at the option comma.
fn first_word(text: &str) -> Option<String> {
    let head = text.split(',').next().unwrap_or("").trim();
    if head.is_empty() {
        return None;
    }
    if let Some(rest) = head.strip_prefix('"') {
        let end = rest.find('"').unwrap_or(rest.len());
        return Some(rest[..end].to_owned());
    }
    head.split_whitespace().next().map(str::to_owned)
}

/// `erase`'s error, shaped like Stata's, for hosts that report a bare io error.
#[must_use]
pub fn erase_failed(path: &str) -> StataError {
    StataError::new(601, format!("file {path} not found")).token(path)
}
