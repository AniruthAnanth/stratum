//! `stratum check <PATH>...` — deterministic checks only (design 08 §4.1, §16).
//!
//! Two things come out of a check, and they are deliberately different objects.
//!
//! * **Diagnostics** — the scanner's own findings plus `stratum_parse::lints`'
//!   `L001`–`L007`, which are real, deterministic, run at keystroke latency in
//!   the editor's wasm build, and are the same code the problems pane shows.
//!   That is what makes a code seen in CI, in `--json`, and in a `*! nolint(...)`
//!   suppression the same string (ARCHITECTURE C14).
//! * **A [`ReproReport`]** — §16's checklist. Its honesty rule is written into
//!   `stratum-proto`'s own header: `runs_clean` is `Tri::Unknown` until an
//!   *actual* `Isolation::Subprocess` clean run verifies it, and "a green mark
//!   that was inferred from static analysis is the single worst thing this
//!   feature could ship."
//!
//! **What this build does NOT claim.** The `R001`–`R026` reproducibility checks
//! are `stratum-intel`'s (work unit W20), which has not landed. `inputs_resolved`
//! and `no_hidden_deps` are therefore `Tri::Unknown` — not `Yes`, and not `No`.
//! `seed_defined` *is* answered, because "does this file set a seed" is exactly
//! what the field asks and is decidable from the token stream alone.

use std::io::Write;

use camino::Utf8Path;
use stratum_proto::diagnostic::{Diagnostic, Severity};
use stratum_proto::ids::{DocumentId, Span};
use stratum_proto::repro::{ReproReport, Tri};

use crate::cli::{CheckArgs, DenyLevel, ExitCode, Format};
use crate::cmd::{read_to_string, CmdError};
use crate::output::human;

/// Everything one file's audit produced.
#[derive(Clone, Debug, serde::Serialize)]
pub struct FileAudit {
    /// The file, as given.
    pub file: camino::Utf8PathBuf,
    /// §16's checklist.
    pub report: ReproReport,
    /// Scanner findings plus `L001`–`L007`.
    pub diagnostics: Vec<Diagnostic>,
}

/// Audit one buffer. Pure: no filesystem, no clock beyond
/// `ReproReport::generated_at_ms`.
#[must_use]
pub fn audit(file: &Utf8Path, src: &str) -> FileAudit {
    let seg = stratum_parse::segment(src);
    let mut diagnostics: Vec<Diagnostic> = seg
        .diags
        .iter()
        .cloned()
        .map(|mut d| {
            d.file = Some(file.to_owned());
            d
        })
        .collect();

    let mut seed_defined = Tri::No;
    for (i, line) in seg.lines.iter().enumerate() {
        if line.is_trivia {
            continue;
        }
        let derived = seg.derived[i].as_deref();
        let code = line.code(seg.src, derived);
        if code.trim().is_empty() {
            continue;
        }
        // Speculative: the editor's mode. Macro values are unknown at check
        // time, so a `` `x' `` in the command position downgrades rather than
        // being guessed at.
        let (ast, parse_diags) =
            stratum_parse::parse_command(code, stratum_parse::ParseMode::Speculative);
        if is_set_seed(code) {
            seed_defined = Tri::Yes;
        }
        let cx = stratum_parse::LintCtx {
            text: code,
            vars: None,
        };
        for mut d in parse_diags
            .into_iter()
            .chain(stratum_parse::lint(&ast, &cx))
        {
            // Lint spans are in the LINE's coordinates; the file wants source
            // coordinates. `to_source` is what puts a `///`-spliced line back.
            d.span = d.span.map(|s| Span {
                start: line.to_source(derived, s.start),
                end: line.to_source(derived, s.end),
            });
            d.file = Some(file.to_owned());
            diagnostics.push(d);
        }
    }

    // Stable order: severity, then position, then code. The problems pane is
    // diffed, and CI output that reorders itself between runs is unreadable.
    diagnostics.sort_by(|a, b| {
        a.severity
            .cmp(&b.severity)
            .then(span_key(a).cmp(&span_key(b)))
            .then(a.code.cmp(&b.code))
    });

    FileAudit {
        file: file.to_owned(),
        report: ReproReport {
            doc: DocumentId(1),
            file_hash: stratum_parse::text_hash(src),
            generated_at_ms: crate::cmd::now_ms(),
            // NEVER inferred from static analysis. See this module's header.
            runs_clean: Tri::Unknown,
            verified_by: None,
            verified_duration_us: None,
            seed_defined,
            // `stratum-intel` (W20) owns R003/R005; claiming either way here
            // would be exactly the green-mark-by-inference the contract forbids.
            inputs_resolved: Tri::Unknown,
            no_hidden_deps: Tri::Unknown,
            findings: Vec::new(),
            suppressed: Vec::new(),
        },
        diagnostics,
    }
}

fn span_key(d: &Diagnostic) -> (u32, u32) {
    d.span.map_or((u32::MAX, u32::MAX), |s| (s.start, s.end))
}

/// `set seed <n>` — decidable from the head of a logical line without an engine.
fn is_set_seed(code: &str) -> bool {
    let mut words = code.split_whitespace();
    words
        .next()
        .is_some_and(|w| "set".starts_with(w) && !w.is_empty())
        && words.next() == Some("seed")
}

/// The severity at or above which a finding fails the run.
#[must_use]
pub fn fails_at(deny: DenyLevel) -> Option<Severity> {
    match deny {
        DenyLevel::Note => Some(Severity::Note),
        DenyLevel::Warning => Some(Severity::Warning),
        DenyLevel::Error => Some(Severity::Error),
        DenyLevel::Never => None,
    }
}

/// `stratum check`.
///
/// # Errors
/// [`CmdError::Io`].
pub fn check(
    args: &CheckArgs,
    out: &mut impl Write,
    err: &mut impl Write,
) -> Result<ExitCode, CmdError> {
    let format = if args.json { Format::Json } else { args.format };
    // `Severity` is declared Error, Warning, Note, Help, so `Ord` puts the most
    // severe FIRST. "At or above the deny level" is therefore `<=`.
    let threshold = fails_at(args.deny);
    let mut failed = false;

    for path in &args.paths {
        let src = read_to_string(path)?;
        let mut a = audit(path, &src);
        if args.warn_as_error {
            for d in &mut a.diagnostics {
                if d.severity == Severity::Warning {
                    d.severity = Severity::Error;
                }
            }
        }
        if let Some(t) = threshold {
            failed |= a.diagnostics.iter().any(|d| d.severity <= t);
        }
        match format {
            Format::Quiet => {}
            Format::Json => {
                let line =
                    serde_json::to_string(&a).map_err(|e| CmdError::Internal(e.to_string()))?;
                writeln!(out, "{line}").map_err(|source| CmdError::Io {
                    path: path.clone(),
                    source,
                })?;
            }
            Format::Text => {
                for d in &a.diagnostics {
                    human::diagnostic(err, d).map_err(|source| CmdError::Io {
                        path: path.clone(),
                        source,
                    })?;
                }
                writeln!(
                    out,
                    "{path}: {} finding(s); runs-clean {:?}, seed {:?}, generated {}",
                    a.diagnostics.len(),
                    a.report.runs_clean,
                    a.report.seed_defined,
                    // The one place in the workspace that renders a `UnixMs` for
                    // a person (A2). A repro report a human reads has to say
                    // when it was taken, or "runs-clean Unknown" is undatable.
                    human::format_unix_ms(a.report.generated_at_ms)
                )
                .map_err(|source| CmdError::Io {
                    path: path.clone(),
                    source,
                })?;
            }
        }
    }
    Ok(if failed {
        ExitCode::CheckFailed
    } else {
        ExitCode::Success
    })
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;
    use crate::cli::{Cli, Command};

    fn parse(argv: &[&str]) -> CheckArgs {
        match Cli::try_parse_from(argv).expect("argv parses").command {
            Command::Check(a) => a,
            other => panic!("expected `check`, got {other:?}"),
        }
    }

    fn go(src: &str, extra: &[&str]) -> (ExitCode, String, String) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.do");
        std::fs::write(&p, src).unwrap();
        let mut argv = vec!["stratum", "check", p.to_str().unwrap()];
        argv.extend_from_slice(extra);
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = check(&parse(&argv), &mut out, &mut err).expect("readable file");
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    #[test]
    fn a_clean_file_is_exit_zero() {
        let (code, _, _) = go("sysuse auto, clear\nsummarize price\n", &[]);
        assert_eq!(code, ExitCode::Success);
    }

    /// `L005` — an absolute path in `using`/`use`. It is a warning, so the
    /// default deny level (error) does not fail the run, and `--deny warning`
    /// does. That difference IS the flag.
    #[test]
    fn the_deny_level_decides_whether_a_warning_fails_the_run() {
        let src = "use \"/Users/ana/data/raw.dta\", clear\n";
        let (default, _, _) = go(src, &[]);
        let (strict, _, err) = go(src, &["--deny", "warning"]);
        assert_eq!(default, ExitCode::Success);
        assert_eq!(strict, ExitCode::CheckFailed);
        assert!(err.contains("L005"), "{err}");
    }

    #[test]
    fn warn_as_error_promotes_and_then_fails() {
        let (code, _, _) = go(
            "use \"/Users/ana/data/raw.dta\", clear\n",
            &["--warn-as-error"],
        );
        assert_eq!(code, ExitCode::CheckFailed);
    }

    /// The honesty rule from `stratum-proto`'s own header, as a test.
    #[test]
    fn runs_clean_is_never_inferred_from_static_analysis() {
        let a = audit(Utf8Path::new("a.do"), "sysuse auto, clear\n");
        assert_eq!(a.report.runs_clean, Tri::Unknown);
        assert_eq!(a.report.verified_by, None);
        assert_eq!(a.report.inputs_resolved, Tri::Unknown);
        assert_eq!(a.report.no_hidden_deps, Tri::Unknown);
    }

    #[test]
    fn seed_defined_is_answered_because_it_is_decidable() {
        assert_eq!(
            audit(Utf8Path::new("a.do"), "set seed 12345\nbootstrap\n")
                .report
                .seed_defined,
            Tri::Yes
        );
        assert_eq!(
            audit(Utf8Path::new("a.do"), "bootstrap\n")
                .report
                .seed_defined,
            Tri::No
        );
    }

    /// CI output that reorders itself between runs is unreadable, and the
    /// problems pane is diffed.
    #[test]
    fn findings_come_back_in_a_stable_order() {
        let src = "use \"/a/b.dta\", clear\nuse \"/c/d.dta\", clear\n";
        let a = audit(Utf8Path::new("a.do"), src);
        let b = audit(Utf8Path::new("a.do"), src);
        assert_eq!(
            a.diagnostics
                .iter()
                .map(|d| (&d.code, d.span))
                .collect::<Vec<_>>(),
            b.diagnostics
                .iter()
                .map(|d| (&d.code, d.span))
                .collect::<Vec<_>>()
        );
        let spans: Vec<_> = a.diagnostics.iter().filter_map(|d| d.span).collect();
        assert!(
            spans.windows(2).all(|w| w[0].start <= w[1].start),
            "{spans:?}"
        );
    }

    /// Spans are reported in SOURCE coordinates, not in the logical line's, so
    /// an editor can jump to them.
    #[test]
    fn spans_are_in_source_coordinates() {
        let src = "sysuse auto, clear\nuse \"/tmp/x.dta\", clear\n";
        let a = audit(Utf8Path::new("a.do"), src);
        let d = a
            .diagnostics
            .iter()
            .find(|d| d.code == "L005")
            .expect("L005 fires on an absolute path");
        let span = d.span.expect("a span");
        assert!(
            span.start >= 19,
            "the finding is on the second line, at byte {} of {}",
            span.start,
            src.len()
        );
        assert!(src.get(span.start as usize..span.end as usize).is_some());
    }

    #[test]
    fn json_is_one_audit_per_file() {
        let (_, out, _) = go("sysuse auto, clear\n", &["--json"]);
        let v: serde_json::Value = serde_json::from_str(out.trim_end()).unwrap();
        assert_eq!(v["report"]["runs_clean"], "unknown");
        assert!(v["diagnostics"].is_array());
    }

    #[test]
    fn deny_never_reports_without_failing() {
        let (code, _, _) = go(
            "use \"/Users/ana/data/raw.dta\", clear\n",
            &["--deny", "never"],
        );
        assert_eq!(code, ExitCode::Success);
    }

    #[test]
    fn severity_ordering_is_the_direction_the_deny_check_assumes() {
        assert!(Severity::Error < Severity::Warning);
        assert!(Severity::Warning < Severity::Note);
        assert_eq!(fails_at(DenyLevel::Never), None);
        assert_eq!(fails_at(DenyLevel::Error), Some(Severity::Error));
    }
}
