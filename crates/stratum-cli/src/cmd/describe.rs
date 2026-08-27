//! `stratum describe <PATH>...` — the structural description of a `.do`.
//!
//! Design 08 §4.1: "On `.do`: the logical-region decomposition from §2 … This is
//! exactly what the editor gutter needs, and exporting it from the CLI means the
//! editor and CI agree on block boundaries by construction."
//!
//! **"By construction" is load-bearing and is the whole design of this file.**
//! The gutter renders `BlockMap.regions`, which is `Vec<RegionSummary>`, which
//! is produced by `stratum_parse::Segmentation::summaries()`. This command calls
//! that function — not a reimplementation of it, not a projection of it, not a
//! second walk over `Segmentation::regions` that happens to agree today. There
//! is one line of code between `segment` and the JSON, and
//! `tests::describe_is_literally_the_gutters_own_function` asserts equality
//! against a fresh call to it.
//!
//! # Why the output is not a §7.1 envelope
//!
//! `describe` answers no engine request: it is pure computation over a file, it
//! allocates no `BlockId` (CONTRACTS §2 reserves that to `stratum-exec`), and it
//! opens no session. Wrapping it in `{v,t,corr,body}` would imply a
//! correspondence with the protocol that does not exist. `--json` therefore
//! emits one plain JSON document per file, one per line — still NDJSON, still
//! `| jq`-able, still stdout-only.

use std::io::Write;

use camino::{Utf8Path, Utf8PathBuf};
use serde::Serialize;
use stratum_proto::block::{CellMarker, Delimiter, RegionKind, RegionSummary, SectionSpan};
use stratum_proto::diagnostic::Diagnostic;
use stratum_proto::ids::TextHash;

use crate::cli::{DescribeArgs, ExitCode, Format};
use crate::cmd::{read_to_string, CmdError};

/// What `describe` knows about a `.do`.
///
/// Every field is a `stratum-proto` wire type, so the JSON here and the JSON the
/// engine sends the editor are the same bytes for the same file.
#[derive(Clone, PartialEq, Debug, Serialize)]
pub struct DoDescription {
    /// The file, as given.
    pub file: Utf8PathBuf,
    /// blake3-128 over the raw bytes INCLUDING comments (CONTRACTS §1.1). This
    /// is "did the file change on disk?", never staleness.
    pub text_hash: TextHash,
    /// Delimiter mode at end of source. A fragment segmented without it is
    /// silently mis-parsed (design 02 §13.2).
    pub end_delimiter: Delimiter,
    /// Executable regions plus trivia, in document order, tiling the file.
    pub regions: Vec<RegionSummary>,
    /// `// %%` / `* %%` cell markers (spec §3).
    pub markers: Vec<CellMarker>,
    /// One section per marker.
    pub sections: Vec<SectionSpan>,
    /// Scanner diagnostics.
    pub diagnostics: Vec<Diagnostic>,
}

/// Segment a `.do` and project it exactly as the editor gutter sees it.
///
/// The one line that matters is `seg.summaries()`.
#[must_use]
pub fn describe_do(file: &Utf8Path, src: &str) -> DoDescription {
    let seg = stratum_parse::segment(src);
    DoDescription {
        file: file.to_owned(),
        text_hash: stratum_parse::text_hash(src),
        end_delimiter: seg.end_delimiter,
        regions: seg.summaries(),
        markers: seg.markers.clone(),
        sections: seg.sections.clone(),
        diagnostics: seg.diags.clone(),
    }
}

/// `stratum describe`.
///
/// # Errors
/// [`CmdError::Io`] for an unreadable file, [`CmdError::Unsupported`] for a
/// `.dta` (the reader is `stratum-dta`, work unit W03, which is not linked here).
pub fn describe(
    args: &DescribeArgs,
    out: &mut impl Write,
    _err: &mut impl Write,
) -> Result<ExitCode, CmdError> {
    let format = if args.json { Format::Json } else { args.format };
    for path in &args.paths {
        if path.extension() == Some("dta") {
            return Err(CmdError::Unsupported(format!(
                "`describe {path}`: the .dta reader (crates/stratum-dta, work unit W03) \
                 is not linked into this build; `describe` handles .do files today"
            )));
        }
        let src = read_to_string(path)?;
        let d = describe_do(path, &src);
        match format {
            Format::Quiet => {}
            Format::Json => {
                let line =
                    serde_json::to_string(&d).map_err(|e| CmdError::Internal(e.to_string()))?;
                writeln!(out, "{line}").map_err(io(path))?;
            }
            Format::Text => write_text(out, &d).map_err(io(path))?,
        }
    }
    Ok(ExitCode::Success)
}

fn io(path: &Utf8Path) -> impl Fn(std::io::Error) -> CmdError + '_ {
    move |source| CmdError::Io {
        path: path.to_owned(),
        source,
    }
}

fn write_text(out: &mut impl Write, d: &DoDescription) -> std::io::Result<()> {
    writeln!(out, "{}", d.file)?;
    writeln!(
        out,
        "  {} regions, {} markers, delimiter {} at EOF",
        d.regions.len(),
        d.markers.len(),
        match d.end_delimiter {
            Delimiter::Cr => "cr",
            Delimiter::Semi => ";",
        }
    )?;
    writeln!(out, "  {:>4}  {:>9}  {:<22}  command", "#", "lines", "kind")?;
    for r in &d.regions {
        writeln!(
            out,
            "  {:>4}  {:>4}-{:<4}  {:<22}  {}",
            r.index,
            r.code_lines.start + 1,
            r.code_lines.end,
            kind_name(&r.kind),
            r.canonical.as_deref().unwrap_or("")
        )?;
    }
    Ok(())
}

/// One name per `RegionKind`. Exhaustive on purpose: a new kind in proto must be
/// given a name here rather than silently rendering as something else.
fn kind_name(k: &RegionKind) -> String {
    match k {
        RegionKind::Simple => "simple".to_owned(),
        RegionKind::Brace { opener } => format!("brace:{opener:?}").to_lowercase(),
        RegionKind::EndBlock { opener, name } => match name {
            Some(n) => format!("{opener:?}:{n}").to_lowercase(),
            None => format!("{opener:?}").to_lowercase(),
        },
        RegionKind::Directive { directive } => format!("directive:{directive:?}").to_lowercase(),
        RegionKind::Trivia { has_marker } => {
            if *has_marker {
                "trivia:marker".to_owned()
            } else {
                "trivia".to_owned()
            }
        }
        RegionKind::Unterminated { expected } => {
            format!("unterminated:{expected:?}").to_lowercase()
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;
    use crate::cli::{Cli, Command};

    const SRC: &str = "\
// %% Load
sysuse auto, clear

* the model
regress price mpg weight

foreach v of varlist mpg weight {
    summarize `v'
}
";

    fn parse(argv: &[&str]) -> DescribeArgs {
        match Cli::try_parse_from(argv).expect("argv parses").command {
            Command::Describe(a) => a,
            other => panic!("expected `describe`, got {other:?}"),
        }
    }

    fn go(argv: &[&str]) -> (Result<ExitCode, CmdError>, String) {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let r = describe(&parse(argv), &mut out, &mut err);
        (r, String::from_utf8(out).unwrap())
    }

    /// **The acceptance bullet.** `describe` emits the same region decomposition
    /// the editor gutter uses, *from the same function* — so the editor and CI
    /// agree on block boundaries by construction rather than by coincidence.
    #[test]
    fn describe_is_literally_the_gutters_own_function() {
        let got = describe_do(Utf8Path::new("a.do"), SRC);
        let gutter = stratum_parse::segment(SRC).summaries();
        assert_eq!(
            got.regions, gutter,
            "describe must call Segmentation::summaries(), not reimplement it"
        );
    }

    /// CONTRACTS §2: "Consecutive `outer_span`s TILE THE FILE EXACTLY." The
    /// gutter relies on it to answer "which region is the cursor in" at every
    /// byte, so exporting a decomposition that did not tile would be worse than
    /// exporting none.
    #[test]
    fn the_exported_regions_tile_the_file() {
        let d = describe_do(Utf8Path::new("a.do"), SRC);
        let mut at = 0u32;
        for r in &d.regions {
            assert_eq!(r.outer_span.start, at, "a gap or an overlap at byte {at}");
            at = r.outer_span.end;
        }
        assert_eq!(at as usize, SRC.len(), "the last region reaches EOF");
    }

    #[test]
    fn json_is_one_document_per_file_and_nothing_else_on_stdout() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.do");
        std::fs::write(&p, SRC).unwrap();
        let (r, out) = go(&["stratum", "describe", p.to_str().unwrap(), "--json"]);
        assert_eq!(r.unwrap(), ExitCode::Success);
        assert_eq!(out.lines().count(), 1);
        let v: serde_json::Value = serde_json::from_str(out.trim_end()).unwrap();
        assert!(v["regions"].as_array().unwrap().len() >= 4);
        assert_eq!(v["markers"].as_array().unwrap().len(), 1, "one `// %%`");
        assert!(v["text_hash"].is_array(), "blake3-128 as 16 bytes");
        // No BlockId is minted: CONTRACTS §2 reserves that to stratum-exec.
        assert!(!out.contains("\"blocks\""));
    }

    #[test]
    fn text_mode_names_every_region_kind_it_meets() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.do");
        std::fs::write(&p, SRC).unwrap();
        let (r, out) = go(&["stratum", "describe", p.to_str().unwrap()]);
        assert_eq!(r.unwrap(), ExitCode::Success);
        assert!(out.contains("simple"));
        assert!(out.contains("brace:foreach"), "{out}");
        assert!(out.contains("regress"));
    }

    #[test]
    fn a_dta_is_reported_as_unsupported_rather_than_guessed_at() {
        let (r, _) = go(&["stratum", "describe", "auto.dta", "--json"]);
        let e = r.expect_err("the .dta reader has not landed");
        assert_eq!(e.exit_code(), ExitCode::Unsupported);
    }

    #[test]
    fn a_missing_file_is_an_io_error() {
        let (r, _) = go(&["stratum", "describe", "/nonexistent/x.do"]);
        assert_eq!(r.expect_err("missing").exit_code(), ExitCode::Io);
    }
}
