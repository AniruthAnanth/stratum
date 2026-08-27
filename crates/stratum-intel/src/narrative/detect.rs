//! Narrative region detection — design 07 §9.1, spec §24.
//!
//! **Explicit markers only. Zero heuristics.** Three forms, all of them ordinary
//! Stata comments, all portable to Stata 18 unchanged:
//!
//! * **Line form** — a maximal run of consecutive lines matching `^\s*//\|`.
//!   `//|` is just a `//` comment whose text begins with `|`; Stata neither
//!   knows nor cares.
//! * **Block form** — an opener `^\s*/\*md\s*$`, terminated by the matching
//!   `*/`.
//! * **Cell heading** — spec §3's `// %% Title` marker renders as an H2 in
//!   Document View for free.
//!
//! # Why not heuristic detection
//!
//! A rule that rendered "any comment that looks like Markdown" would turn
//! `* fix this later` into a bullet and `* 1. check the merge` into an ordered
//! list, and would mangle the millions of existing do-files that use `*`
//! decoratively (`*** SECTION 2 ***`). Zero false positives is worth two extra
//! keystrokes.
//!
//! # Why this reads the scanner's output and not the raw bytes
//!
//! Detection consults the segmentation, so a `//|` inside a string literal or
//! inside a `/* */` block is not a narrative region — for free, and without a
//! second comment scanner that could disagree with the runtime about where a
//! comment starts.

use core::ops::Range;

use stratum_parse::Segmentation;

use crate::ParseIndex;

/// Which marker opened the region.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NarrativeForm {
    /// A run of `//|` lines.
    Line,
    /// A `/*md … */` block.
    Block,
    /// A `// %%` / `* %%` cell marker, rendered as a heading.
    CellHeading,
}

/// One narrative region.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct NarrativeRegion {
    /// Extent in the source buffer, **including** the markers.
    pub byte_range: Range<usize>,
    /// Which form opened it.
    pub form: NarrativeForm,
    /// The Markdown, markers stripped.
    pub markdown: String,
}

/// Every narrative region in the buffer, in document order.
#[must_use]
pub fn detect(idx: &ParseIndex<'_>) -> Vec<NarrativeRegion> {
    detect_in(idx.source(), idx.segmentation())
}

/// [`detect`] against a segmentation the caller already has.
#[must_use]
pub fn detect_in(src: &str, seg: &Segmentation<'_>) -> Vec<NarrativeRegion> {
    let mut out: Vec<NarrativeRegion> = Vec::new();
    // A run of `//|` lines under construction: (start, end, accumulated text).
    let mut run: Option<(u32, u32, Vec<String>)> = None;

    for line in &seg.lines {
        // Only a line that contributed NO code can be a narrative marker. This
        // is what makes `di "//| not markdown"` inert.
        if !line.is_trivia {
            flush(&mut run, &mut out);
            continue;
        }
        let raw = src
            .get(line.span.start as usize..line.span.end as usize)
            .unwrap_or("");
        let body = raw.trim_start();

        if let Some(rest) = body.strip_prefix("//|") {
            let text = strip_one_space(first_physical_line(rest));
            match &mut run {
                Some((_, end, acc)) => {
                    *end = line.span.end;
                    acc.push(text.to_owned());
                }
                None => run = Some((line.span.start, line.span.end, vec![text.to_owned()])),
            }
            continue;
        }
        flush(&mut run, &mut out);

        if is_md_opener(body) {
            out.push(NarrativeRegion {
                byte_range: line.span.start as usize..line.span.end as usize,
                form: NarrativeForm::Block,
                markdown: block_body(raw),
            });
            continue;
        }
        if line.is_cell_marker {
            if let Some(title) = stratum_parse::scan::marker_title(body) {
                out.push(NarrativeRegion {
                    byte_range: line.span.start as usize..line.span.end as usize,
                    form: NarrativeForm::CellHeading,
                    markdown: format!("## {title}"),
                });
            }
        }
    }
    flush(&mut run, &mut out);
    out
}

fn flush(run: &mut Option<(u32, u32, Vec<String>)>, out: &mut Vec<NarrativeRegion>) {
    if let Some((start, end, acc)) = run.take() {
        out.push(NarrativeRegion {
            byte_range: start as usize..end as usize,
            form: NarrativeForm::Line,
            markdown: acc.join("\n"),
        });
    }
}

/// `^\s*/\*md\s*$` — the opener must be alone on its physical line, so that
/// `/*md-ish note */` mid-line is an ordinary comment.
fn is_md_opener(body: &str) -> bool {
    let Some(rest) = body.strip_prefix("/*md") else {
        return false;
    };
    first_physical_line(rest).trim().is_empty()
}

/// The `/*md … */` body with the opener, the terminator and one leading blank
/// line removed.
fn block_body(raw: &str) -> String {
    let after = raw.trim_start().strip_prefix("/*md").unwrap_or(raw);
    let inner = match after.rfind("*/") {
        Some(i) => after.get(..i).unwrap_or(after),
        None => after,
    };
    inner
        .trim_start_matches([' ', '\t'])
        .trim_start_matches(['\r', '\n'])
        .trim_end()
        .to_owned()
}

/// The `//|` prefix strips **at most one** following space, so an intentionally
/// indented Markdown line (a nested list, a fenced block's body) keeps its
/// indentation.
fn strip_one_space(s: &str) -> &str {
    s.strip_prefix(' ').unwrap_or(s)
}

fn first_physical_line(s: &str) -> &str {
    match s.find(['\n', '\r']) {
        Some(i) => s.get(..i).unwrap_or(s),
        None => s,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;

    fn regions(src: &str) -> Vec<NarrativeRegion> {
        let idx = ParseIndex::new(src);
        detect(&idx)
    }

    #[test]
    fn a_run_of_line_markers_is_one_region() {
        let src = "\
//| ## Model specification
//|
//| We estimate a log-linear wage equation.
regress ln_wage educ exper
";
        let r = regions(src);
        assert_eq!(r.len(), 1, "{r:?}");
        assert_eq!(r[0].form, NarrativeForm::Line);
        assert_eq!(
            r[0].markdown,
            "## Model specification\n\nWe estimate a log-linear wage equation."
        );
    }

    #[test]
    fn a_command_between_two_runs_splits_them() {
        let src = "//| one\ndi 1\n//| two\n";
        let r = regions(src);
        assert_eq!(r.len(), 2, "{r:?}");
        assert_eq!(r[0].markdown, "one");
        assert_eq!(r[1].markdown, "two");
    }

    #[test]
    fn the_block_form_is_recognised_and_stripped() {
        let src = "\
/*md
## Data construction

We merge the 2019 and 2020 waves 1:1 on `pid`.
*/
merge 1:1 pid using wave2020.dta
";
        let r = regions(src);
        assert_eq!(r.len(), 1, "{r:?}");
        assert_eq!(r[0].form, NarrativeForm::Block);
        assert!(
            r[0].markdown.starts_with("## Data construction"),
            "{:?}",
            r[0].markdown
        );
        assert!(
            r[0].markdown.ends_with("1:1 on `pid`."),
            "{:?}",
            r[0].markdown
        );
    }

    #[test]
    fn a_cell_marker_becomes_a_heading() {
        let r = regions("// %% Data loading\nsysuse auto, clear\n");
        assert_eq!(r.len(), 1, "{r:?}");
        assert_eq!(r[0].form, NarrativeForm::CellHeading);
        assert_eq!(r[0].markdown, "## Data loading");
    }

    #[test]
    fn a_marker_inside_a_string_is_not_a_region() {
        assert!(regions("di \"//| not markdown\"\n").is_empty());
        assert!(regions("local x \"/*md\"\n").is_empty());
    }

    #[test]
    fn a_decorative_star_comment_is_never_narrative() {
        // The whole reason detection is explicit: these must stay inert.
        assert!(regions("*** SECTION 2 ***\n").is_empty());
        assert!(regions("* 1. check the merge\n").is_empty());
        assert!(regions("* fix this later\n").is_empty());
    }

    #[test]
    fn an_md_lookalike_opener_is_not_a_block() {
        assert!(regions("/*mdash note */\ndi 1\n").is_empty());
        assert!(regions("/*md-ish note */\ndi 1\n").is_empty());
    }

    #[test]
    fn only_one_space_is_stripped_after_the_marker() {
        let r = regions("//| - a\n//|   - nested\n");
        assert_eq!(r[0].markdown, "- a\n  - nested");
    }

    #[test]
    fn byte_ranges_land_on_the_markers() {
        let src = "di 1\n//| note\ndi 2\n";
        let r = regions(src);
        assert_eq!(r.len(), 1);
        assert_eq!(
            src.get(r[0].byte_range.clone()).unwrap().trim_end(),
            "//| note"
        );
    }
}
