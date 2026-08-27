//! Golden segmentation cases and the tiling / self-containment properties of
//! design 02 §5.4.
//!
//! Each `tests/segment/*.do` has a sidecar `*.expect` in the format of 02 §5.4:
//!
//! ```text
//! 1..1    Simple             cr        canonical=use
//! 3..7    Brace/Foreach      cr
//! 9..13   EndBlock/Program   cr        name=myprog
//! 15..15  Directive/DelimitSemi cr->semi
//! ```
//!
//! Lines are 1-based and INCLUSIVE, over `Region::lines` (the outer span, so
//! attached leading comments are visible in the golden). `Trivia` regions are
//! listed too — a golden that hides them cannot show that a comment attached to
//! the command below it rather than standing alone, which is exactly the rule
//! most likely to regress.
//!
//! Set `STRATUM_BLESS=1` to rewrite the sidecars. Every rewrite must be read
//! line by line before it is committed: these files are the record of what the
//! scanner claims Stata does, and blessing them blind turns them into a record
//! of what the scanner happens to do.

use std::fmt::Write as _;

use stratum_parse::scan::region::{segment, segment_with, Region, RegionShape, SegmentOptions};
use stratum_proto::{Delimiter, RegionKind};

fn render(seg: &stratum_parse::Segmentation<'_>) -> String {
    let mut out = String::new();
    for r in &seg.regions {
        // The GOLDEN is rendered from the wire projection, not from `Region`'s
        // internal shape: `EndBlock`'s name lives only there (see `RegionShape`),
        // and the wire kind is what the UI and `stratum-exec` actually see.
        let wire = r.wire_kind(seg.src, &seg.lines, &seg.derived);
        let kind = kind_name(&wire);
        let delim = if r.entry_delimiter == r.exit_delimiter {
            delim_name(r.entry_delimiter).to_owned()
        } else {
            format!(
                "{}->{}",
                delim_name(r.entry_delimiter),
                delim_name(r.exit_delimiter)
            )
        };
        write!(
            out,
            "{}..{}\t{:<22}\t{:<9}",
            r.lines.start + 1,
            r.lines.end,
            kind,
            delim
        )
        .unwrap();
        if let RegionKind::EndBlock { name: Some(n), .. } = &wire {
            write!(out, "\tname={n}").unwrap();
        }
        if let Some(c) = r.head.canonical() {
            write!(out, "\tcanonical={c}").unwrap();
        }
        if r.head.is_estimation() {
            out.push_str("\test");
        }
        if r.head.has_macro_in_head() {
            out.push_str("\tmacro");
        }
        if let RegionKind::Trivia { has_marker: true } = &wire {
            out.push_str("\tmarker");
        }
        out.push('\n');
    }
    out
}

fn kind_name(k: &RegionKind) -> String {
    match k {
        RegionKind::Simple => "Simple".into(),
        RegionKind::Brace { opener } => format!("Brace/{opener:?}"),
        RegionKind::EndBlock { opener, .. } => format!("EndBlock/{opener:?}"),
        RegionKind::Directive { directive } => format!("Directive/{directive:?}"),
        RegionKind::Trivia { .. } => "Trivia".into(),
        RegionKind::Unterminated { expected } => format!("Unterminated/{expected:?}"),
    }
}

fn delim_name(d: Delimiter) -> &'static str {
    match d {
        Delimiter::Cr => "cr",
        Delimiter::Semi => "semi",
    }
}

fn cases() -> Vec<(String, String)> {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/segment");
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).expect("tests/segment") {
        let p = e.unwrap().path();
        if p.extension().and_then(|s| s.to_str()) != Some("do") {
            continue;
        }
        let src = std::fs::read_to_string(&p).unwrap();
        out.push((p.to_string_lossy().into_owned(), src));
    }
    out.sort();
    out
}

#[test]
fn golden_segmentation() {
    let bless = std::env::var("STRATUM_BLESS").is_ok();
    let mut failures = Vec::new();
    for (path, src) in cases() {
        let seg = segment(&src);
        let got = render(&seg);
        let expect_path = path.replace(".do", ".expect");
        if bless {
            std::fs::write(&expect_path, &got).unwrap();
            continue;
        }
        let want = std::fs::read_to_string(&expect_path)
            .unwrap_or_else(|_| panic!("missing {expect_path}; run with STRATUM_BLESS=1"));
        if want != got {
            failures.push(format!("--- {path}\nwant:\n{want}\ngot:\n{got}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// 02 §5.4 property 1. Concatenating `src[r.outer_span]` reproduces the source
/// byte for byte, which is a stronger statement than "the spans are sorted and
/// do not overlap" and is the one the editor's gutter actually depends on.
#[test]
fn tiling() {
    for (path, src) in cases() {
        assert_tiles(&src, &path);
    }
}

fn assert_tiles(src: &str, what: &str) {
    let seg = segment(src);
    let mut at = 0u32;
    let mut joined = String::new();
    for r in &seg.regions {
        assert_eq!(r.outer_span.start, at, "gap or overlap in {what}");
        assert!(
            r.outer_span.end > r.outer_span.start,
            "empty region in {what}"
        );
        joined.push_str(&src[r.outer_span.start as usize..r.outer_span.end as usize]);
        at = r.outer_span.end;
    }
    assert_eq!(at as usize, src.len(), "regions stop short in {what}");
    assert_eq!(joined, src, "concatenation differs in {what}");
}

/// 02 §5.4 property 2 — the property that makes `Cmd+Enter` correct.
#[test]
fn self_containment() {
    for (path, src) in cases() {
        let seg = segment(&src);
        for r in &seg.regions {
            if !r.is_executable() {
                continue;
            }
            assert_self_contained(&src, r, &path);
        }
    }
}

fn assert_self_contained(src: &str, r: &Region, what: &str) {
    let frag = &src[r.span.start as usize..r.span.end as usize];
    let opts = SegmentOptions {
        initial_delimiter: r.entry_delimiter,
        ..SegmentOptions::default()
    };
    let sub = segment_with(frag, &opts);
    let live: Vec<&Region> = sub
        .regions
        .iter()
        .filter(|x| !matches!(x.kind, RegionShape::Trivia { .. }))
        .collect();
    assert_eq!(
        live.len(),
        1,
        "{what}: region {} ({:?}) re-segments to {} regions:\n{frag}\n{:#?}",
        r.index,
        r.kind,
        live.len(),
        sub.regions.iter().map(|x| &x.kind).collect::<Vec<_>>()
    );
    assert_eq!(
        live[0].kind, r.kind,
        "{what}: region {} changed kind when run alone:\n{frag}",
        r.index
    );
}

/// 02 §5.4 property 3.
#[test]
fn purity() {
    for (_, src) in cases() {
        assert_eq!(segment(&src), segment(&src));
    }
}

/// 02 §5.4 property 5.
#[test]
fn line_byte_agreement() {
    for (path, src) in cases() {
        let seg = segment(&src);
        for r in &seg.regions {
            assert_eq!(
                seg.line_index.line_of(r.span.start),
                r.code_lines.start,
                "{path}: region {} code_lines disagrees with line_of(span.start)",
                r.index
            );
            assert_eq!(
                seg.line_index.line_of(r.outer_span.start),
                r.lines.start,
                "{path}: region {} lines disagrees with line_of(outer_span.start)",
                r.index
            );
        }
    }
}

// ---------------------------------------------------------------------------
// The verified traps of 02 §2.1, asserted on the COMMENT-STRIPPED code text.
// The golden files above show how lines group; these show what the scanner
// decided the code actually is, which is what the expander and the parser see.
// ---------------------------------------------------------------------------

fn code_of(src: &str) -> Vec<String> {
    let seg = segment(src);
    seg.lines
        .iter()
        .zip(&seg.derived)
        .filter(|(l, _)| !l.is_trivia)
        .map(|(l, d)| l.code(seg.src, d.as_deref()).to_string())
        .collect()
}

#[test]
fn comments_inside_strings_are_not_comments() {
    // [V] `di "a // b"` prints `a // b`. `:` is not whitespace, which is why the
    // manual's `https://` case needs no special handling anywhere.
    assert_eq!(code_of("di \"a // b\"\n"), ["di \"a // b\""]);
    assert_eq!(
        code_of("di \"https://example.com\"\n"),
        ["di \"https://example.com\""]
    );
}

#[test]
fn slash_star_splices_with_nothing() {
    // [V] `local u ab/*⏎*/cd` yields "abcd" — no separator is inserted.
    assert_eq!(code_of("local u ab/*\n*/cd\n"), ["local u abcd"]);
    // An inline comment leaves the spaces that surrounded it, and nothing else.
    assert_eq!(code_of("di \"x\" /* c */ \"y\"\n"), ["di \"x\"  \"y\""]);
}

#[test]
fn block_comments_nest() {
    // [V] the inner `/*` opens a second level, so the FIRST `*/` does not close
    // the comment. A depth flag instead of a counter resumes code at ` still`.
    assert_eq!(
        code_of("di \"x\" /* a /* b */ still comment */ \"tail\"\n"),
        ["di \"x\"  \"tail\""]
    );
}

#[test]
fn triple_slash_splices_with_no_separator() {
    // [V] `local t 1 ///⏎   2` is "1" + the space before `///` + the three
    // leading spaces of the next line + "2" — NOT "1 2" and not "12".
    assert_eq!(code_of("local t 1 ///\n   2\n"), ["local t 1    2"]);
}

#[test]
fn star_comment_with_continuation_swallows_the_next_line() {
    // [V] `* comment ///` continues the COMMENT onto the next line.
    let src = "* note ///\ndi \"swallowed\"\ndi 3\n";
    assert_eq!(code_of(src), ["di 3"]);
}

#[test]
fn separator_line_joins_the_next_line() {
    // [V] three or more slashes is a continuation, so a `//////` rule line joins
    // with what follows. Design 02 §11 gives this lint L003 (W04b).
    let src = "di 1\n//////////\ndi 2\n";
    let seg = segment(src);
    let live: Vec<&stratum_parse::LogicalLine> =
        seg.lines.iter().filter(|l| !l.is_trivia).collect();
    assert_eq!(live.len(), 2);
    assert_eq!(live[1].code(src, None), "di 2");
    assert_eq!(live[1].first_line, 1, "the joined line starts at the rule");
    assert_eq!(live[1].last_line, 2);
}

#[test]
fn unterminated_string_closes_at_end_of_line() {
    // [V] no error, and the next line is a separate command.
    assert_eq!(
        code_of("di \"no closing quote\ndi \"closed\"\n"),
        ["di \"no closing quote", "di \"closed\""]
    );
}

#[test]
fn double_slash_at_column_zero_is_a_comment() {
    assert_eq!(code_of("//comment\ndi 1\n"), ["di 1"]);
    assert_eq!(code_of("di 1//not a comment\n"), ["di 1//not a comment"]);
}

#[test]
fn brace_counting_is_quote_aware() {
    // [V] `forvalues i=1/1 { ⏎ di "{" ⏎ }` runs in Stata, which it could not if
    // the brace counter saw the brace inside the string.
    let seg = segment("forvalues i = 1/1 {\n    di \"{\"\n}\ndi 1\n");
    assert_eq!(seg.regions.len(), 2);
    assert!(matches!(
        seg.regions[0].kind,
        RegionShape::Brace {
            opener: stratum_proto::BraceOpener::Forvalues
        }
    ));
}

#[test]
fn semi_mode_star_comment_runs_to_the_semicolon() {
    // [V] in `;` mode a `*` comment ends at the `;`, not at the newline.
    let src = "#delimit ;\n* a comment\n  spanning lines ;\ndi 1 ;\n";
    assert_eq!(code_of(src), ["#delimit ;", "di 1"]);
}

#[test]
fn delimiter_mode_is_carried_on_every_region() {
    let src = "di 1\n#delimit ;\ndi 2 ;\n#delimit cr\ndi 3\n";
    let seg = segment(src);
    let modes: Vec<(Delimiter, Delimiter)> = seg
        .regions
        .iter()
        .map(|r| (r.entry_delimiter, r.exit_delimiter))
        .collect();
    assert_eq!(
        modes,
        [
            (Delimiter::Cr, Delimiter::Cr),
            (Delimiter::Cr, Delimiter::Semi),
            (Delimiter::Semi, Delimiter::Semi),
            (Delimiter::Semi, Delimiter::Cr),
            (Delimiter::Cr, Delimiter::Cr),
        ]
    );
    assert_eq!(seg.end_delimiter, Delimiter::Cr);
}

#[test]
fn empty_source_segments_to_nothing() {
    let seg = segment("");
    assert!(seg.regions.is_empty());
    assert_eq!(seg.src_len, 0);
    assert_eq!(seg.line_index.line_count(), 1);
}
