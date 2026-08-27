//! Property 7 of design 02 §5.4: **no panics**, on arbitrary bytes.
//!
//! The scanner indexes `&str` by byte offset everywhere, so the two ways it can
//! fail are a slice that lands inside a UTF-8 sequence and an arithmetic
//! underflow on an empty or degenerate input. Both are panics, so the target
//! does not need an oracle — it asserts the invariants that make the segmenter
//! usable and lets the panic itself be the finding:
//!
//! * every `outer_span` and every `span` is a valid `&str` slice;
//! * the `outer_span`s tile the input exactly (property 1), which is what the
//!   editor's gutter and "run everything above" depend on;
//! * segmentation is pure (property 3).
//!
//! `cargo fuzz run fuzz_segment`. The same properties run in CI as proptests in
//! `tests/resegment.rs`, because cargo-fuzz needs a nightly toolchain and CI is
//! pinned to stable.
//!
//! NOTE FOR THE ARCHITECT: `crates/stratum-parse/fuzz/Cargo.toml` — the manifest
//! `cargo fuzz` needs — is claimed by NO unit in `docs/ownership.toml`, which
//! lists only this file. It is not created here, because creating it would fail
//! `cargo xtask ownership` (a tracked file owned by nobody). Until it is
//! assigned, this target is source that documents the property rather than one
//! that runs.

#![no_main]

use libfuzzer_sys::fuzz_target;
use stratum_parse::segment;

fuzz_target!(|data: &[u8]| {
    let Ok(src) = std::str::from_utf8(data) else {
        return;
    };
    let seg = segment(src);

    let mut at = 0u32;
    for r in &seg.regions {
        assert_eq!(r.outer_span.start, at, "regions must tile");
        assert!(r.outer_span.end > r.outer_span.start, "empty region");
        assert!(r.span.start >= r.outer_span.start && r.span.end <= r.outer_span.end);
        // Panics if either end is not on a char boundary.
        let _ = &src[r.outer_span.start as usize..r.outer_span.end as usize];
        let _ = &src[r.span.start as usize..r.span.end as usize];
        at = r.outer_span.end;
    }
    assert_eq!(at as usize, src.len(), "regions must cover the input");

    assert_eq!(seg, segment(src), "segmentation must be pure");
});
