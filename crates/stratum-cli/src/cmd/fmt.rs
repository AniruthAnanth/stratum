//! `stratum fmt <PATH>...` — format do-files.
//!
//! # Why this formatter is deliberately small
//!
//! A `.do` file is the user's source of record (spec §5): Stata must still be
//! able to run it, and `stratum-workspace` exists because "exactly one function
//! in the product can put bytes into such a file, and four callers of it, each
//! holding a proof that its edit is safe" (ADR-010). A formatter is a fifth
//! caller in spirit, so it carries a proof of the same kind.
//!
//! The proof here is **staleness neutrality**. `CodeHash` is blake3-128 over a
//! per-region canonical token stream, *not* over source text, precisely so that
//! comments, reindentation and `///` reflow are provably staleness-neutral
//! (spec §23, CONTRACTS §1.2). So a formatting pass is admissible exactly when
//! it leaves every region's `CodeHash` — and the number of regions — unchanged.
//! [`is_staleness_neutral`] checks that on every file, and a file whose
//! candidate rewrite fails it is **left untouched** rather than written.
//!
//! That check is not decoration, and it is not the only guard, because it turned
//! out not to be sufficient. Stata auto-closes an unterminated string at end of
//! line, so `di "hello   ` has three significant trailing spaces inside a string
//! and `display` prints them. **`CodeHash` cannot see the difference**:
//! `stratum-parse`'s `ScanLine::code` strips a line's trailing whitespace before
//! anything downstream runs, so `di "hello   ` and `di "hello` produce the same
//! canonical tokens and the same 16 hash bytes (measured; see
//! `tests::the_segmenter_cannot_tell_the_two_apart`, and the escalation it
//! carries). A formatter that relied on the hash alone would eat those bytes and
//! the proof would wave it through.
//!
//! So there are two guards, at different levels:
//!
//! * [`trim_trailing`] asks the **lexer** whether a token reaches into the
//!   whitespace it is about to drop, and leaves the line alone if one does. This
//!   is what makes the case above a no-op rather than a refusal — a do-file with
//!   a significant trailing blank is legal Stata, not an error.
//! * [`is_staleness_neutral`] is the write gate: region count, `hash_ordinal`,
//!   `CodeHash` and raw token text, all compared before a byte is written.
//!
//! What the pass does: LF line endings, no trailing whitespace, exactly one
//! terminating newline. What it does not do: reindent, re-wrap, reorder, or
//! touch a single byte inside a line. Those need `stratum-intel`'s equivalence
//! proofs (work unit W20) and are not v1.

use std::io::Write;

use stratum_parse::TokKind;

use crate::cli::{ExitCode, FmtArgs};
use crate::cmd::{read_to_string, CmdError};

/// The conservative rewrite. Pure; never touches the filesystem.
#[must_use]
pub fn format(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    // `split('\n')` after normalising CR keeps a final empty element for a
    // trailing newline, which is what lets the "exactly one" rule below be a
    // truncation rather than a special case.
    let unix = src.replace("\r\n", "\n").replace('\r', "\n");
    for (i, line) in unix.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(trim_trailing(line));
    }
    while out.ends_with('\n') {
        out.pop();
    }
    if !out.is_empty() {
        out.push('\n');
    }
    out
}

/// Trailing spaces and tabs — **unless a token reaches into them**.
///
/// Stata auto-closes an unterminated string at end of line, so `di "hello   `
/// has three significant trailing blanks *inside* a string literal and
/// `display` will print them. The lexer is asked rather than guessed at: if any
/// token's span extends past the trim point, those bytes are program text and
/// the line is returned untouched.
///
/// This has to be decided here, and cannot be left to
/// [`is_staleness_neutral`], because **`ScanLine::code` already drops them**:
/// measured on `stratum-parse` as it stands, the segmenter reports
/// `code == "di \"hello"` for both spellings, so every downstream comparison —
/// canonical tokens, `CodeHash`, the token-text clause below — sees two
/// identical programs. See `tests::the_segmenter_cannot_tell_the_two_apart`,
/// which pins that behaviour so a fix upstream is noticed rather than silently
/// making this guard redundant.
fn trim_trailing(line: &str) -> &str {
    let trimmed = line.trim_end_matches([' ', '\t']);
    if trimmed.len() == line.len() {
        return line;
    }
    let covered = stratum_parse::tokens(line, stratum_parse::LexMode::Speculative)
        .iter()
        .any(|t| t.kind != TokKind::Eof && t.span.end as usize > trimmed.len());
    if covered {
        line
    } else {
        trimmed
    }
}

/// Would rewriting `before` as `after` change what any block *means*?
///
/// Two clauses, and the second one is not redundant.
///
/// 1. **Region count, `CodeHash` and `hash_ordinal`, in order.** A formatter
///    that preserved the hashes but split one region into two would renumber
///    every `hash_ordinal` after it and silently restale the document.
/// 2. **Byte-for-byte token text.** `CodeHash` is blake3-128 over a *canonical*
///    token stream, and canonical means normalised: measured against
///    `stratum-parse` as it stands, `di "hello   ` and `di "hello` hash to the
///    same 16 bytes — `b0 07 22 df 1e f4 d3 01 82 4e 3c 8e 98 d0 99 6e` for
///    both. Stata auto-closes an unterminated string at end of line, so those
///    three spaces are program text that `display` will print, and clause 1
///    alone would happily let the formatter eat them. Comparing the *raw* text
///    of every token catches it. Comment text is excluded, because
///    `ScanLine::code` has already removed it — which is what keeps a trailing
///    space at the end of a `* note` from blocking the whole file.
#[must_use]
pub fn is_staleness_neutral(before: &str, after: &str) -> bool {
    let a = stratum_parse::segment(before);
    let b = stratum_parse::segment(after);
    a.regions.len() == b.regions.len()
        && a.regions
            .iter()
            .zip(&b.regions)
            .all(|(x, y)| x.code_hash == y.code_hash && x.hash_ordinal == y.hash_ordinal)
        && program_text(&a) == program_text(&b)
}

/// Every token of every non-trivia logical line, as `(kind, raw text)`.
///
/// Clause 2 of [`is_staleness_neutral`]. It goes through `stratum-parse`'s own
/// lexer rather than a private one so that "what counts as a token" has a single
/// definition in the product.
fn program_text(seg: &stratum_parse::Segmentation<'_>) -> Vec<(TokKind, String)> {
    let mut out = Vec::new();
    for (i, line) in seg.lines.iter().enumerate() {
        if line.is_trivia {
            continue;
        }
        let derived = seg.derived[i].as_deref();
        let code = line.code(seg.src, derived);
        // Speculative: `fmt` runs over source with macro references still in it,
        // which is the editor's mode and not the execution path's.
        for tok in stratum_parse::tokens(code, stratum_parse::LexMode::Speculative) {
            if tok.kind == TokKind::Eof {
                continue;
            }
            out.push((tok.kind, tok.text(code).to_owned()));
        }
    }
    out
}

/// One file's verdict.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Verdict {
    /// Already formatted.
    Unchanged,
    /// Would change, and the rewrite is staleness-neutral.
    Reformatted,
    /// Would change, but the rewrite would move a `CodeHash`. Left untouched.
    Refused,
}

/// `stratum fmt`.
///
/// # Errors
/// [`CmdError::Io`].
pub fn fmt(
    args: &FmtArgs,
    out: &mut impl Write,
    err: &mut impl Write,
) -> Result<ExitCode, CmdError> {
    let mut changed = false;
    let mut refused = false;

    for path in &args.paths {
        let src = read_to_string(path)?;
        let candidate = format(&src);
        let verdict = if candidate == src {
            Verdict::Unchanged
        } else if is_staleness_neutral(&src, &candidate) {
            Verdict::Reformatted
        } else {
            Verdict::Refused
        };

        match verdict {
            Verdict::Unchanged => {}
            Verdict::Refused => {
                refused = true;
                writeln!(
                    err,
                    "{path}: refused — the rewrite would move a block's CodeHash, \
                     which would restale the document. Left untouched."
                )
                .ok();
            }
            Verdict::Reformatted => changed = true,
        }

        if args.stdout {
            let text = if verdict == Verdict::Refused {
                &src
            } else {
                &candidate
            };
            out.write_all(text.as_bytes())
                .map_err(|source| CmdError::Io {
                    path: path.clone(),
                    source,
                })?;
        } else if verdict == Verdict::Reformatted && !args.check {
            std::fs::write(path, &candidate).map_err(|source| CmdError::Io {
                path: path.clone(),
                source,
            })?;
        }
    }

    if refused {
        // An invariant we refused to break is not a formatting result; it is a
        // file the user must look at.
        return Ok(ExitCode::Internal);
    }
    Ok(if args.check && changed {
        ExitCode::FormatChanged
    } else {
        ExitCode::Success
    })
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;
    use crate::cli::{Cli, Command};

    fn parse(argv: &[&str]) -> FmtArgs {
        match Cli::try_parse_from(argv).expect("argv parses").command {
            Command::Fmt(a) => a,
            other => panic!("expected `fmt`, got {other:?}"),
        }
    }

    fn go(src: &str, extra: &[&str]) -> (ExitCode, String, String, String) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.do");
        std::fs::write(&p, src).unwrap();
        let mut argv = vec!["stratum", "fmt", p.to_str().unwrap()];
        argv.extend_from_slice(extra);
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = fmt(&parse(&argv), &mut out, &mut err).expect("readable");
        (
            code,
            std::fs::read_to_string(&p).unwrap(),
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    #[test]
    fn line_endings_trailing_space_and_the_final_newline() {
        assert_eq!(format("a\r\nb  \n\n\n"), "a\nb\n");
        assert_eq!(format("a"), "a\n");
        assert_eq!(format(""), "");
        assert_eq!(format("\n\n"), "");
        assert_eq!(format("a\n"), "a\n", "already formatted is a fixed point");
    }

    #[test]
    fn formatting_is_idempotent() {
        let src = "sysuse auto, clear   \r\n\r\nsummarize price\t\n\n\n";
        let once = format(src);
        assert_eq!(format(&once), once);
    }

    /// The case the whole guard exists for. Stata auto-closes an unterminated
    /// string at end of line, so those three trailing spaces are program text
    /// that `display` prints — and the formatter must not eat them.
    #[test]
    fn trailing_space_inside_an_unterminated_string_is_left_alone() {
        let src = "di \"hello   \n";
        assert_eq!(
            format(src),
            src,
            "the lexer says a token covers those bytes"
        );
        let (code, on_disk, _, err) = go(src, &[]);
        assert_eq!(on_disk, src, "the file was left untouched");
        // Not an error: a do-file with a significant trailing blank is legal
        // Stata. It is simply a line this formatter has nothing to say about.
        assert_eq!(code, ExitCode::Success);
        assert!(err.is_empty(), "{err}");
        // And `--check` must not claim it would reformat it.
        let (checked, _, _, _) = go(src, &["--check"]);
        assert_eq!(checked, ExitCode::Success);
    }

    /// **An escalation, pinned as a test.** `stratum-parse` (W04) cannot
    /// distinguish `di "hello   ` from `di "hello`: `ScanLine::code` trims a
    /// line's trailing whitespace before canonicalisation, so both spell the
    /// same canonical token stream and hash to the same `CodeHash`. They are
    /// different Stata programs — the first prints three trailing blanks.
    ///
    /// Nothing in this crate can fix that; `crates/stratum-parse/**` is W04's.
    /// What this crate can do is (a) not rely on the hash for this decision,
    /// which [`trim_trailing`] does not, and (b) notice the day it changes. If
    /// this test fails, the segmenter has started distinguishing them: delete
    /// the test and simplify [`fmt`]'s module header.
    #[test]
    fn the_segmenter_cannot_tell_the_two_apart() {
        let loose = stratum_parse::segment("di \"hello   \n");
        let tight = stratum_parse::segment("di \"hello\n");
        assert_eq!(loose.regions.len(), 1);
        assert_eq!(
            loose.regions[0].code_hash, tight.regions[0].code_hash,
            "stratum-parse now distinguishes them — see this test's doc comment"
        );
        assert!(
            is_staleness_neutral("di \"hello   \n", "di \"hello\n"),
            "the CodeHash proof alone would wave the unsafe rewrite through"
        );
    }

    #[test]
    fn an_ordinary_file_is_rewritten_and_stays_staleness_neutral() {
        let src = "sysuse auto, clear   \r\nsummarize price\r\n";
        let (code, on_disk, _, _) = go(src, &[]);
        assert_eq!(code, ExitCode::Success);
        assert_eq!(on_disk, "sysuse auto, clear\nsummarize price\n");
        assert!(is_staleness_neutral(src, &on_disk));
    }

    /// Exit 6.
    #[test]
    fn check_reports_without_writing() {
        let src = "sysuse auto, clear   \n";
        let (code, on_disk, _, _) = go(src, &["--check"]);
        assert_eq!(code, ExitCode::FormatChanged);
        assert_eq!(on_disk, src, "--check never writes");
    }

    #[test]
    fn check_on_a_formatted_file_is_exit_zero() {
        let (code, _, _, _) = go("sysuse auto, clear\n", &["--check"]);
        assert_eq!(code, ExitCode::Success);
    }

    #[test]
    fn stdout_mode_writes_the_candidate_and_leaves_the_file_alone() {
        let src = "sysuse auto, clear  \n";
        let (_, on_disk, out, _) = go(src, &["--stdout"]);
        assert_eq!(on_disk, src);
        assert_eq!(out, "sysuse auto, clear\n");
    }

    /// A rewrite that split one region into two would renumber every
    /// `hash_ordinal` after it and silently restale the document, even with
    /// every hash still present.
    #[test]
    fn splitting_a_region_is_not_staleness_neutral() {
        let before = "foreach v of varlist a b {\n  summarize `v'\n}\n";
        let after = "summarize a\nsummarize b\n";
        assert!(!is_staleness_neutral(before, after));
    }
}
