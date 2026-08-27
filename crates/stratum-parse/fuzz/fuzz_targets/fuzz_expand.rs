//! Macro expansion must terminate and must not panic, on arbitrary bytes.
//!
//! The expander indexes `&str` by byte offset (`match_backtick`, `match_brace`,
//! `greedy_name`), so the two ways it can fail are a slice landing inside a
//! UTF-8 sequence and a recursion that does not converge. Both are covered here
//! by asserting the invariants that make expansion usable and letting the panic
//! itself be the finding:
//!
//! * expansion is PURE — the same input and environment give the same output;
//! * the piece table only ever maps into the input, so a span composed through
//!   it can be sliced;
//! * the depth and length caps are honoured, which is what turns
//!   `local a `a'` into r(920) instead of a hang.
//!
//! `cargo fuzz run fuzz_expand`. The same properties run in CI as ordinary tests
//! in `tests/macro.rs`, because cargo-fuzz needs a nightly toolchain and CI is
//! pinned to stable.
//!
//! NOTE FOR THE ARCHITECT: `crates/stratum-parse/fuzz/Cargo.toml` — the manifest
//! `cargo fuzz` needs — is claimed by NO unit in `docs/ownership.toml`, which
//! lists only the target files. It is not created here, because creating it
//! would fail `cargo xtask ownership` (a tracked file owned by nobody). W04 left
//! the identical note on `fuzz_segment.rs`; until the partition is amended this
//! target is source that documents the property rather than one that runs.

#![no_main]

use libfuzzer_sys::fuzz_target;
use stratum_parse::macros::{expand, MacroEnv, NoHost};

fuzz_target!(|data: &[u8]| {
    let Ok(src) = std::str::from_utf8(data) else {
        return;
    };
    let mut env = MacroEnv::new();
    // A self-referential local, so the depth cap is on the hot path rather than
    // being reached only by an unlikely random input.
    env.set_local("a", "`a'`b'");
    env.set_local("b", "$a");
    env.set_global("a", "`a'");
    // Small caps keep each case cheap; the CODE PATH is what is under test.
    env.limits.max_depth = 24;
    env.limits.max_expanded_len = 1 << 16;

    let mut host = NoHost;
    let Ok(first) = expand(src, &mut env.clone(), &mut host) else {
        return;
    };

    // Purity: the same input and the same environment give the same output.
    let second = expand(src, &mut env.clone(), &mut host).expect("second run must also succeed");
    assert_eq!(first.text, second.text, "expansion must be pure");
    assert_eq!(first.map, second.map);

    // Every mapped offset must be a valid index into the INPUT, or the composed
    // span cannot be sliced to underline an error (spec §21).
    for off in 0..=first.text.len() as u32 {
        let at = first.map.to_source(off);
        assert!(at as usize <= src.len(), "map escaped the input");
    }

    assert!(first.text.len() <= env.limits.max_expanded_len as usize);
    assert!(first.stats.max_depth <= env.limits.max_depth);
});
