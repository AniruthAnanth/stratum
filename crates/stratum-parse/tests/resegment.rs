//! Incremental re-segmentation — design 02 §5.4 property 4 and §5.5, as amended
//! by A25.
//!
//! Two obligations, and they pull against each other:
//!
//! 1. **Equality.** `resegment(prev, new, edit) == segment(new)` for EVERY
//!    edit, including the ones where convergence legitimately cannot happen: an
//!    edit that opens a `/*`, flips `#delimit`, or starts a `program` block
//!    changes the meaning of everything below it and the whole tail must be
//!    rescanned. The proptest below generates exactly those.
//! 2. **Convergence.** For an ordinary edit the number of regions RE-HASHED must
//!    be small — A25 fixes the gate at ≤ 8 for a ≤ 3-block edit at 5 % into a
//!    2 MB file. That is asserted here with the instrumentation counter, not
//!    with wall time; `benches/segment.rs` carries the timing.
//!
//! `resegment` CONSUMES its previous segmentation — that is what lets a
//! keystroke reuse the allocation instead of copying a 2 MB document (A25).
//! These tests need `prev` afterwards to compare against, so they clone it
//! explicitly; the clone is scaffolding and is outside every measurement.

use proptest::prelude::*;
use stratum_parse::{resegment, resegment_with_stats, segment, SourceEdit};
use stratum_proto::Span;

/// Apply an edit the way an editor would, so the test and the implementation
/// cannot disagree about what `SourceEdit` means.
fn apply(src: &str, at: usize, take: usize, ins: &str) -> (String, SourceEdit) {
    let mut out = String::with_capacity(src.len() + ins.len());
    out.push_str(&src[..at]);
    out.push_str(ins);
    out.push_str(&src[at + take..]);
    (
        out,
        SourceEdit {
            range: Span {
                start: at as u32,
                end: (at + take) as u32,
            },
            new_len: ins.len() as u32,
        },
    )
}

fn check(src: &str, at: usize, take: usize, ins: &str) {
    let (new, edit) = apply(src, at, take, ins);
    let prev = segment(src);
    let inc = resegment(prev.clone(), &new, edit);
    let full = segment(&new);
    assert_eq!(
        inc.regions,
        full.regions,
        "regions differ after replacing {at}..{} with {ins:?} in:\n{src}",
        at + take
    );
    assert_eq!(inc.lines, full.lines, "logical lines differ");
    assert_eq!(inc.markers, full.markers, "markers differ");
    assert_eq!(inc.sections, full.sections, "sections differ");
    assert_eq!(inc.diags, full.diags, "diagnostics differ");
    assert_eq!(inc.line_index, full.line_index, "line index differs");
    assert_eq!(inc.end_delimiter, full.end_delimiter);
    assert_eq!(inc, full);
}

const DOC: &str = "\
use auto, clear
* describe the data
describe
summarize price mpg
foreach v of varlist mpg price {
    summarize `v'
}
regress price mpg weight
di \"done\"
";

#[test]
fn edit_inside_one_region() {
    let at = DOC.find("mpg weight").unwrap();
    check(DOC, at, 3, "turn");
}

#[test]
fn edit_that_opens_a_block_comment() {
    let at = DOC.find("describe\n").unwrap();
    check(DOC, at, 0, "/* ");
}

#[test]
fn edit_that_flips_the_delimiter() {
    let at = DOC.find("describe\n").unwrap();
    check(DOC, at, 0, "#delimit ;\n");
}

#[test]
fn edit_that_starts_a_program_block() {
    let at = DOC.find("describe\n").unwrap();
    check(DOC, at, 0, "program define p\n");
}

#[test]
fn edit_that_deletes_a_closing_brace() {
    let at = DOC.find("}\n").unwrap();
    check(DOC, at, 2, "");
}

#[test]
fn edit_at_the_very_start_and_very_end() {
    check(DOC, 0, 0, "di 1\n");
    check(DOC, DOC.len(), 0, "di 2\n");
    check(DOC, 0, DOC.len(), "di 3\n");
    check(DOC, 0, DOC.len(), "");
}

#[test]
fn edit_that_adds_and_removes_a_cell_marker() {
    let at = DOC.find("summarize price").unwrap();
    check(DOC, at, 0, "// %% Section two\n");
    let marked = format!("// %% One\n{DOC}");
    let prev = segment(&marked);
    let (new, edit) = apply(&marked, 0, "// %% One\n".len(), "");
    assert_eq!(resegment(prev.clone(), &new, edit), segment(&new));
}

#[test]
fn a_duplicated_region_keeps_its_ordinals_across_an_edit() {
    // The case that makes `hash_ordinal` repair necessary: the rescan removes an
    // occurrence of a hash that also occurs after the edit.
    let src = "di 1\nsummarize price\ndi 2\nsummarize price\n";
    let at = src.find("summarize price").unwrap();
    check(src, at, "summarize price".len(), "di 9");
    check(src, at, 0, "summarize price\n");
}

#[test]
fn a_malformed_edit_descriptor_falls_back_to_a_full_pass() {
    let prev = segment(DOC);
    let lie = SourceEdit {
        range: Span { start: 0, end: 4 },
        new_len: 999,
    };
    assert_eq!(resegment(prev.clone(), DOC, lie), segment(DOC));
}

// ---------------------------------------------------------------------------
// The A25 convergence gate
// ---------------------------------------------------------------------------

/// Build a do-file of at least `bytes` bytes out of realistic Stata.
fn big_doc(bytes: usize) -> String {
    const UNIT: &str = "\
* block {N}: describe and model
use panel{N}.dta, clear
gen ln_y{N} = log(y{N})
foreach v of varlist x1 x2 x3 {
    summarize `v', detail
}
regress ln_y{N} x1 x2 x3 ///
    if year > 2000, robust
predict yhat{N}, xb
";
    let mut out = String::with_capacity(bytes + UNIT.len());
    let mut n = 0usize;
    while out.len() < bytes {
        out.push_str(&UNIT.replace("{N}", &n.to_string()));
        n += 1;
    }
    out
}

#[test]
fn a_three_block_edit_five_percent_in_rehashes_at_most_eight_regions() {
    let src = big_doc(2 * 1024 * 1024);
    let prev = segment(&src);
    assert!(prev.src_len as usize >= 2 * 1024 * 1024);

    // 5 % into the file, snapped to a line start so the edit is one a user could
    // actually make.
    // Three statements inserted at a line boundary 5 % into the file — the edit
    // A25 names. Snapped to a line start so it is one a user could actually make.
    let target = src.len() / 20;
    let at = src[target..].find('\n').unwrap() + target + 1;
    let (new, edit) = apply(&src, at, 0, "di 1\ndi 2\ndi 3\n");

    let (inc, stats) = resegment_with_stats(prev.clone(), &new, edit);
    assert_eq!(inc, segment(&new), "incremental result diverged");
    assert!(stats.converged, "the rescan never re-converged");
    assert!(
        stats.rescanned <= 8,
        "re-hashed {} regions, the A25 gate is 8",
        stats.rescanned
    );
    assert!(
        stats.bytes_scanned < 4096,
        "scanned {} bytes for a 3-block edit at 5 % into 2 MB",
        stats.bytes_scanned
    );
    assert!(stats.reused_prefix > 100, "the prefix was not reused");
    assert!(stats.reused_suffix > 1000, "the suffix was not reused");
}

#[test]
fn a_single_character_edit_rehashes_one_region() {
    let src = big_doc(256 * 1024);
    let prev = segment(&src);
    let at = src.find("robust").unwrap();
    let (new, edit) = apply(&src, at, 6, "vce(r)");
    let (inc, stats) = resegment_with_stats(prev.clone(), &new, edit);
    assert_eq!(inc, segment(&new));
    assert!(stats.converged);
    assert!(stats.rescanned <= 2, "re-hashed {}", stats.rescanned);
}

/// A region whose head is not a command word at all — a bare `end`, a `}`, a
/// `#delimit` line — carries a `HeadInfo` with nothing resolved, and the fields
/// that are "absent" there are absent by SENTINEL rather than by `Option`, to
/// keep `Region` sixteen bytes smaller (see `HeadInfo`). A sentinel that the
/// keystroke path rebases along with the real coordinates stops being a
/// sentinel, and the region then compares unequal to the same region segmented
/// from scratch — property 4 gone, silently, only for documents containing one
/// of these heads. The 10 000-case proptest below found exactly that; this
/// keeps the case named after it was fixed.
#[test]
fn a_head_with_no_command_word_survives_a_resegment() {
    let src = "summarize price\n// %% Section\nend\n#delimit cr\ndi 1\nsummarize price\n";
    let prev = segment(src);
    // Delete one byte inside the first region, which moves everything after it.
    let (new, edit) = apply(src, 1, 1, "");
    assert_eq!(resegment(prev, &new, edit), segment(&new));
}

#[test]
fn an_edit_that_opens_a_comment_cannot_converge_and_says_so() {
    let src = big_doc(64 * 1024);
    let prev = segment(&src);
    let at = src.len() / 20;
    let at = src[at..].find('\n').unwrap() + at + 1;
    let (new, edit) = apply(&src, at, 0, "/* ");
    let (inc, stats) = resegment_with_stats(prev.clone(), &new, edit);
    assert_eq!(inc, segment(&new));
    assert!(
        !stats.converged,
        "an unterminated /* swallows the file; there is nothing to converge to"
    );
}

// ---------------------------------------------------------------------------
// Property 4 and property 7, over generated Stata
// ---------------------------------------------------------------------------

/// Fragments chosen so the generator produces the cases where convergence
/// legitimately fails, not only the easy ones.
fn fragment() -> impl Strategy<Value = &'static str> {
    prop::sample::select(vec![
        "di 1\n",
        "summarize price\n",
        "* a comment\n",
        "\n",
        "// %% Section\n",
        "foreach v of varlist a b {\n",
        "}\n",
        "else {\n",
        "#delimit ;\n",
        "#delimit cr\n",
        "di 2 ;\n",
        "/* open\n",
        "*/ di 3\n",
        "program define p\n",
        "end\n",
        "input a b\n",
        "mata:\n",
        "local t 1 ///\n",
        "   2\n",
        "di \"a // b\"\n",
        "di `\"compound\"'\n",
        "di \"unterminated\n",
        "//////////\n",
        "regress y x\n",
        "   ",
        "{",
        "}",
        ";",
        "\"",
        "`",
    ])
}

fn document() -> impl Strategy<Value = String> {
    prop::collection::vec(fragment(), 0..24).prop_map(|v| v.concat())
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 512, ..ProptestConfig::default() })]

    /// Properties 1 (tiling), 3 (purity) and 5 (line/byte agreement) over
    /// generated documents.
    #[test]
    fn tiling_purity_and_lines(src in document()) {
        let a = segment(&src);
        prop_assert_eq!(&a, &segment(&src));
        let mut at = 0u32;
        for r in &a.regions {
            prop_assert_eq!(r.outer_span.start, at);
            at = r.outer_span.end;
            prop_assert_eq!(a.line_index.line_of(r.span.start), r.code_lines.start);
            prop_assert_eq!(a.line_index.line_of(r.outer_span.start), r.lines.start);
            if r.span.end > r.span.start {
                prop_assert_eq!(a.line_index.line_of(r.span.end - 1) + 1, r.code_lines.end);
            }
            prop_assert_eq!(a.line_index.line_of(r.outer_span.end - 1) + 1, r.lines.end);
        }
        prop_assert_eq!(at as usize, src.len());
    }

    /// Property 2 — self-containment, over generated documents.
    #[test]
    fn self_containment(src in document()) {
        let seg = segment(&src);
        for r in &seg.regions {
            if !r.is_executable() {
                continue;
            }
            let frag = &src[r.span.start as usize..r.span.end as usize];
            let opts = stratum_parse::SegmentOptions {
                initial_delimiter: r.entry_delimiter,
                ..Default::default()
            };
            let sub = stratum_parse::segment_with(frag, &opts);
            let live: Vec<_> = sub.regions.iter().filter(|x| x.is_executable()
                || matches!(x.kind, stratum_parse::RegionShape::Unterminated { .. })).collect();
            prop_assert_eq!(live.len(), 1, "{:?} -> {:?}", frag, sub.regions.iter().map(|x| x.kind).collect::<Vec<_>>());
            prop_assert_eq!(&live[0].kind, &r.kind, "{:?}", frag);
        }
    }

    /// Property 7 — no panics, on arbitrary bytes rather than plausible Stata.
    /// `fuzz/fuzz_targets/fuzz_segment.rs` is the same property under cargo-fuzz;
    /// this runs in CI, where cargo-fuzz does not.
    #[test]
    fn arbitrary_text_never_panics(src in ".{0,400}") {
        let seg = segment(&src);
        let mut at = 0u32;
        for r in &seg.regions {
            prop_assert_eq!(r.outer_span.start, at);
            at = r.outer_span.end;
            let _ = &src[r.span.start as usize..r.span.end as usize];
            let _ = &src[r.outer_span.start as usize..r.outer_span.end as usize];
        }
        prop_assert_eq!(at as usize, src.len());
    }
}

// Property 4 — incrementality, over 10 000 random edits as A25 requires.
//
// The generator deliberately emits the cases where convergence CANNOT happen —
// an edit that opens `/*`, flips `#delimit`, or starts a `program` block —
// because those are exactly the ones a convergence optimisation gets wrong, and
// the ones a naive "rescan to end of file" gets right by accident.
proptest! {
    #![proptest_config(ProptestConfig { cases: 10_000, ..ProptestConfig::default() })]

    #[test]
    fn resegment_equals_segment(
        src in document(),
        ins in document(),
        cut in 0usize..1000,
        take in 0usize..40,
    ) {
        let at = char_boundary(&src, cut % (src.len() + 1));
        let take = char_boundary(&src, (at + take).min(src.len())) - at;
        let (new, edit) = apply(&src, at, take, &ins);
        let prev = segment(&src);
        prop_assert_eq!(resegment(prev.clone(), &new, edit), segment(&new));
    }
}

fn char_boundary(s: &str, mut i: usize) -> usize {
    i = i.min(s.len());
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}
