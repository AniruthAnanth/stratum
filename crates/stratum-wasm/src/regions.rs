//! Projection of `stratum-parse`'s segmentation onto CONTRACTS §14's flat rows.
//!
//! Nothing here decides where a block starts. Every structural question —
//! grouping, delimiters, hashes, sections — is already answered by the value
//! this module is handed; what it does is lay those answers out in the exact
//! `i32`/`u64` order `apps/desktop/src/wasm/types.ts` reads back.
//!
//! # Two rules this file exists to keep
//!
//! * **No `BlockId` is minted here.** `hash_lo`/`hash_hi`/`hash_ordinal` are the
//!   region's `CodeHash` and its occurrence index, nothing more. Identity is the
//!   engine's, and it arrives in a `BlockMap`.
//! * **No allocation per region.** The projection runs on every keystroke over
//!   every region in the document, so it is a loop of `extend_from_slice`s over
//!   `Copy` data. In particular the `EndBlock` program name is never
//!   materialised: `encode_kind` ignores it (proven by lib.rs's
//!   `the_endblock_name_is_not_in_the_flat_row`), so the shape is encoded
//!   directly instead of going through `Region::wire_kind`, which allocates a
//!   `String` per `program` block.

use stratum_parse::scan::marker_title;
use stratum_parse::{Region, RegionShape, Segmentation as ParseSegmentation};
use stratum_proto::{
    BraceOpener, CellMarker, DirectiveKind, EndBlockOpener, RegionKind, SectionSpan, Span,
    Unterminated,
};

use crate::{
    encode_delimiter, Delimiter, RegionRow, Segmentation, FAMILY_BRACE, FAMILY_DIRECTIVE,
    FAMILY_END_BLOCK, FAMILY_SIMPLE, FAMILY_TRIVIA, FAMILY_UNTERMINATED, FLAG_ESTIMATION,
    FLAG_EXECUTABLE, FLAG_EXIT_SEMI, FLAG_MACRO_IN_HEAD, FLAG_SECTION_HEAD, NARRATIVE_BLOCK,
    NARRATIVE_LINE, NARRATIVE_STRIDE, REGION_HASH_STRIDE, REGION_STRIDE, SECTION_STRIDE,
};

/// Fill `out` from a completed segmentation.
pub fn project(seg: &ParseSegmentation<'_>, out: &mut Segmentation) {
    push_regions(seg, out);
    push_sections(seg, out);
    push_narrative(seg, out);
    for d in &seg.diags {
        out.push_diagnostic(d.clone());
    }
}

fn push_regions(seg: &ParseSegmentation<'_>, out: &mut Segmentation) {
    // Markers are in document order and regions tile the document, so "which
    // region does this marker open" is a single forward walk rather than a
    // binary search per marker.
    let mut marker = 0usize;
    for r in &seg.regions {
        while marker < seg.markers.len() && seg.markers[marker].span.start < r.outer_span.end {
            marker += 1;
        }
        let opens_section = marker > 0
            && seg.markers[marker - 1].span.start >= r.outer_span.start
            && seg.markers[marker - 1].span.start < r.outer_span.end;
        out.push(&row(r, opens_section));
    }
}

/// One region as the flat row `regions_view` hands to the editor.
fn row(r: &Region, opens_section: bool) -> RegionRow {
    let mut flags = 0;
    if r.is_executable() {
        flags |= FLAG_EXECUTABLE;
    }
    if r.head.is_estimation() {
        flags |= FLAG_ESTIMATION;
    }
    if r.head.has_macro_in_head() {
        flags |= FLAG_MACRO_IN_HEAD;
    }
    if r.exit_delimiter == Delimiter::Semi {
        flags |= FLAG_EXIT_SEMI;
    }
    if opens_section {
        flags |= FLAG_SECTION_HEAD;
    }

    // `CodeHash` is `[u8; 16]`; §14's `RegionRow` fixes the split as
    // `hi = be(h[0..8])`, `lo = be(h[8..16])` so that the webview's
    // `format!("{hi:016x}{lo:016x}")` is the hash's canonical hex and matches
    // `RegionSummary::code_hash` in the `BlockMap`. Any other split silently
    // breaks result anchoring across a reconcile, which is why it is spelled out
    // in both files.
    let h = r.code_hash.0;
    let hash_hi = u64::from_be_bytes([h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]]);
    let hash_lo = u64::from_be_bytes([h[8], h[9], h[10], h[11], h[12], h[13], h[14], h[15]]);

    RegionRow {
        span: r.span.start..r.span.end,
        outer: r.outer_span.start..r.outer_span.end,
        kind: shape_code(r.kind),
        entry_delim: encode_delimiter(r.entry_delimiter),
        head_line: r.code_lines.start as i32,
        // `code_lines` is half-open; the row carries the last line inclusive,
        // because the gutter draws a bracket from `head_line` to `last_line`.
        // An empty range (`Trivia` with no code) collapses onto `head_line`
        // rather than going one line negative.
        last_line: r.code_lines.end.max(r.code_lines.start + 1) as i32 - 1,
        flags,
        hash_lo,
        hash_hi,
        hash_ordinal: u64::from(r.hash_ordinal),
    }
}

/// `RegionShape` → the wire `RegionKind`, without the `EndBlock` name.
///
/// The name is deliberately dropped: the flat row has no field for it, the
/// webview slices one out of the document when it wants one, and recovering it
/// here would allocate a `String` per `program` block on the keystroke path.
///
/// Only [`shape_code`]'s equivalence test calls this now — see there.
#[cfg(test)]
const fn shape_kind(shape: RegionShape) -> RegionKind {
    match shape {
        RegionShape::Simple => RegionKind::Simple,
        RegionShape::Brace { opener } => RegionKind::Brace { opener },
        RegionShape::EndBlock { opener } => RegionKind::EndBlock { opener, name: None },
        RegionShape::Directive { directive } => RegionKind::Directive { directive },
        RegionShape::Trivia { has_marker } => RegionKind::Trivia { has_marker },
        RegionShape::Unterminated { expected } => RegionKind::Unterminated { expected },
    }
}

/// [`crate::encode_kind`] of a shape, without building the `RegionKind`.
///
/// **This is a measured shortcut, not a second encoding.** `RegionKind::EndBlock`
/// carries an `Option<String>`, which makes the whole enum non-`Copy` and gives
/// it drop glue — so `encode_kind(&shape_kind(r.kind))` constructs and destroys
/// a heap-capable value per region, on every keystroke, for a number that is
/// three bits of shape and four of detail. On the 2 MB corpus (40 120 regions)
/// removing it took the projection from ~655 µs to the figure recorded in
/// `benches/resegment.rs`.
///
/// `the_shortcut_agrees_with_encode_kind` asserts the two agree on every variant
/// of every payload, so this cannot drift from the codec the webview decodes
/// with: adding a `RegionShape` variant fails to compile here, and changing a
/// detail code in `lib.rs` without changing it here fails that test.
const fn shape_code(shape: RegionShape) -> i32 {
    let (family, detail) = match shape {
        RegionShape::Simple => (FAMILY_SIMPLE, 0),
        RegionShape::Brace { opener } => (
            FAMILY_BRACE,
            match opener {
                BraceOpener::Foreach => 0,
                BraceOpener::Forvalues => 1,
                BraceOpener::While => 2,
                BraceOpener::IfElseChain => 3,
                BraceOpener::Capture => 4,
                BraceOpener::Quietly => 5,
                BraceOpener::Noisily => 6,
                BraceOpener::Anonymous => 7,
                BraceOpener::Other => 8,
            },
        ),
        RegionShape::EndBlock { opener } => (
            FAMILY_END_BLOCK,
            match opener {
                EndBlockOpener::Program => 0,
                EndBlockOpener::Input => 1,
                EndBlockOpener::Mata => 2,
                EndBlockOpener::Python => 3,
                EndBlockOpener::Java => 4,
            },
        ),
        RegionShape::Directive { directive } => (
            FAMILY_DIRECTIVE,
            match directive {
                DirectiveKind::DelimitCr => 0,
                DirectiveKind::DelimitSemi => 1,
                DirectiveKind::Other => 2,
            },
        ),
        RegionShape::Trivia { has_marker } => (FAMILY_TRIVIA, has_marker as i32),
        RegionShape::Unterminated { expected } => (
            FAMILY_UNTERMINATED,
            match expected {
                Unterminated::CloseBrace => 0,
                Unterminated::End => 1,
                Unterminated::BlockComment => 2,
                Unterminated::CompoundQuote => 3,
            },
        ),
    };
    (family << crate::FAMILY_SHIFT) | detail
}

fn push_sections(seg: &ParseSegmentation<'_>, out: &mut Segmentation) {
    // `marker::finish` builds both vectors in one pass, one section per marker,
    // in the same order — so index `i` of each describes the same `%%` line.
    for (s, m) in seg.sections.iter().zip(&seg.markers) {
        let title = title_span(seg.src, m);
        out.push_section(
            s.span.start..s.span.end,
            section_id(s),
            title,
            m.line as i32,
        );
    }
}

/// `SectionId` as the `i32` the flat row carries.
fn section_id(s: &SectionSpan) -> i32 {
    i32::try_from(s.id.0).unwrap_or(i32::MAX)
}

/// The title's byte range inside the marker line.
///
/// The title travels as a range rather than a string (§14): the webview already
/// holds the document, so slicing it there costs nothing and marshalling a
/// string per section costs a JS allocation per section per keystroke.
///
/// `marker_title` returns a slice OF the marker line, so its offset is the
/// difference of the two pointers — address arithmetic, no dereference, and it
/// cannot disagree with the trimming rule the way a second parse of `%%` would.
fn title_span(src: &str, m: &CellMarker) -> std::ops::Range<u32> {
    let raw = &src[m.span.start as usize..m.span.end as usize];
    match marker_title(raw) {
        Some(t) => {
            let off = (t.as_ptr() as usize - raw.as_ptr() as usize) as u32;
            let start = m.span.start + off;
            start..start + t.len() as u32
        }
        // `finish` only ever builds a marker out of a line `marker_title`
        // accepted, so this is unreachable in practice; an empty range is the
        // answer that cannot make the webview slice garbage if it ever is not.
        None => m.span.start..m.span.start,
    }
}

/// `//|` runs and `/*md … */` blocks (spec §3's narrative comments).
///
/// Both are recognised off the ONE logical-line reader's output rather than by a
/// second comment scanner: `is_trivia` already means "this line contributed no
/// code", and a `/*md` block is a single logical line however many physical
/// lines it spans, because the reader counts `/* */` nesting. A narrative marker
/// inside a string or a nested comment is therefore not one, for free.
///
/// **`stratum-intel` (W20) owns `narrative/**`.** It landed while W11b was
/// running, and `stratum_intel::narrative::detect` is where the run-grouping
/// below belongs; the trivia/`is_trivia` test is the part that belongs here.
/// The edge is not drawn yet because `stratum-intel` pulls `stratum-effects`
/// into this crate's dependency tree, and that tree is the subject of
/// `tests/parity.rs::no_forbidden_crate_is_in_the_wasm_dep_tree` — whoever draws
/// it re-runs that gate.
fn push_narrative(seg: &ParseSegmentation<'_>, out: &mut Segmentation) {
    let src = seg.src;
    let mut run: Option<Span> = None;
    for line in &seg.lines {
        if !line.is_trivia {
            if let Some(r) = run.take() {
                out.push_narrative(r.start..r.end, NARRATIVE_LINE);
            }
            continue;
        }
        let raw = &src[line.span.start as usize..line.span.end as usize];
        let body = raw.trim_start_matches(|c: char| c.is_ascii_whitespace());
        if body.starts_with("//|") {
            // Consecutive `//|` lines are ONE narrative region: the editor
            // renders a run of them as a single prose paragraph, and emitting
            // one region per line would put a widget boundary between every
            // sentence.
            match &mut run {
                Some(r) => r.end = line.span.end,
                None => run = Some(line.span),
            }
            continue;
        }
        if let Some(r) = run.take() {
            out.push_narrative(r.start..r.end, NARRATIVE_LINE);
        }
        if body.starts_with("/*md") {
            out.push_narrative(line.span.start..line.span.end, NARRATIVE_BLOCK);
        }
    }
    if let Some(r) = run {
        out.push_narrative(r.start..r.end, NARRATIVE_LINE);
    }
}

// ===========================================================================
// The golden projection (W11b's parity gate).
// ===========================================================================

/// A segmentation as canonical JSON — the artefact `tests/golden/segmentation/`
/// pins and the parity gate compares native against wasm.
///
/// # Why this is hand-written rather than `serde_json`
///
/// The comparison being made is "the same bytes came out of a native build and
/// out of node", so the writer has to be part of the thing under test. Pulling a
/// JSON library into `[dependencies]` to serialise a test artefact would also
/// put it in the shipped module, inside a 700 KB budget, for a function no user
/// ever calls — `serde_json` alone is well over a tenth of it.
///
/// # What it contains
///
/// Exactly the flat views, decoded. Not the parse tree, not `RegionSummary`: the
/// gate exists to catch a change in **what the editor is told**, so the golden is
/// written in the editor's vocabulary — the same nine `i32`s, the same three
/// `u64`s, the same section and narrative triples. A refactor inside
/// `stratum-parse` that leaves the flat rows alone leaves these files alone.
///
/// Every number here is a `u32`/`i32` and every string is ASCII-or-escaped, so
/// the output has no float formatting and no platform-width integer in it. That
/// is what makes "byte-identical on two targets" a property rather than a hope.
#[must_use]
pub fn golden_json(seg: &Segmentation) -> String {
    // One region is ~180 bytes of JSON; sizing up front keeps a 10 k-region
    // document from walking the allocator on the way to a test assertion.
    let mut out = String::with_capacity(seg.len() * 192 + 256);
    out.push_str("{\n  \"abi\": ");
    push_u32(&mut out, crate::WASM_ABI);
    out.push_str(",\n  \"regions\": ");
    write_array(&mut out, seg.len(), |out, i| {
        let r = &seg.rows[i * REGION_STRIDE..(i + 1) * REGION_STRIDE];
        let h = &seg.hashes[i * REGION_HASH_STRIDE..(i + 1) * REGION_HASH_STRIDE];
        out.push_str("{\"i\": ");
        push_u32(out, i as u32);
        out.push_str(", \"span\": ");
        push_pair(out, r[0], r[1]);
        out.push_str(", \"outer\": ");
        push_pair(out, r[2], r[3]);
        out.push_str(", \"kind\": \"");
        out.push_str(&kind_name(r[4]));
        out.push_str("\", \"entry\": \"");
        out.push_str(delim_name(r[5]));
        out.push_str("\", \"exit\": \"");
        out.push_str(if r[8] & FLAG_EXIT_SEMI == 0 {
            "cr"
        } else {
            "semi"
        });
        out.push_str("\", \"lines\": ");
        push_pair(out, r[6], r[7]);
        out.push_str(", \"flags\": ");
        push_flags(out, r[8]);
        out.push_str(", \"hash\": \"");
        push_hex64(out, h[1]);
        push_hex64(out, h[0]);
        out.push_str("\", \"ordinal\": ");
        push_u32(out, h[2] as u32);
        out.push('}');
    });
    out.push_str(",\n  \"sections\": ");
    write_array(&mut out, seg.sections.len() / SECTION_STRIDE, |out, i| {
        let s = &seg.sections[i * SECTION_STRIDE..(i + 1) * SECTION_STRIDE];
        out.push_str("{\"span\": ");
        push_pair(out, s[0], s[1]);
        out.push_str(", \"id\": ");
        push_i32(out, s[2]);
        out.push_str(", \"title\": ");
        push_pair(out, s[3], s[4]);
        out.push_str(", \"line\": ");
        push_i32(out, s[5]);
        out.push('}');
    });
    out.push_str(",\n  \"narrative\": ");
    write_array(
        &mut out,
        seg.narrative.len() / NARRATIVE_STRIDE,
        |out, i| {
            let n = &seg.narrative[i * NARRATIVE_STRIDE..(i + 1) * NARRATIVE_STRIDE];
            out.push_str("{\"span\": ");
            push_pair(out, n[0], n[1]);
            out.push_str(", \"kind\": \"");
            out.push_str(if n[2] == NARRATIVE_BLOCK {
                "block"
            } else {
                "line"
            });
            out.push_str("\"}");
        },
    );
    out.push_str(",\n  \"diagnostics\": ");
    write_array(&mut out, seg.diagnostics.len(), |out, i| {
        let d = &seg.diagnostics[i];
        out.push_str("{\"severity\": \"");
        out.push_str(severity_name(d.severity));
        out.push_str("\", \"code\": ");
        push_str_lit(out, &d.code);
        out.push_str(", \"span\": ");
        match d.span {
            Some(s) => push_pair(out, s.start as i32, s.end as i32),
            None => out.push_str("null"),
        }
        out.push_str(", \"message\": ");
        push_str_lit(out, &d.message);
        out.push('}');
    });
    out.push_str("\n}\n");
    out
}

/// `[` … `]`, one element per line, two-space indent, no trailing comma.
///
/// Written out rather than `join`ed so the whole document is one `String` and no
/// intermediate `Vec<String>` is built: the 2 MB corpus segments to ~70 000
/// regions, and a per-region allocation there is the difference between a test
/// that runs and a test nobody waits for.
fn write_array(out: &mut String, n: usize, mut each: impl FnMut(&mut String, usize)) {
    if n == 0 {
        out.push_str("[]");
        return;
    }
    out.push_str("[\n");
    for i in 0..n {
        out.push_str("    ");
        each(out, i);
        if i + 1 < n {
            out.push(',');
        }
        out.push('\n');
    }
    out.push_str("  ]");
}

fn push_pair(out: &mut String, a: i32, b: i32) {
    out.push('[');
    push_i32(out, a);
    out.push_str(", ");
    push_i32(out, b);
    out.push(']');
}

/// Flags as names, ascending by bit, so a diff says which affordance moved
/// rather than that `9` became `11`.
fn push_flags(out: &mut String, flags: i32) {
    const NAMES: [(i32, &str); 5] = [
        (FLAG_EXECUTABLE, "executable"),
        (FLAG_ESTIMATION, "estimation"),
        (FLAG_MACRO_IN_HEAD, "macro_in_head"),
        (FLAG_EXIT_SEMI, "exit_semi"),
        (FLAG_SECTION_HEAD, "section_head"),
    ];
    out.push('[');
    let mut first = true;
    for (bit, name) in NAMES {
        if flags & bit == 0 {
            continue;
        }
        if !first {
            out.push_str(", ");
        }
        first = false;
        out.push('"');
        out.push_str(name);
        out.push('"');
    }
    out.push(']');
}

/// The `kind` i32, decoded back into the name the webview shows.
///
/// Through [`crate::decode_kind`] rather than by re-deriving from the region:
/// the golden is a statement about the ENCODED row, so a decoder that could not
/// read the row is itself a failure worth recording in the file.
fn kind_name(code: i32) -> String {
    let Some(kind) = crate::decode_kind(code) else {
        return format!("undecodable:{code}");
    };
    match kind {
        RegionKind::Simple => "simple".to_owned(),
        RegionKind::Brace { opener } => format!("brace:{}", brace_name(opener)),
        RegionKind::EndBlock { opener, .. } => format!("end_block:{}", end_block_name(opener)),
        RegionKind::Directive { directive } => format!("directive:{}", directive_name(directive)),
        RegionKind::Trivia { has_marker } => {
            if has_marker {
                "trivia:marker".to_owned()
            } else {
                "trivia".to_owned()
            }
        }
        RegionKind::Unterminated { expected } => {
            format!("unterminated:{}", unterminated_name(expected))
        }
    }
}

const fn brace_name(o: stratum_proto::BraceOpener) -> &'static str {
    use stratum_proto::BraceOpener as B;
    match o {
        B::Foreach => "foreach",
        B::Forvalues => "forvalues",
        B::While => "while",
        B::IfElseChain => "if_else_chain",
        B::Capture => "capture",
        B::Quietly => "quietly",
        B::Noisily => "noisily",
        B::Anonymous => "anonymous",
        B::Other => "other",
    }
}

const fn end_block_name(o: stratum_proto::EndBlockOpener) -> &'static str {
    use stratum_proto::EndBlockOpener as E;
    match o {
        E::Program => "program",
        E::Input => "input",
        E::Mata => "mata",
        E::Python => "python",
        E::Java => "java",
    }
}

const fn directive_name(d: stratum_proto::DirectiveKind) -> &'static str {
    use stratum_proto::DirectiveKind as D;
    match d {
        D::DelimitCr => "delimit_cr",
        D::DelimitSemi => "delimit_semi",
        D::Other => "other",
    }
}

const fn unterminated_name(u: stratum_proto::Unterminated) -> &'static str {
    use stratum_proto::Unterminated as U;
    match u {
        U::CloseBrace => "close_brace",
        U::End => "end",
        U::BlockComment => "block_comment",
        U::CompoundQuote => "compound_quote",
    }
}

const fn severity_name(s: stratum_proto::Severity) -> &'static str {
    use stratum_proto::Severity as S;
    match s {
        S::Error => "error",
        S::Warning => "warning",
        S::Note => "note",
        S::Help => "help",
    }
}

const fn delim_name(code: i32) -> &'static str {
    if code == crate::DELIM_SEMI {
        "semi"
    } else {
        "cr"
    }
}

/// `write!` would pull `core::fmt`'s machinery onto this path for an integer.
/// The whole encoder is ~15 lines of digit arithmetic instead; on the 2 MB
/// corpus that is the difference between a golden dump measured in seconds and
/// one measured in tens of milliseconds.
fn push_u32(out: &mut String, mut v: u32) {
    if v == 0 {
        out.push('0');
        return;
    }
    let mut buf = [0u8; 10];
    let mut n = 0;
    while v > 0 {
        buf[n] = b'0' + (v % 10) as u8;
        v /= 10;
        n += 1;
    }
    for i in (0..n).rev() {
        out.push(buf[i] as char);
    }
}

fn push_i32(out: &mut String, v: i32) {
    if v < 0 {
        out.push('-');
        push_u32(out, v.unsigned_abs());
    } else {
        push_u32(out, v as u32);
    }
}

/// One `u64` as exactly sixteen lowercase hex digits. Two of these back to back
/// are the 32-character canonical hex of a `CodeHash`, which is `hashKey` in
/// `segmenter.ts` character for character.
fn push_hex64(out: &mut String, v: u64) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for i in (0..16).rev() {
        out.push(HEX[((v >> (i * 4)) & 0xf) as usize] as char);
    }
}

/// A JSON string literal. Minimal escaping, `\u00XX` for every control byte, so
/// the encoder has exactly one representation for any input and two targets
/// cannot disagree about it.
fn push_str_lit(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str("\\u00");
                let b = c as u32;
                const HEX: &[u8; 16] = b"0123456789abcdef";
                out.push(HEX[((b >> 4) & 0xf) as usize] as char);
                out.push(HEX[(b & 0xf) as usize] as char);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use stratum_parse::segment;

    use super::*;
    use crate::decode_kind;

    fn project_str(doc: &str) -> Segmentation {
        let mut out = Segmentation::default();
        project(&segment(doc), &mut out);
        out
    }

    /// `regionAt` in `segmenter.ts` is a binary search over `outer`; a gap or an
    /// overlap makes it return the WRONG region rather than none, which is how a
    /// result card ends up on the block above the one that produced it.
    #[test]
    fn the_rows_tile_the_document() {
        let doc = "// %% Load\nsysuse auto, clear\n\n// %% Model\nforeach v of varlist price mpg {\n    summarize `v'\n}\nregress price mpg weight\n\nprogram define mysum\n    display 1\nend\n";
        let seg = project_str(doc);
        assert!(seg.len() > 1);
        let mut cursor = 0;
        for row in seg.rows.chunks_exact(REGION_STRIDE) {
            assert_eq!(row[2], cursor, "outer spans do not tile");
            assert!(row[3] >= row[2], "inverted outer span");
            assert!(
                row[0] >= row[2] && row[1] <= row[3],
                "span {}..{} escapes outer {}..{}",
                row[0],
                row[1],
                row[2],
                row[3]
            );
            assert!(row[7] >= row[6], "last_line before head_line");
            cursor = row[3];
        }
        assert_eq!(
            cursor as usize,
            doc.len(),
            "the tiling stopped short of EOF"
        );
        assert_eq!(seg.hashes.len(), seg.len() * REGION_HASH_STRIDE);
        assert_eq!(seg.sections.len(), 2 * SECTION_STRIDE);
    }

    /// The shortcut and the codec the webview decodes with agree on every
    /// variant of every payload. Enumerated by hand because a `RegionShape` has
    /// no iterator — and the enumeration is itself the check that a new variant
    /// was thought about rather than defaulted.
    #[test]
    fn the_shortcut_agrees_with_encode_kind() {
        use stratum_proto::Unterminated as U;
        use stratum_proto::{BraceOpener as B, DirectiveKind as D, EndBlockOpener as E};

        let mut all = vec![RegionShape::Simple];
        for opener in [
            B::Foreach,
            B::Forvalues,
            B::While,
            B::IfElseChain,
            B::Capture,
            B::Quietly,
            B::Noisily,
            B::Anonymous,
            B::Other,
        ] {
            all.push(RegionShape::Brace { opener });
        }
        for opener in [E::Program, E::Input, E::Mata, E::Python, E::Java] {
            all.push(RegionShape::EndBlock { opener });
        }
        for directive in [D::DelimitCr, D::DelimitSemi, D::Other] {
            all.push(RegionShape::Directive { directive });
        }
        for has_marker in [false, true] {
            all.push(RegionShape::Trivia { has_marker });
        }
        for expected in [U::CloseBrace, U::End, U::BlockComment, U::CompoundQuote] {
            all.push(RegionShape::Unterminated { expected });
        }

        let mut seen = Vec::new();
        for shape in all {
            let code = shape_code(shape);
            assert_eq!(
                code,
                crate::encode_kind(&shape_kind(shape)),
                "the shortcut disagrees with the codec for {shape:?}"
            );
            assert!(!seen.contains(&code), "duplicate code {code} for {shape:?}");
            seen.push(code);
            assert!(
                decode_kind(code).is_some(),
                "the webview cannot decode {code} for {shape:?}"
            );
        }
    }

    #[test]
    fn every_kind_code_decodes() {
        let doc = "#delimit ;\nsummarize price;\n#delimit cr\nforeach v of varlist a {\n}\nprogram define p\nend\n* just a comment\nwhile 1 {\n";
        let seg = project_str(doc);
        for row in seg.rows.chunks_exact(REGION_STRIDE) {
            assert!(
                decode_kind(row[4]).is_some(),
                "kind {} is not decodable by the webview",
                row[4]
            );
        }
    }

    #[test]
    fn the_hash_split_is_the_canonical_hex() {
        let doc = "summarize price\n";
        let seg = project_str(doc);
        let parsed = segment(doc);
        let h = parsed.regions[0].code_hash.0;
        let canonical: String = h.iter().map(|b| format!("{b:02x}")).collect();
        // This is `hashKey` in `segmenter.ts`, character for character.
        let key = format!("{:016x}{:016x}", seg.hashes[1], seg.hashes[0]);
        assert_eq!(key, canonical);
    }

    #[test]
    fn identical_code_differs_only_by_ordinal() {
        let seg = project_str("list\nlist\n");
        assert_eq!(seg.len(), 2);
        assert_eq!(seg.hashes[0], seg.hashes[3]);
        assert_eq!(seg.hashes[1], seg.hashes[4]);
        assert_eq!((seg.hashes[2], seg.hashes[5]), (0, 1));
    }

    #[test]
    fn the_hash_survives_reindentation() {
        let a = project_str("summarize price\n");
        let b = project_str("   summarize    price\n");
        assert_eq!(a.hashes[..2], b.hashes[..2]);
    }

    #[test]
    fn a_marker_flags_the_region_it_opens_and_no_other() {
        let doc = "list\n// %% Model\nregress price mpg\nsummarize price\n";
        let seg = project_str(doc);
        let heads: Vec<bool> = seg
            .rows
            .chunks_exact(REGION_STRIDE)
            .map(|r| r[8] & FLAG_SECTION_HEAD != 0)
            .collect();
        assert_eq!(
            heads.iter().filter(|h| **h).count(),
            1,
            "exactly one region opens the one section"
        );
        assert!(!heads[0], "the region above the marker is not a head");
    }

    #[test]
    fn the_section_title_range_slices_the_title() {
        let doc = "// %%   Model fitting  \nregress price mpg\n";
        let seg = project_str(doc);
        assert_eq!(seg.sections.len(), SECTION_STRIDE);
        let (from, to) = (seg.sections[3] as usize, seg.sections[4] as usize);
        assert_eq!(&doc[from..to], "Model fitting");
    }

    #[test]
    fn narrative_runs_group_and_blocks_stand_alone() {
        let doc = "//| first\n//| second\nlist\n/*md\nprose\n*/\nsummarize price\n";
        let seg = project_str(doc);
        assert_eq!(seg.narrative.len(), 2 * crate::NARRATIVE_STRIDE);
        assert_eq!(seg.narrative[2], NARRATIVE_LINE);
        assert_eq!(
            &doc[seg.narrative[0] as usize..seg.narrative[1] as usize],
            "//| first\n//| second\n"
        );
        assert_eq!(seg.narrative[5], NARRATIVE_BLOCK);
        assert!(doc[seg.narrative[3] as usize..seg.narrative[4] as usize].starts_with("/*md"));
    }

    /// A `//|` inside a string is not a narrative comment, and the reason it is
    /// not is that this file never looks at a string: `is_trivia` is the logical
    /// line reader's answer, and it knows about quotes.
    #[test]
    fn a_narrative_marker_inside_a_string_is_not_one() {
        let seg = project_str("display \"//| not prose\"\n");
        assert!(seg.narrative.is_empty());
    }

    #[test]
    fn no_block_id_is_minted() {
        // The whole of §14's identity rule, as an assertion: the only u64s that
        // leave here are the hash and its ordinal, and the ordinal is bounded by
        // the number of regions rather than being drawn from a counter.
        let seg = project_str("list\nlist\nlist\n");
        let ordinals: Vec<u64> = seg
            .hashes
            .chunks_exact(REGION_HASH_STRIDE)
            .map(|h| h[2])
            .collect();
        assert_eq!(ordinals, vec![0, 1, 2]);
    }
}
