//! The chatter — everything that goes to **stderr**.
//!
//! CONTRACTS §7 guarantee 4 and design 08 §4.3 draw one line: stdout carries the
//! stream (NDJSON under `--json`, a classic log under `--format text`) and
//! stderr carries everything a person reads *about* the run. That is what makes
//! `stratum run x.do --json | jq` work, and it is why every function here takes
//! its writer as an argument — there is no `eprintln!` in this crate outside the
//! panic hook, so there is no way to write to stderr without going past this
//! module.

use std::io::Write;

use stratum_proto::diagnostic::{Diagnostic, Severity};

use crate::cli::ExitCode;
use crate::output::Tally;

/// Design 08 §4.3's summary line:
///
/// ```text
/// stratum: 14 blocks, 12 succeeded, 1 failed (r(111)), 1 skipped in 2.431s
/// ```
///
/// The duration is **recorded, not asserted** (ADR-017): it is what the engine
/// reported in `RunFinished.duration_us`, printed for a human, and nothing in
/// the test suite gates on it.
///
/// # Errors
/// A write error on stderr.
pub fn summary(w: &mut impl Write, tally: &Tally, exit: ExitCode) -> std::io::Result<()> {
    let succeeded = tally.blocks_run.saturating_sub(tally.blocks_failed);
    write!(
        w,
        "stratum: {} blocks, {succeeded} succeeded",
        tally.blocks_run
    )?;
    if tally.blocks_failed > 0 {
        write!(
            w,
            ", {} failed (r({}))",
            tally.blocks_failed, tally.outcome.rc
        )?;
    }
    if tally.blocks_skipped > 0 {
        write!(w, ", {} skipped", tally.blocks_skipped)?;
    }
    let secs = tally.duration_us as f64 / 1_000_000.0;
    write!(w, " in {secs:.3}s")?;
    if exit != ExitCode::Success {
        write!(w, " — exit {}", exit.code())?;
    }
    writeln!(w)
}

/// One diagnostic, in the shape `rustc` taught everyone to read.
///
/// The Stata return code is printed as `r(111)` because that is the string a
/// Stata user searches for, and `Diagnostic.stata_rc` is what makes it
/// available without scraping English prose (design 08 §3.3).
///
/// # Errors
/// A write error on stderr.
pub fn diagnostic(w: &mut impl Write, d: &Diagnostic) -> std::io::Result<()> {
    let level = match d.severity {
        Severity::Error => "error",
        Severity::Warning => "warning",
        Severity::Note => "note",
        Severity::Help => "help",
    };
    write!(w, "{level}[{}]", d.code)?;
    if let Some(rc) = d.stata_rc {
        write!(w, " r({rc})")?;
    }
    writeln!(w, ": {}", d.message)?;
    if let Some(file) = &d.file {
        match d.span {
            Some(s) => writeln!(w, "  --> {file}:@{}..{}", s.start, s.end)?,
            None => writeln!(w, "  --> {file}")?,
        }
    }
    for related in &d.related {
        match &related.file {
            Some(f) => writeln!(w, "  note: {} ({f})", related.message)?,
            None => writeln!(w, "  note: {}", related.message)?,
        }
    }
    for note in &d.notes {
        writeln!(w, "  = {note}")?;
    }
    for s in &d.suggestions {
        writeln!(w, "  help: {}", s.label)?;
    }
    Ok(())
}

/// Render a `UnixMs` for a person. **The only place in the workspace that turns
/// a wire timestamp into text** (A2: `time` is an L3-only dependency, and the
/// wire carries `u64` milliseconds because everything below L3 builds for
/// `wasm32-unknown-unknown`).
///
/// Falls back to the raw millisecond count if the value is out of range, rather
/// than panicking: a corrupt timestamp in a capture must not take down a report
/// about that capture.
#[must_use]
pub fn format_unix_ms(ms: u64) -> String {
    let Ok(secs) = i64::try_from(ms / 1_000) else {
        return format!("{ms}ms");
    };
    let Ok(t) = time::OffsetDateTime::from_unix_timestamp(secs) else {
        return format!("{ms}ms");
    };
    let fmt = time::macros::format_description!("[year]-[month]-[day]T[hour]:[minute]:[second]Z");
    t.format(&fmt).unwrap_or_else(|_| format!("{ms}ms"))
}

#[cfg(test)]
mod tests {
    use stratum_proto::diagnostic::Confidence;

    use super::*;
    use crate::cli::RunOutcome;

    fn line(tally: &Tally, exit: ExitCode) -> String {
        let mut buf = Vec::new();
        summary(&mut buf, tally, exit).unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn the_summary_matches_design_08_section_4_3() {
        let tally = Tally {
            blocks_run: 14,
            blocks_failed: 1,
            blocks_skipped: 1,
            duration_us: 2_431_000,
            outcome: RunOutcome {
                rc: 111,
                had_real_error: true,
                ..RunOutcome::default()
            },
            ..Tally::default()
        };
        assert_eq!(
            line(&tally, ExitCode::RuntimeError),
            "stratum: 14 blocks, 13 succeeded, 1 failed (r(111)), 1 skipped in 2.431s — exit 1\n"
        );
    }

    #[test]
    fn a_clean_run_says_nothing_about_failures_or_exit_codes() {
        let tally = Tally {
            blocks_run: 3,
            duration_us: 1_000,
            ..Tally::default()
        };
        assert_eq!(
            line(&tally, ExitCode::Success),
            "stratum: 3 blocks, 3 succeeded in 0.001s\n"
        );
    }

    #[test]
    fn a_diagnostic_prints_its_stata_return_code() {
        let d = Diagnostic {
            severity: Severity::Error,
            code: "STATA0111".to_owned(),
            stata_rc: Some(111),
            message: "variable income not found".to_owned(),
            file: Some(camino::Utf8PathBuf::from("analysis.do")),
            span: Some(stratum_proto::ids::Span { start: 4, end: 10 }),
            offending_token: Some("income".to_owned()),
            block: None,
            related: Vec::new(),
            suggestions: Vec::new(),
            notes: vec!["did you mean `incom`?".to_owned()],
            confidence: Confidence::Exact,
        };
        let mut buf = Vec::new();
        diagnostic(&mut buf, &d).unwrap();
        let text = String::from_utf8(buf).unwrap();
        assert!(text.starts_with("error[STATA0111] r(111): variable income not found\n"));
        assert!(text.contains("--> analysis.do:@4..10"));
        assert!(text.contains("= did you mean `incom`?"));
    }

    #[test]
    fn a_unix_ms_renders_as_utc_and_never_panics() {
        assert_eq!(format_unix_ms(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_unix_ms(1_755_820_000_123), "2025-08-21T23:46:40Z");
        // Far outside `time`'s supported range: reported, not fatal.
        assert!(format_unix_ms(u64::MAX).ends_with("ms"));
    }
}
