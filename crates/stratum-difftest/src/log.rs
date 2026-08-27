//! Reading a Stata batch log: banner, echoed commands, output blocks, and the
//! return code.
//!
//! # Never trust the exit code
//!
//! `stata-mp -b` returns 0 unconditionally — on `r(111)`, and even on an
//! explicit `exit _rc`. The only truthful record of how a batch run ended is
//! the log itself: a failed run prints `r(NNN);` on a line of its own. That is
//! what [`final_rc`] reads, and nothing in this crate ever looks at Stata's
//! process exit status for anything but "did it run at all".
//!
//! # The banner is data we must tolerate, not parse
//!
//! A batch log opens with the Stata banner: logo, version, licence and serial
//! lines, session notes. The committed corpus has its licence lines replaced
//! with `[redacted]` — the goldens are public, the licence is not — so nothing
//! here may assume the banner's exact shape. [`body`] simply skips to the
//! first `. do ` echo, which is the first line batch mode prints after the
//! banner in every variant we have seen, redacted or live.
//!
//! # The banner is also the one place a log carries PII
//!
//! `Serial number:` and `Licensed to:` (plus its indented continuation, the
//! institution) name a person. A live log is private to the machine that
//! produced it, but any EXCERPT of one that reaches a message — a SKIP
//! reason, a mismatch detail — ends up in a terminal, a CI log, a pasted bug
//! report. [`redact_licence`] rewrites those lines to the committed corpus's
//! own `[redacted]` convention, and every excerpt this crate prints goes
//! through it first.

/// Rewrite the banner's licence block to the corpus's `[redacted]` form,
/// preserving line structure: the three labelled lines keep their label and
/// lose their value; the indented continuation after `Licensed to:` (the
/// institution) keeps its indent and becomes `[redacted]`. Every other line
/// is untouched. Idempotent — a log that is already redacted comes back
/// byte-identical — so the goldens are a fixed point.
///
/// Over-redaction is the safe direction: a body line that merely looks like
/// a licence label is redacted too, and so is any indented run of lines
/// directly under `Licensed to:`. This is for diagnostic excerpts, where a
/// lost line of context costs far less than a leaked name.
#[must_use]
pub fn redact_licence(log: &str) -> String {
    const LABELS: [&str; 3] = ["Stata license:", "Serial number:", "Licensed to:"];
    let mut out = String::with_capacity(log.len());
    let mut under_licensee = false;
    for segment in log.split_inclusive('\n') {
        let content_end = segment.trim_end_matches(['\r', '\n']).len();
        let (content, terminator) = segment.split_at(content_end);
        let indent_len = content.len() - content.trim_start().len();
        let (indent, text) = content.split_at(indent_len);
        if let Some(label) = LABELS.iter().find(|l| text.starts_with(*l)) {
            out.push_str(indent);
            out.push_str(label);
            out.push_str(" [redacted]");
            under_licensee = *label == "Licensed to:";
        } else if under_licensee && indent_len > 0 && !text.is_empty() {
            out.push_str(indent);
            out.push_str("[redacted]");
        } else {
            out.push_str(content);
            under_licensee = false;
        }
        out.push_str(terminator);
    }
    out
}

/// The log body: everything after the banner (exclusive of the `. do` echo
/// line itself). When no `. do ` echo exists the whole text is the body, so a
/// fragment pasted from mid-log still parses.
#[must_use]
pub fn body(log: &str) -> &str {
    let mut start = 0usize;
    while start < log.len() {
        let end = log[start..]
            .find('\n')
            .map_or(log.len(), |nl| start + nl + 1);
        let line = log[start..end].trim_end();
        if line.starts_with(". do ") || line.starts_with(". run ") {
            return &log[end..];
        }
        start = end;
    }
    log
}

/// The final Stata return code recorded in the log: the **last** `r(NNN);`
/// standing alone on a line, or 0 when none is present. Parsed from the log
/// because the process exit code is meaningless (module header).
#[must_use]
pub fn final_rc(log: &str) -> u32 {
    let mut rc = 0;
    for line in log.lines() {
        if let Some(code) = parse_rc_line(line) {
            rc = code;
        }
    }
    rc
}

/// `r(NNN);` exactly — the anchored `^r\((\d+)\);$` of the acceptance bullet,
/// with trailing whitespace tolerated because Stata pads some log lines.
fn parse_rc_line(line: &str) -> Option<u32> {
    let t = line.trim_end();
    let inner = t.strip_prefix("r(")?.strip_suffix(");")?;
    if inner.is_empty() || !inner.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    inner.parse().ok()
}

/// The output block that followed the `occurrence`-th echo of `cmd`.
///
/// A batch log echoes each command as `. cmd` (continuations as `> …`), then
/// prints the command's output verbatim until the next echo. The block
/// returned is those raw bytes with one leading blank line and any trailing
/// blank lines removed — exactly the cut the byte-exact goldens use — joined
/// with `\n` and ending in `\n`.
///
/// Interior blank lines are preserved: they are part of the layout under
/// test. Nothing inside the block is normalized, because the corpus channel
/// is byte-exact and a normalizer that "helps" here would hide the exact
/// class of bug this harness exists to catch.
#[must_use]
pub fn command_output(log_body: &str, cmd: &str, occurrence: usize) -> Option<String> {
    let mut seen = 0usize;
    let mut lines = log_body.lines().peekable();
    while let Some(line) = lines.next() {
        if !is_echo_of(line, &mut lines, cmd) {
            continue;
        }
        if seen < occurrence {
            seen += 1;
            continue;
        }
        let mut block: Vec<&str> = Vec::new();
        for out in lines.by_ref() {
            if out == "." || out.starts_with(". ") {
                break;
            }
            block.push(out.trim_end_matches('\r'));
        }
        // One leading and one trailing blank line are Stata's inter-command
        // spacing and belong to the LOG, not to the command's output. Any
        // further blank lines are the command's own (correlate ends its
        // table with one) and must survive: this cut is byte-exact.
        if block.first() == Some(&"") {
            block.remove(0);
        }
        if block.last() == Some(&"") {
            block.pop();
        }
        let mut text = block.join("\n");
        text.push('\n');
        return Some(text);
    }
    None
}

/// Is `line` the echo of `cmd`, consuming `> ` continuation lines when the
/// echo wrapped? Matches the whole command, never a prefix: `. summarize`
/// must not match `. summarize price`.
fn is_echo_of<'a, I>(line: &str, lines: &mut std::iter::Peekable<I>, cmd: &str) -> bool
where
    I: Iterator<Item = &'a str>,
{
    let Some(echoed) = line.trim_end().strip_prefix(". ") else {
        return false;
    };
    let mut full = echoed.to_owned();
    while full != cmd {
        // The echo of a long line wraps onto `> ` continuations.
        let Some(next) = lines.peek() else { break };
        let Some(cont) = next.trim_end().strip_prefix("> ") else {
            break;
        };
        full.push_str(cont);
        lines.next();
    }
    full == cmd
}

/// One section of a `return list` / `ereturn list` block, parsed.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Listing {
    /// `(plain name, printed value)` — `e(N) =  74` parses to `("N", "74")`.
    /// The value is kept as the **string** Stata printed; it is parsed to f64
    /// only at compare time, same rule as every capture value.
    pub scalars: Vec<(String, String)>,
    /// `(plain name, text)` — `e(cmd) : "regress"` parses to `("cmd", "regress")`.
    pub macros: Vec<(String, String)>,
    /// `(plain name, rows, cols)`.
    pub matrices: Vec<(String, usize, usize)>,
    /// Plain names — `e(sample)` parses to `"sample"`.
    pub functions: Vec<String>,
}

/// Parse the output block of a `return list` / `ereturn list` echo (as
/// returned by [`command_output`]) into its sections.
///
/// # Errors
///
/// Any line that fits no section is an error, not a skip: a silently dropped
/// result is exactly the failure this harness exists to catch.
pub fn parse_listing(block: &str) -> Result<Listing, String> {
    let mut out = Listing::default();
    let mut section = "";
    for raw in block.lines() {
        let t = raw.trim();
        if t.is_empty() {
            continue;
        }
        if let Some(s) = t.strip_suffix(':') {
            if matches!(s, "scalars" | "macros" | "matrices" | "functions") {
                section = match s {
                    "scalars" => "s",
                    "macros" => "m",
                    "matrices" => "x",
                    _ => "f",
                };
                continue;
            }
        }
        match section {
            "s" => {
                let (name, value) = t
                    .split_once('=')
                    .ok_or_else(|| format!("scalar line without `=`: {t:?}"))?;
                out.scalars
                    .push((plain_name(name.trim())?, value.trim().to_owned()));
            }
            "m" => {
                let (name, value) = t
                    .split_once(':')
                    .ok_or_else(|| format!("macro line without `:`: {t:?}"))?;
                let v = value.trim();
                let v = v
                    .strip_prefix('"')
                    .and_then(|v| v.strip_suffix('"'))
                    .unwrap_or(v);
                out.macros.push((plain_name(name.trim())?, v.to_owned()));
            }
            "x" => {
                let (name, shape) = t
                    .split_once(':')
                    .ok_or_else(|| format!("matrix line without `:`: {t:?}"))?;
                let (r, c) = shape
                    .split_once('x')
                    .ok_or_else(|| format!("matrix shape without `x`: {t:?}"))?;
                out.matrices.push((
                    plain_name(name.trim())?,
                    r.trim().parse().map_err(|e| format!("rows: {e}"))?,
                    c.trim().parse().map_err(|e| format!("cols: {e}"))?,
                ));
            }
            "f" => out.functions.push(plain_name(t)?),
            _ => return Err(format!("value outside any section: {t:?}")),
        }
    }
    Ok(out)
}

/// `e(N)` / `r(mean)` → `N` / `mean`, matching `ResultSet`'s plain keys.
fn plain_name(wrapped: &str) -> Result<String, String> {
    let inner = wrapped
        .strip_prefix("e(")
        .or_else(|| wrapped.strip_prefix("r("))
        .or_else(|| wrapped.strip_prefix("s("))
        .and_then(|s| s.strip_suffix(')'))
        .ok_or_else(|| format!("not an e()/r()/s() name: {wrapped:?}"))?;
    Ok(inner.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FAKE: &str = "banner line one
Stata license: [redacted]
Serial number: [redacted]
. do case.do

. summarize mpg

    Variable |        Obs
-------------+-----------
         mpg |         74

. return list

scalars:
                  r(N) =  74
               r(mean) =  21.2972972972973

. bad command
command bad is unrecognized
r(199);

end of do-file
r(199);
";

    #[test]
    fn banner_is_skipped_even_when_redacted() {
        let b = body(FAKE);
        assert!(b.starts_with('\n'), "body starts after the `. do` echo");
        assert!(!b.contains("[redacted]"));
    }

    /// The live banner, as StataMP 18.5 prints it (invented licensee). The
    /// continuation under `Licensed to:` is the institution, indented to the
    /// value column — the line the committed goldens show as `[redacted]`.
    const LIVE_BANNER: &str = "\
Stata license: Single-user 4-core , expiring 22 Aug 2026
Serial number: 501809
  Licensed to: Ada Example
               Example Institute of Technology
Notes:
      1. Stata is running in batch mode.
";

    #[test]
    fn licence_lines_redact_to_the_corpus_convention() {
        let got = redact_licence(LIVE_BANNER);
        // Byte-for-byte the shape tests/golden/stata18/*.log commit: labels
        // kept, values gone, the institution line reduced to its indent.
        assert_eq!(
            got,
            "\
Stata license: [redacted]
Serial number: [redacted]
  Licensed to: [redacted]
               [redacted]
Notes:
      1. Stata is running in batch mode.
"
        );
        for pii in ["501809", "Ada Example", "Example Institute", "expiring"] {
            assert!(!got.contains(pii), "{pii:?} survived redaction: {got}");
        }
    }

    #[test]
    fn redaction_is_idempotent_and_leaves_the_body_alone() {
        let once = redact_licence(LIVE_BANNER);
        assert_eq!(redact_licence(&once), once, "the goldens are a fixed point");
        // Everything after the banner — echoes, tables, `r(NNN);` — is
        // untouched, CRLF and a missing final newline included.
        let body_only = body(FAKE).replace('\n', "\r\n");
        let body_only = body_only.trim_end();
        assert_eq!(redact_licence(body_only), body_only);
        // An indented line NOT under `Licensed to:` is never a continuation.
        assert_eq!(
            redact_licence("Notes:\n      1. kept\n"),
            "Notes:\n      1. kept\n"
        );
    }

    #[test]
    fn the_expired_licence_log_keeps_its_diagnosis_and_loses_its_name() {
        // What an expired licence actually writes: no logo, no `. do` echo,
        // just the licence block and the verdict. The verdict must survive
        // — it is the whole point of the SKIP reason — and nothing else may.
        let log = "Stata license: Single-user 4-core , expiring 22 Aug 2026\n\
                   Serial number: 501809\n  Licensed to: Ada Example\n\
                   \x20              Example Institute of Technology\n\
                   Your license has expired\n";
        let got = redact_licence(log);
        assert!(
            got.ends_with("               [redacted]\nYour license has expired\n"),
            "{got}"
        );
        assert!(!got.contains("Ada") && !got.contains("Institute") && !got.contains("501809"));
    }

    #[test]
    fn rc_is_parsed_from_the_log_never_from_an_exit_code() {
        assert_eq!(final_rc(FAKE), 199);
        assert_eq!(final_rc("clean run\nend of do-file\n"), 0);
        // Anchored: junk around the marker is not a marker.
        assert_eq!(final_rc("xr(1);\nr(2); trailing\nr(x);\n"), 0);
        assert_eq!(final_rc("r(111);\n"), 111);
    }

    #[test]
    fn command_output_cuts_the_block_between_echoes() {
        let b = body(FAKE);
        let block = command_output(b, "summarize mpg", 0).expect("block");
        assert_eq!(
            block,
            "    Variable |        Obs\n-------------+-----------\n         mpg |         74\n"
        );
        // Whole-line match only: a prefix is not the command.
        assert!(command_output(b, "summarize", 0).is_none());
        // A second occurrence that does not exist is None, not the first again.
        assert!(command_output(b, "summarize mpg", 1).is_none());
    }

    #[test]
    fn listing_parses_scalars_by_plain_name() {
        let b = body(FAKE);
        let block = command_output(b, "return list", 0).expect("listing block");
        let l = parse_listing(&block).expect("parse");
        assert_eq!(l.scalars.len(), 2);
        assert_eq!(l.scalars[0], ("N".to_owned(), "74".to_owned()));
        assert_eq!(l.scalars[1].0, "mean");
        assert!(l.macros.is_empty() && l.matrices.is_empty() && l.functions.is_empty());
    }

    #[test]
    fn wrapped_echo_lines_are_reassembled() {
        let log = ". do x.do\n. regress price mpg weight foreign headroom trunk \
                   length turn\n>  displacement\n\noutput line\n\n. next\n";
        let b = body(log);
        let cmd = "regress price mpg weight foreign headroom trunk \
                   length turn displacement";
        let block = command_output(b, cmd, 0).expect("wrapped echo found");
        assert_eq!(block, "output line\n");
    }
}
