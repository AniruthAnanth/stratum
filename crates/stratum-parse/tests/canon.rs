//! `CodeHash` and the canonical token stream — CONTRACTS §1.2, and design 02
//! §5.4 property 6.
//!
//! Property 6 is the one spec §23 rests on: reindenting a block, reflowing a
//! `///` chain, or writing a comment above a command must NOT invalidate a
//! cached result, while any semantic edit must.

use pretty_assertions::assert_eq;
use stratum_parse::cmdsig::{CommandLookup, CommandTable};
use stratum_parse::spanmap::SpanMap;
use stratum_parse::{canonical_tokens, segment, LineIndex, Span};
use stratum_proto::TokenKind;

fn hashes(src: &str) -> Vec<stratum_proto::CodeHash> {
    segment(src).regions.iter().map(|r| r.code_hash).collect()
}

fn first_hash(src: &str) -> stratum_proto::CodeHash {
    let seg = segment(src);
    seg.regions
        .iter()
        .find(|r| r.is_executable())
        .expect("no executable region")
        .code_hash
}

// ---------------------------------------------------------------------------
// Property 6 — invariance
// ---------------------------------------------------------------------------

#[test]
fn reindentation_is_staleness_neutral() {
    let a = "foreach v of varlist mpg price {\n    summarize `v'\n}\n";
    let b = "foreach v of varlist mpg price {\n\t\t\tsummarize   `v'\n}\n";
    assert_eq!(first_hash(a), first_hash(b));
}

#[test]
fn comment_insertion_is_staleness_neutral() {
    let a = "summarize price\n";
    let b = "* explain what this does\nsummarize price\n";
    let c = "summarize price // trailing note\n";
    let d = "summarize /* interior */ price\n";
    assert_eq!(first_hash(a), first_hash(b));
    assert_eq!(first_hash(a), first_hash(c));
    assert_eq!(first_hash(a), first_hash(d));
}

#[test]
fn continuation_reflow_is_staleness_neutral() {
    let a = "regress price mpg weight foreign\n";
    let b = "regress price ///\n    mpg ///\n    weight ///\n    foreign\n";
    assert_eq!(first_hash(a), first_hash(b));
}

#[test]
fn a_semantic_edit_changes_the_hash() {
    assert_ne!(
        first_hash("summarize price\n"),
        first_hash("summarize mpg\n")
    );
    assert_ne!(first_hash("gen x = a + b\n"), first_hash("gen x = a - b\n"));
    // Whitespace INSIDE a string literal is significant and must survive
    // collapsing: `di "a  b"` and `di "a b"` print different things.
    assert_ne!(first_hash("di \"a  b\"\n"), first_hash("di \"a b\"\n"));
}

#[test]
fn delimiter_mode_is_folded_into_the_hash() {
    // CONTRACTS §1.2 rule 3: `;`-mode code must not collide with the same tokens
    // in `cr` mode, because they do not mean the same thing.
    let cr = segment("di 1\n");
    let semi = segment("#delimit ;\ndi 1 ;\n");
    let a = cr.regions[0].code_hash;
    let b = semi.regions[1].code_hash;
    assert_ne!(a, b);
}

#[test]
fn hash_ordinal_disambiguates_identical_regions() {
    let seg = segment("summarize price\nsummarize price\nsummarize price\n");
    assert_eq!(seg.regions.len(), 3);
    assert_eq!(seg.regions[0].code_hash, seg.regions[2].code_hash);
    let ordinals: Vec<u32> = seg.regions.iter().map(|r| r.hash_ordinal).collect();
    assert_eq!(ordinals, [0, 1, 2]);
}

#[test]
fn trivia_regions_hash_alike_and_are_ordinal_distinguished() {
    let seg = segment("* one\n\ndi 1\n\n* two\n\ndi 2\n");
    let trivia: Vec<&stratum_parse::Region> =
        seg.regions.iter().filter(|r| !r.is_executable()).collect();
    assert!(trivia.len() >= 2);
    assert_eq!(trivia[0].code_hash, trivia[1].code_hash);
    assert_ne!(trivia[0].hash_ordinal, trivia[1].hash_ordinal);
}

#[test]
fn every_region_in_a_document_gets_a_hash() {
    let src = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/segment/blocks.do"
    ))
    .unwrap();
    assert_eq!(hashes(&src).len(), segment(&src).regions.len());
}

// ---------------------------------------------------------------------------
// The token stream itself
// ---------------------------------------------------------------------------

#[test]
fn strings_and_macro_refs_are_byte_exact() {
    let seg = segment("di \"a  b\" `local' $global\n");
    let toks = canonical_tokens(seg.src, &seg.lines, &seg.derived);
    let kinds: Vec<TokenKind> = toks.iter().map(|t| t.kind).collect();
    assert_eq!(kinds[0], TokenKind::StatementBreak);
    let texts: Vec<&str> = toks
        .iter()
        .filter(|t| {
            matches!(
                t.kind,
                TokenKind::StrLit | TokenKind::MacroRef | TokenKind::Ident
            )
        })
        .map(|t| t.text.as_str())
        .collect();
    assert_eq!(texts, ["di", "\"a  b\"", "`local'", "$global"]);
}

#[test]
fn whitespace_runs_collapse_to_one_separator() {
    let seg = segment("di    1\n");
    let toks = canonical_tokens(seg.src, &seg.lines, &seg.derived);
    let ws: Vec<&str> = toks
        .iter()
        .filter(|t| t.kind == TokenKind::Whitespace)
        .map(|t| t.text.as_str())
        .collect();
    assert_eq!(ws, [" "]);
}

#[test]
fn compound_quotes_are_one_token() {
    let seg = segment("di `\"a `\"b\"' c\"'\n");
    let toks = canonical_tokens(seg.src, &seg.lines, &seg.derived);
    let cq: Vec<&str> = toks
        .iter()
        .filter(|t| t.kind == TokenKind::CompoundQuote)
        .map(|t| t.text.as_str())
        .collect();
    assert_eq!(cq, ["`\"a `\"b\"' c\"'"]);
}

// ---------------------------------------------------------------------------
// SpanMap — the other half of "underline the right byte" (spec §21)
// ---------------------------------------------------------------------------

#[test]
fn spanmap_maps_spliced_code_back_to_source() {
    // `local u ab/*⏎*/cd` -> code "local u abcd"; the `c` of `cd` is on line 2.
    let src = "local u ab/*\n*/cd\n";
    let seg = segment(src);
    let line = &seg.lines[0];
    let d = seg.derived[0].as_deref();
    let code = line.code(src, d);
    assert_eq!(code, "local u abcd");
    let c_at = code.find("cd").unwrap() as u32;
    let src_off = line.to_source(d, c_at);
    assert_eq!(&src[src_off as usize..src_off as usize + 2], "cd");
    let spans = line.span_to_source(
        d,
        Span {
            start: code.find("ab").unwrap() as u32,
            end: c_at + 2,
        },
    );
    assert_eq!(
        spans.len(),
        2,
        "a spliced span is genuinely two source ranges"
    );
}

#[test]
fn spanmap_compose_is_transitive() {
    // a -> b: drop the first 3 bytes; b -> c: shift by 10.
    let mut ab = SpanMap::new();
    ab.push(0, 3, 5);
    let mut bc = SpanMap::new();
    bc.push(0, 10, 20);
    let ac = ab.compose(&bc);
    for i in 0..5u32 {
        assert_eq!(ac.to_source(i), bc.to_source(ab.to_source(i)));
    }
}

// ---------------------------------------------------------------------------
// LineIndex
// ---------------------------------------------------------------------------

#[test]
fn line_index_agrees_with_a_from_scratch_rebuild_after_an_edit() {
    let old = "aaa\nbbb\nccc\nddd\n";
    let new = "aaa\nXX\nYY\nZZ\nccc\nddd\n";
    let li = LineIndex::new(old).patch(
        new,
        Span { start: 4, end: 8 },
        u32::try_from("XX\nYY\nZZ\n".len()).unwrap(),
    );
    let fresh = LineIndex::new(new);
    assert_eq!(li, fresh);
}

// ---------------------------------------------------------------------------
// The wave-1 command table
// ---------------------------------------------------------------------------

#[test]
fn cmdsig_rows_are_sorted_and_unique() {
    let rows = CommandTable::core().rows();
    for w in rows.windows(2) {
        assert!(
            w[0].canonical < w[1].canonical,
            "CORE_COMMANDS is binary-searched: {} must precede {}",
            w[0].canonical,
            w[1].canonical
        );
    }
}

#[test]
fn abbreviations_resolve_the_way_stata_does() {
    let t = CommandTable::core();
    // `d` is describe and `di` is display: the shortest legal abbreviation of
    // each is what disambiguates, not the order of the table.
    assert_eq!(t.canonical("d").map(|s| s.canonical), Some("describe"));
    assert_eq!(t.canonical("di").map(|s| s.canonical), Some("display"));
    assert_eq!(t.canonical("g").map(|s| s.canonical), Some("generate"));
    assert_eq!(t.canonical("gl").map(|s| s.canonical), Some("global"));
    assert_eq!(t.canonical("l").map(|s| s.canonical), Some("list"));
    assert_eq!(t.canonical("su").map(|s| s.canonical), Some("summarize"));
    assert_eq!(t.canonical("reg").map(|s| s.canonical), Some("regress"));
    // `replace` and `drop` cannot be abbreviated at all ([U] 11.2.1).
    assert!(matches!(t.resolve("repl"), CommandLookup::Unknown));
    assert!(matches!(t.resolve("dr"), CommandLookup::Unknown));
    // `s` is ambiguous between six commands, none of which allows one letter.
    assert!(matches!(t.resolve("s"), CommandLookup::Unknown));
    assert!(matches!(t.resolve("summarize"), CommandLookup::Exact(_)));
}

#[test]
fn the_core_table_admits_it_is_provisional() {
    // W04b's generated table supersedes it. A consumer that needs `slots` must
    // branch on this rather than read an empty mask as "this command takes no
    // varlist".
    assert!(CommandTable::core().is_provisional());
    assert!(!CommandTable::new(CommandTable::core().rows()).is_provisional());
}
