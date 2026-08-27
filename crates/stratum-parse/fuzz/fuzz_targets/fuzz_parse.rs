//! Parsing must not panic and must never lose the text, on arbitrary bytes.
//!
//! Decision D7 is the property under test as much as the absence of panics: the
//! parser accepts everything. An input this grammar cannot understand becomes
//! `Command::Unknown` with its tail preserved, so the editor can still fold,
//! highlight and gutter-mark an ado-file using commands this build has never
//! heard of. A target that only checked for panics would let a "reject on the
//! first surprise" regression through.
//!
//! Asserted here:
//!
//! * neither mode ever panics, in particular on a span that would land inside a
//!   UTF-8 sequence;
//! * every span in the tree is a valid slice of the input;
//! * parsing is pure;
//! * the TOLERANT mode never reports more than the strict one. That is the
//!   precise statement of "one grammar, two modes" (02 §10): speculative mode
//!   suppresses findings the editor cannot act on — an unexpanded macro, an
//!   option no table row lists — and invents none of its own.
//!
//! `cargo fuzz run fuzz_parse`. See `fuzz_expand.rs` for the note about the
//! missing `fuzz/Cargo.toml`.

#![no_main]

use libfuzzer_sys::fuzz_target;
use stratum_parse::ast::{Command, Prefix};
use stratum_parse::{parse_command, ParseMode, Span};

fn check_span(src: &str, s: Span) {
    assert!(s.start <= s.end, "inverted span {s:?}");
    assert!(s.end as usize <= src.len(), "span past the end: {s:?}");
    // Panics if either end is not on a char boundary.
    let _ = &src[s.start as usize..s.end as usize];
}

fuzz_target!(|data: &[u8]| {
    let Ok(src) = std::str::from_utf8(data) else {
        return;
    };
    let mut counts = [0usize; 2];
    for (slot, mode) in [ParseMode::Execute, ParseMode::Speculative].into_iter().enumerate() {
        let (stmt, diags) = parse_command(src, mode);
        counts[slot] = diags.iter().filter(|d| d.stata_rc.is_some()).count();

        check_span(src, stmt.span);
        check_span(src, stmt.src);
        for p in &stmt.prefixes {
            match p {
                Prefix::By(b) => check_span(src, b.span),
                Prefix::Quietly { span }
                | Prefix::Noisily { span }
                | Prefix::Capture { span }
                | Prefix::Version { span, .. }
                | Prefix::Frame { span, .. } => check_span(src, *span),
                Prefix::Generic { span, args, .. } => {
                    check_span(src, *span);
                    check_span(src, *args);
                }
            }
        }
        match &stmt.cmd {
            Command::Known(k) => {
                check_span(src, k.name_span);
                for o in &k.slots.options.items {
                    check_span(src, o.span);
                }
                if let Some(v) = &k.slots.varlist {
                    check_span(src, v.span);
                    for i in &v.items {
                        check_span(src, i.span);
                    }
                }
                if let Some(r) = &k.slots.rest {
                    check_span(src, r.span);
                }
            }
            Command::Unknown { name_span, rest, .. } => {
                check_span(src, *name_span);
                check_span(src, rest.span);
            }
            _ => {}
        }

        // Purity.
        let (again, diags2) = parse_command(src, mode);
        assert_eq!(stmt, again, "parsing must be pure");
        assert_eq!(diags, diags2);

    }
    assert!(
        counts[1] <= counts[0],
        "tolerant mode reported more than strict mode: {counts:?}"
    );
});
