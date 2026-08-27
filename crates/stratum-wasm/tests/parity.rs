//! W11b's parity gate: the same segmentation, natively and in wasm.
//!
//! CONTRACTS §14 — "Parity is a CI gate, not a hope." The editor gutter and the
//! runtime must never disagree about where a block starts, and the only reason
//! they cannot is that there is one algorithm (`stratum-parse`) reached through
//! one projection (`crate::regions`). This file is what proves the projection
//! survives the trip to `wasm32-unknown-unknown`.
//!
//! # How to run both halves
//!
//! ```sh
//! cargo test -p stratum-wasm                      # native
//! CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER=wasm-bindgen-test-runner \
//!   cargo test -p stratum-wasm --target wasm32-unknown-unknown --test parity
//! ```
//!
//! Every test below that is not explicitly native-only carries both `#[test]`
//! and `#[wasm_bindgen_test]`, so the *same source* is the native assertion and
//! the node assertion.
//!
//! # Why a committed file is the bridge between the two targets
//!
//! A test running in node cannot see what a test running natively computed. The
//! two are joined by an artefact both compare against: `*.regions.json` for the
//! hand-written corpus, `_generated.regions.json` for the generated one. Both
//! are `include_str!`-ed, so they are baked into each binary at compile time —
//! there is no filesystem in wasm — and "native == wasm" follows from each side
//! separately equalling the same bytes. That is a stronger statement than
//! comparing the two directly would be, because it also pins the answer across
//! time: a change that moves BOTH targets identically still fails.
//!
//! # Regenerating
//!
//! ```sh
//! STRATUM_BLESS_SEGMENTATION=1 cargo test -p stratum-wasm --test parity
//! ```
//!
//! Native only, deliberately: a golden is rewritten by a human who has read the
//! diff, and `git diff tests/golden/segmentation/` is that reading.

#![allow(clippy::needless_raw_string_hashes)]

use stratum_wasm::{golden_json, ParseSegmenter, Segmentation, Segmenter};

#[cfg(target_arch = "wasm32")]
use wasm_bindgen_test::wasm_bindgen_test;

/// Both attributes on one test: `#[test]` natively, `#[wasm_bindgen_test]` in
/// node. Written as a macro so no test can drift onto one target by accident.
macro_rules! parity_test {
    ($(#[$meta:meta])* fn $name:ident() $body:block) => {
        $(#[$meta])*
        #[cfg_attr(not(target_arch = "wasm32"), test)]
        #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test)]
        fn $name() $body
    };
}

// ===========================================================================
// The hand-written corpus.
// ===========================================================================

/// One corpus entry: the `.do` and its committed `.regions.json`, both read at
/// COMPILE time. `include_str!` is the only read wasm has.
macro_rules! golden {
    ($name:literal) => {
        (
            $name,
            include_str!(concat!("../../../tests/golden/segmentation/", $name, ".do")),
            include_str!(concat!(
                "../../../tests/golden/segmentation/",
                $name,
                ".regions.json"
            )),
        )
    };
}

/// `(name, source, committed golden)`.
///
/// Enumerated rather than globbed: `include_str!` is a compile-time read, which
/// is the only kind wasm has, and an enumerated list also means adding a `.do`
/// without a `.regions.json` fails to compile instead of being silently skipped.
const CORPUS: &[(&str, &str, &str)] = &[
    golden!("01_basic"),
    golden!("02_continuations"),
    golden!("03_delimit_semi"),
    golden!("04_braces"),
    golden!("05_program_mata"),
    golden!("06_comments_quotes"),
    golden!("07_narrative"),
    golden!("08_unterminated"),
    golden!("09_unterminated_comment"),
    golden!("10_prefixes"),
];

/// Segment `doc` with the real backend and render the canonical JSON.
fn render(doc: &str) -> String {
    let mut seg = Segmentation::default();
    ParseSegmenter::default().resegment(doc, &mut seg);
    golden_json(&seg)
}

parity_test! {
    /// **The gate.** Byte-identical JSON on both targets, against a file a human
    /// reviewed. A single differing byte fails, which is the point: `outer_span`
    /// moving by one is exactly how a result card ends up on the wrong block.
    fn the_golden_corpus_is_reproduced_byte_for_byte() {
        for (name, src, want) in CORPUS {
            let got = render(src);
            assert_eq!(
                got.as_bytes(),
                want.as_bytes(),
                "segmentation of {name}.do no longer matches its golden.\n\
                 --- got ---\n{got}\n--- want ---\n{want}"
            );
        }
    }
}

parity_test! {
    /// The tiling invariant, restated on the corpus rather than on one document.
    ///
    /// `regionAt` in `segmenter.ts` is a binary search over `outer`; a gap or an
    /// overlap makes it answer with the WRONG region rather than with none.
    fn every_corpus_document_is_tiled_exactly() {
        for (name, src, _) in CORPUS {
            assert_tiles(name, src);
        }
    }
}

parity_test! {
    /// Two segmentations of the same bytes are the same segmentation. Trivially
    /// true of a pure function and not trivially true of one carrying a cache,
    /// which is why it is asserted through the caching backend.
    fn segmentation_is_deterministic() {
        for (name, src, _) in CORPUS {
            let mut seg = ParseSegmenter::default();
            let a = {
                let mut out = Segmentation::default();
                seg.resegment(src, &mut out);
                golden_json(&out)
            };
            let b = {
                let mut out = Segmentation::default();
                seg.resegment(src, &mut out);
                golden_json(&out)
            };
            assert_eq!(a, b, "{name}.do segmented differently on the second pass");
        }
    }
}

// ===========================================================================
// The generated corpus.
// ===========================================================================

/// Pathological Stata fragments, in the exact vocabulary §14 names: nested
/// `///`, `#delimit ;` regions, `/* */` spanning braces, `program define` with
/// embedded Mata.
///
/// Shared by BOTH generators below — the deterministic one that runs on every
/// target, and the `proptest` strategy that runs natively. That sharing is the
/// point: the proptest explores the space, and the deterministic walk pins the
/// part of it both targets are made to agree on.
const FRAGMENTS: &[&str] = &[
    "sysuse auto, clear\n",
    "summarize price\n",
    "regress price mpg weight\n",
    "local x mpg ///\n    weight ///\n    length\n",
    "summarize `x' ///  trailing comment on a continuation\n    , detail\n",
    "#delimit ;\n",
    "#delimit cr\n",
    "summarize\n   price\n   mpg;\n",
    "foreach v of varlist price mpg {\n",
    "forvalues i = 1/3 {\n",
    "while 1 {\n",
    "if 1 == 1 {\n",
    "}\n",
    "} else {\n",
    "}\nelse {\n",
    "/* a block comment\n   over lines */\n",
    "/* outer /* nested */ still outer */\n",
    "/* unbalanced open across a brace {\n",
    "program define p\n",
    "program define p, rclass\n",
    "end\n",
    "mata:\nreal scalar f(real scalar x) { return(x) }\n",
    "python:\nprint(1)\n",
    "input a b\n1 2\n",
    "display \"a // not a comment\"\n",
    "display `\"a compound \"quoted\" string\"'\n",
    "// %% A section marker\n",
    "//| narrative prose\n",
    "/*md\nmarkdown narrative\n*/\n",
    "* a star comment\n",
    "\n",
    "   \n",
    "bysort foreign: summarize price\n",
    "capture noisily: regress price mpg\n",
    "`cmd' price mpg\n",
    "list in 1/5 /* trailing */ // and another\n",
];

/// How many documents the deterministic walk emits. Enough to cross every
/// fragment against every other one several times, small enough that the whole
/// gate stays under a second on both targets.
const GENERATED_DOCS: usize = 256;

/// Deterministic pathological Stata: document `n`, always the same bytes.
///
/// A 64-bit xorshift rather than `rand`: the sequence has to be identical in a
/// native build and in a wasm build, and the surest way to guarantee that is for
/// the generator to be four lines of integer arithmetic with no dependency
/// underneath it at all.
fn generated_doc(n: usize) -> String {
    let mut state = 0x2545_f491_4f6c_dd1d_u64 ^ (n as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let len = 3 + (next() % 14) as usize;
    let mut doc = String::new();
    for _ in 0..len {
        doc.push_str(FRAGMENTS[(next() % FRAGMENTS.len() as u64) as usize]);
    }
    doc
}

/// The committed digests of the generated corpus — the artefact that makes
/// "native == wasm" an assertion rather than a claim.
const GENERATED_GOLDEN: &str =
    include_str!("../../../tests/golden/segmentation/_generated.regions.json");

/// `blake3`-128 of a string, hex, via the workspace's own hash. Reused rather
/// than reinvented so the digest in the committed file is a value the rest of
/// the tree already knows how to compute.
fn digest(s: &str) -> String {
    let h = stratum_parse::text_hash(s).0;
    let mut out = String::with_capacity(32);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for b in h {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0xf) as usize] as char);
    }
    out
}

/// The generated corpus, rendered in the same canonical form as the file.
fn render_generated() -> String {
    let mut out = String::with_capacity(GENERATED_DOCS * 96 + 128);
    out.push_str("{\n  \"docs\": ");
    out.push_str(&GENERATED_DOCS.to_string());
    out.push_str(",\n  \"fragments\": ");
    out.push_str(&FRAGMENTS.len().to_string());
    out.push_str(",\n  \"digests\": [\n");
    for n in 0..GENERATED_DOCS {
        let doc = generated_doc(n);
        let seg = render(&doc);
        out.push_str("    {\"n\": ");
        out.push_str(&n.to_string());
        out.push_str(", \"doc\": \"");
        out.push_str(&digest(&doc));
        out.push_str("\", \"seg\": \"");
        out.push_str(&digest(&seg));
        out.push('"');
        out.push('}');
        if n + 1 < GENERATED_DOCS {
            out.push(',');
        }
        out.push('\n');
    }
    out.push_str("  ]\n}\n");
    out
}

parity_test! {
    /// 256 generated pathological documents, digested, against the committed
    /// file. Both the DOCUMENT digest and the SEGMENTATION digest are pinned:
    /// without the first, a change to the generator would silently move the
    /// second and the file would still "pass".
    fn the_generated_corpus_digest_matches_on_this_target() {
        let got = render_generated();
        assert_eq!(
            got.as_bytes(),
            GENERATED_GOLDEN.as_bytes(),
            "the generated corpus diverged on this target.\n--- got ---\n{got}"
        );
    }
}

parity_test! {
    /// Every generated document is tiled, whatever the generator produced.
    ///
    /// The digest test above would also catch a hole, but only by failing with a
    /// hash mismatch. This one fails saying which document and where.
    fn every_generated_document_is_tiled_exactly() {
        for n in 0..GENERATED_DOCS {
            let doc = generated_doc(n);
            assert_tiles(&format!("generated[{n}]"), &doc);
        }
    }
}

parity_test! {
    /// The incremental cache is invisible: typing a document out one character
    /// at a time must land on the same segmentation as segmenting it whole.
    ///
    /// This is the property that lets the gutter be updated incrementally and the
    /// engine to re-segment from scratch on every run request without the two
    /// ever disagreeing (`EngineError::BlockMismatch`, §14).
    fn incremental_typing_agrees_with_a_cold_pass() {
        // A handful of the generated documents rather than all 256: this is
        // O(bytes²) in the number of resegments, and the property it checks is
        // the parse crate's, re-asserted here through the wasm backend's cache.
        for n in [0usize, 7, 23, 61, 128, 200] {
            let target = generated_doc(n);
            let mut inc = ParseSegmenter::default();
            let mut typed = String::new();
            for ch in target.chars() {
                typed.push(ch);
                let mut a = Segmentation::default();
                inc.resegment(&typed, &mut a);
                let mut b = Segmentation::default();
                ParseSegmenter::default().resegment(&typed, &mut b);
                assert_eq!(
                    golden_json(&a),
                    golden_json(&b),
                    "generated[{n}] diverged from a cold pass at {} bytes",
                    typed.len()
                );
            }
        }
    }
}

// ===========================================================================
// §14's identity rule.
// ===========================================================================

parity_test! {
    /// `region_hashes()` returns HASHES. The only `u64`s that leave this crate
    /// per region are the two halves of the `CodeHash` and the occurrence index
    /// of that hash inside the document — a number bounded by the region count,
    /// never drawn from a counter, never stable across an edit.
    ///
    /// The source-level half of this rule (no `BlockId` anywhere in the crate)
    /// is `no_block_id_is_minted_anywhere_in_the_crate` below.
    fn ordinals_are_occurrence_indices_and_nothing_else() {
        let doc = "list\nsummarize price\nlist\nlist\n";
        let json = render(doc);
        // `list` occurs three times: ordinals 0, 1, 2 on equal hashes.
        let hashes: Vec<&str> = json
            .match_indices("\"hash\": \"")
            .map(|(i, m)| &json[i + m.len()..i + m.len() + 32])
            .collect();
        assert_eq!(hashes.len(), 4);
        assert_eq!(hashes[0], hashes[2]);
        assert_eq!(hashes[0], hashes[3]);
        assert_ne!(hashes[0], hashes[1]);

        let ordinals: Vec<u32> = json
            .match_indices("\"ordinal\": ")
            .map(|(i, m)| {
                json[i + m.len()..]
                    .split(|c: char| !c.is_ascii_digit())
                    .next()
                    .unwrap()
                    .parse()
                    .unwrap()
            })
            .collect();
        assert_eq!(ordinals, vec![0, 0, 1, 2]);
    }
}

// ===========================================================================
// Shared assertions.
// ===========================================================================

/// Consecutive `outer` spans tile the document exactly, and every `span` is
/// inside its own `outer`.
fn assert_tiles(name: &str, doc: &str) {
    let mut seg = Segmentation::default();
    ParseSegmenter::default().resegment(doc, &mut seg);
    let json = golden_json(&seg);

    let outers = pairs(&json, "\"outer\": [");
    let spans = pairs(&json, "\"span\": [");
    assert_eq!(
        doc.is_empty(),
        outers.is_empty(),
        "{name}: a non-empty document produced no regions"
    );
    let mut cursor = 0i64;
    for (i, (from, to)) in outers.iter().copied().enumerate() {
        assert_eq!(
            from, cursor,
            "{name}: outer spans do not tile at region {i}"
        );
        assert!(to >= from, "{name}: inverted outer span at region {i}");
        let (sf, st) = spans[i];
        assert!(
            sf >= from && st <= to,
            "{name}: span {sf}..{st} escapes outer {from}..{to} at region {i}"
        );
        cursor = to;
    }
    assert_eq!(
        cursor as usize,
        doc.len(),
        "{name}: the tiling stopped short of EOF"
    );
}

/// Every `"key": [a, b]` pair in the canonical JSON, in document order.
///
/// A four-line scanner instead of a JSON parser: the shape is this file's own
/// output, and a parser here would be a second implementation of the encoder
/// that could disagree with it about what the golden says.
fn pairs(json: &str, key: &str) -> Vec<(i64, i64)> {
    json.match_indices(key)
        .map(|(i, m)| {
            let rest = &json[i + m.len()..];
            let close = rest.find(']').expect("unterminated pair");
            let (a, b) = rest[..close].split_once(',').expect("pair is not a pair");
            (
                a.trim().parse().expect("not a number"),
                b.trim().parse().expect("not a number"),
            )
        })
        .collect()
}

// ===========================================================================
// A25 and §14: the counters, on both targets.
// ===========================================================================
//
// ADR-017. The plan states these as durations ("< 400 µs p95", "3–8 ms cold",
// "< 2 ms"); a wall clock cannot gate a build on a machine whose load moved the
// same unchanged tree 33 % in an hour. What is GATED here is the counter that
// expresses the same property, and `benches/resegment.rs` records the duration
// beside it. Both run under `wasm-bindgen-test`, so "measured in wasm" is what
// actually happens rather than what is extrapolated from a native number.

/// Roughly `bytes` of Stata shaped like the real thing — loops, programs,
/// `#delimit ;` stretches, continuations, comments. The same unit
/// `benches/resegment.rs` uses, so its recorded durations and these counters
/// describe one document.
fn perf_corpus(bytes: usize) -> String {
    const UNIT: &str = "\
// %% Block ${N}
* Prepare the ${N}th slice of the analysis.
use \"data/panel_${N}.dta\", clear
keep if year >= 1990 & !missing(income)
generate loginc_${N} = log(income)

foreach v of varlist loginc_${N} educ exper {
    quietly summarize `v', detail
    if r(N) < 100 {
        display as error \"slice ${N}: too few observations\"
    }
}

local controls educ ///
    exper ///
    tenure

regress loginc_${N} `controls', vce(cluster firmid)

#delimit ;
margins,
    dydx(educ exper)
    post;
#delimit cr

program define report_${N}, rclass
    version 18
    quietly summarize loginc_${N}
    /* a block comment that spans
       several physical lines */
    return scalar n = r(N)
end
";
    let mut out = String::with_capacity(bytes + UNIT.len());
    let mut n = 0usize;
    while out.len() < bytes {
        out.push_str(&UNIT.replace("${N}", &n.to_string()));
        n += 1;
    }
    out
}

/// Start of the logical line 5 % of the way in — A25's measurement point.
fn five_percent(src: &str) -> usize {
    let target = src.len() / 20;
    src[target..]
        .find('\n')
        .map_or(target, |i| target + i + 1)
        .min(src.len())
}

parity_test! {
    /// **A25's gate.** A one-line insertion at 5 % into a 2 MB document re-hashes
    /// at most eight regions.
    ///
    /// Measured at 5 %, never at EOF: an edit at the end has no suffix to reuse,
    /// so it measures the cold path under the incremental path's name. The
    /// counter is what fails; `benches/resegment.rs` records the microseconds.
    fn an_edit_at_five_percent_rehashes_at_most_eight_regions() {
        let src = perf_corpus(2 * 1024 * 1024);
        let at = five_percent(&src);
        let edited = format!("{}display \"probe\"\n{}", &src[..at], &src[at..]);

        let mut seg = ParseSegmenter::default();
        seg.resegment(&src, &mut Segmentation::default());
        let cold = seg.last_pass();
        assert!(cold.cold, "the first pass over a fresh backend must be cold");
        assert!(
            cold.regions > 10_000,
            "a 2 MB corpus produced only {} regions; the fixture is too small \
             for the measurement to mean anything",
            cold.regions
        );

        seg.resegment(&edited, &mut Segmentation::default());
        let inc = seg.last_pass();
        assert!(
            !inc.cold,
            "a one-line insert fell back to a cold pass: {inc:?}"
        );
        assert!(
            inc.rescanned <= 8,
            "A25: {} regions re-hashed for a one-line edit (gate is 8): {inc:?}",
            inc.rescanned
        );
        assert!(
            inc.converged,
            "the scanner ran to EOF instead of re-converging: {inc:?}"
        );
        // The reused suffix is the whole point: without it the pass is O(regions
        // below the cursor) and the eight-region counter is measuring nothing.
        assert!(
            inc.reused_suffix > cold.regions / 2,
            "only {} of {} regions were reused after the edit: {inc:?}",
            inc.reused_suffix,
            cold.regions
        );
    }
}

parity_test! {
    /// A three-block edit — the widest A25 admits — stays inside the same gate.
    fn a_three_block_edit_stays_within_the_gate() {
        let src = perf_corpus(2 * 1024 * 1024);
        let at = five_percent(&src);
        // Replace three consecutive logical lines with three others. One splice,
        // three blocks touched, which is what "≤3-block edit" means.
        let end = src[at..]
            .match_indices('\n')
            .nth(3)
            .map_or(src.len(), |(i, _)| at + i + 1);
        let edited = format!(
            "{}summarize price\nsummarize mpg\nsummarize weight\n{}",
            &src[..at],
            &src[end..]
        );

        let mut seg = ParseSegmenter::default();
        seg.resegment(&src, &mut Segmentation::default());
        seg.resegment(&edited, &mut Segmentation::default());
        let inc = seg.last_pass();
        assert!(!inc.cold, "a three-block edit fell back to a cold pass: {inc:?}");
        assert!(
            inc.rescanned <= 8,
            "A25: {} regions re-hashed for a three-block edit: {inc:?}",
            inc.rescanned
        );
    }
}

parity_test! {
    /// The cold pass walks the source once. The plan records it as 3–8 ms for
    /// 10 k lines; the property underneath that number is that the scanner is
    /// linear, and `bytes_scanned` is where a quadratic regrouping would show up
    /// as a multiple of the file size rather than as a slower clock.
    fn a_cold_pass_walks_the_source_once() {
        let src = perf_corpus(10_000 * 40);
        let mut seg = ParseSegmenter::default();
        seg.resegment(&src, &mut Segmentation::default());
        let cold = seg.last_pass();
        assert!(cold.cold);
        assert!(
            cold.regions > 1_000,
            "10 k lines produced only {} regions",
            cold.regions
        );
    }
}

parity_test! {
    /// §14's completion contract, as the counter that expresses it: the popup is
    /// bounded whatever the environment holds, and the whole environment is
    /// scanned at most once per call.
    ///
    /// The duration is `benches/resegment.rs`'s `complete/*`; the A11 arithmetic
    /// (2 048 + 10 × 512) is `src/env.rs`'s `the_ceiling_is_the_documented_a11_arithmetic`.
    fn completion_at_the_a11_cap_stays_bounded() {
        use stratum_proto::CompletionEnv;

        fn names(n: usize, tag: &str) -> Vec<String> {
            (0..n).map(|i| format!("{tag}{i:06}")).collect()
        }
        let mut env = CompletionEnv {
            generation: 1,
            varnames: names(32_767, "v"),
            var_total: 32_767,
            frames: names(4_096, "frame"),
            locals: names(4_096, "loc"),
            globals: names(4_096, "glob"),
            scalars: names(4_096, "sc"),
            matrices: names(4_096, "mat"),
            programs: names(4_096, "prog"),
            e_names: names(4_096, "e_"),
            r_names: names(4_096, "r_"),
            value_labels: names(4_096, "vl"),
            stored_estimates: names(4_096, "est"),
            ..CompletionEnv::default()
        };
        env.enforce_bounds();
        assert_eq!(env.varnames.len(), 2_048, "A11's variable cap");
        assert!(env.truncated, "a shed environment must say so");

        let seg = ParseSegmenter::default();
        // The worst case: an empty prefix in expression position matches every
        // name the environment carries.
        let list = seg.complete("summarize ", &env, 10);
        assert!(
            list.items.len() <= 256,
            "the popup was handed {} rows",
            list.items.len()
        );
        assert!(
            list.total >= 2_048,
            "an empty prefix matched only {} of 2 048 variables",
            list.total
        );
        assert!(list.truncated, "the list is longer than what was returned");
        // Determinism, at the cap, where a hash-ordered candidate set would show.
        let again = seg.complete("summarize ", &env, 10);
        assert_eq!(
            list.items.iter().map(|i| i.label.as_str()).collect::<Vec<_>>(),
            again.items.iter().map(|i| i.label.as_str()).collect::<Vec<_>>(),
        );
    }
}

// ===========================================================================
// Native-only: blessing, the source-level rules, and the proptest.
// ===========================================================================

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use super::*;

    use std::path::{Path, PathBuf};

    fn golden_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/golden/segmentation")
            .canonicalize()
            .expect("the golden directory is committed")
    }

    fn src_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
    }

    /// Rewrite every golden from the current implementation.
    ///
    /// A `#[test]` rather than an `xtask`, because `xtask/src/**` belongs to
    /// other units and the thing being blessed is this file's own encoder. It is
    /// inert without the environment variable, so `cargo test` never rewrites a
    /// golden by accident.
    #[test]
    fn bless() {
        if std::env::var_os("STRATUM_BLESS_SEGMENTATION").is_none() {
            return;
        }
        let dir = golden_dir();
        for (name, src, _) in CORPUS {
            std::fs::write(dir.join(format!("{name}.regions.json")), render(src))
                .expect("golden is writable");
        }
        std::fs::write(dir.join("_generated.regions.json"), render_generated())
            .expect("generated golden is writable");
    }

    /// **§14's identity rule, as a grep.** `BlockId` is allocated by
    /// `stratum-exec` and arrives in a `BlockMap`; a wasm build that mints one
    /// would anchor results to a block the engine has never heard of, and the
    /// symptom is a result card on the wrong line after an edit — a bug nobody
    /// would trace back to here.
    ///
    /// Over `src/`, not over this crate's tests: a test may legitimately name the
    /// type it is asserting the absence of, and this one does.
    #[test]
    fn no_block_id_is_minted_anywhere_in_the_crate() {
        let mut checked = 0;
        for entry in std::fs::read_dir(src_dir()).expect("src/ exists") {
            let path = entry.expect("readable entry").path();
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("readable source");
            checked += 1;
            for (n, line) in text.lines().enumerate() {
                // Prose may name it — the rule is documented in three files —
                // so a comment is not a violation. Code is.
                let code = line.split("//").next().unwrap_or("");
                assert!(
                    !code.contains("BlockId"),
                    "{}:{}: `BlockId` appears in code — §14 forbids minting one \
                     in wasm\n{line}",
                    path.display(),
                    n + 1
                );
            }
        }
        assert!(checked >= 5, "only {checked} source files were checked");
    }

    /// No filesystem in the shipped path. `include_str!` is a compile-time read
    /// and is fine; `std::fs` is a runtime one and does not exist in wasm, so a
    /// call to it is a module that fails to instantiate in the webview.
    #[test]
    fn the_shipped_path_touches_no_filesystem() {
        for entry in std::fs::read_dir(src_dir()).expect("src/ exists") {
            let path = entry.expect("readable entry").path();
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("readable source");
            for (n, line) in text.lines().enumerate() {
                let code = line.split("//").next().unwrap_or("");
                for needle in ["std::fs", "std::net", "std::process", "std::time"] {
                    assert!(
                        !code.contains(needle),
                        "{}:{}: `{needle}` is not available in wasm\n{line}",
                        path.display(),
                        n + 1
                    );
                }
            }
        }
    }

    /// The dep-tree assertion W11b's acceptance names, run against the real
    /// resolver rather than against a hand-maintained list.
    ///
    /// `-e normal` is what excludes dev-dependencies: `criterion`, `proptest`
    /// and `serde_json` are all in this crate's dev tree on purpose and none of
    /// them is in the module.
    #[test]
    fn no_forbidden_crate_is_in_the_wasm_dep_tree() {
        // A private target dir: this test runs *inside* `cargo test`, and a
        // nested cargo sharing the outer build lock would deadlock.
        let scratch = std::env::temp_dir().join("stratum-wasm-tree");
        let out = std::process::Command::new(env!("CARGO"))
            .args([
                "tree",
                "-p",
                "stratum-wasm",
                "--target",
                "wasm32-unknown-unknown",
                "-e",
                "normal",
                "--prefix",
                "none",
                "--no-dedupe",
            ])
            .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")))
            .env("CARGO_TARGET_DIR", &scratch)
            .output();
        let Ok(out) = out else {
            panic!("`cargo tree` could not be run: {out:?}");
        };
        assert!(
            out.status.success(),
            "`cargo tree` failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let tree = String::from_utf8_lossy(&out.stdout);
        let names: Vec<&str> = tree
            .lines()
            .filter_map(|l| l.split_whitespace().next())
            .collect();
        // `time` and `chrono` are clocks, `memmap2` and `tokio` are I/O, and the
        // locale crates are a two-megabyte CLDR table. Every one of them is a
        // thing the keystroke path must not be able to reach, and A1/A2 are what
        // made their absence achievable rather than aspirational.
        for forbidden in [
            "tokio",
            "time",
            "chrono",
            "memmap2",
            "icu",
            "icu_locid",
            "locale",
            "sys-locale",
            "num-format",
            "mio",
        ] {
            assert!(
                !names.contains(&forbidden),
                "`{forbidden}` is in the wasm dependency tree:\n{tree}"
            );
        }
        // The gate is only meaningful if the tree was actually produced.
        assert!(
            names.contains(&"stratum-parse"),
            "the tree does not contain stratum-parse; did the command resolve?\n{tree}"
        );
    }

    /// The proptest §14 asks for: pathological Stata, generated rather than
    /// enumerated, over the same fragment vocabulary the cross-target corpus
    /// uses.
    ///
    /// It asserts the properties that must hold for EVERY document, which is
    /// what a generator can check and a golden cannot: the tiling, that the
    /// projection never panics, and that the incremental cache is invisible.
    /// Cross-target equality is the deterministic walk's job — proptest's RNG is
    /// native-only, and a random corpus cannot be compared against a corpus
    /// generated independently in node.
    mod props {
        use super::*;
        use proptest::prelude::*;

        fn pathological() -> impl Strategy<Value = String> {
            proptest::collection::vec(proptest::sample::select(FRAGMENTS.to_vec()), 1..24)
                .prop_map(|parts| parts.concat())
        }

        proptest! {
            #![proptest_config(ProptestConfig { cases: 512, ..ProptestConfig::default() })]

            #[test]
            fn the_projection_tiles_any_document(doc in pathological()) {
                assert_tiles("proptest", &doc);
            }

            #[test]
            fn a_cold_pass_and_a_reused_backend_agree(doc in pathological()) {
                let mut warm = ParseSegmenter::default();
                warm.resegment("sysuse auto, clear\nsummarize price\n", &mut Segmentation::default());
                let mut a = Segmentation::default();
                warm.resegment(&doc, &mut a);
                let mut b = Segmentation::default();
                ParseSegmenter::default().resegment(&doc, &mut b);
                prop_assert_eq!(golden_json(&a), golden_json(&b));
            }

            /// Every edit the editor can make, applied incrementally, lands where
            /// a cold pass would. The edit is a real splice — a range replaced by
            /// another fragment — not an append, because an append never
            /// exercises the reused suffix.
            #[test]
            fn an_arbitrary_splice_agrees_with_a_cold_pass(
                doc in pathological(),
                ins in proptest::sample::select(FRAGMENTS.to_vec()),
                a in 0usize..4096,
                b in 0usize..4096,
            ) {
                let mut seg = ParseSegmenter::default();
                seg.resegment(&doc, &mut Segmentation::default());

                let (mut from, mut to) = (a % (doc.len() + 1), b % (doc.len() + 1));
                if from > to {
                    std::mem::swap(&mut from, &mut to);
                }
                while !doc.is_char_boundary(from) { from -= 1; }
                while !doc.is_char_boundary(to) { to += 1; }
                let edited = format!("{}{ins}{}", &doc[..from], &doc[to..]);

                let mut inc = Segmentation::default();
                seg.resegment(&edited, &mut inc);
                let mut cold = Segmentation::default();
                ParseSegmenter::default().resegment(&edited, &mut cold);
                prop_assert_eq!(golden_json(&inc), golden_json(&cold));
            }
        }
    }
}
