//! CONTRACTS.md §14 — the `stratum-wasm` surface.
//!
//! This crate is the *harness*: the document buffer, the splice protocol, the
//! flat typed-array encoding, and the `#[wasm_bindgen]` methods §14 names. It
//! deliberately contains **no segmentation algorithm**. The algorithm has one
//! implementation, `stratum-parse` (W04), and it is reached through the
//! [`Segmenter`] seam below — because the editor's idea of where a block starts
//! and the runtime's idea of where a block starts must be the same code, not
//! two implementations that agree today.
//!
//! # What this crate must never grow
//!
//! * **A `BlockId`.** §14 is explicit: `region_hashes` returns *hashes*, not
//!   identities. Identity is allocated by `stratum-exec` and arrives in a
//!   `BlockMap`. Anything in here that mints an id is a bug that will show up
//!   as results anchored to the wrong block after an edit.
//! * **A second segmenter that can reach a user.** The one segmentation
//!   algorithm is `stratum-parse`'s. [`ReferenceSegmenter`] approximates it
//!   until W11b lands, and is fenced by [`engine_linked`] returning `false` —
//!   `loader.ts` refuses such a module outright in production, which is a
//!   stronger fence than emitting no regions, because a refusal is loud and an
//!   empty document is not.
//! * **A panic on the typing path.** Every method here runs inside the
//!   CodeMirror transaction cycle. A malformed splice records a [`Diagnostic`]
//!   and leaves the document untouched; it never unwinds into JS.
//!
//! # Coordinates
//!
//! Every offset that crosses this boundary is a **UTF-8 byte offset** into the
//! document. CodeMirror counts UTF-16 code units, so the conversion is the
//! TypeScript wrapper's job (`apps/desktop/src/wasm/segmenter.ts`) and is done
//! once per transaction, not once per region.
//!
//! # Layout versioning
//!
//! The flat views are a positional encoding, which §15 otherwise forbids on the
//! wire; it is permitted here because both ends are built from this file and
//! [`abi_version`] makes a mismatch a startup error rather than a silent
//! misread. **Any change to a stride or a field position bumps [`WASM_ABI`].**

use std::ops::Range;

use serde::{Deserialize, Serialize};
use stratum_proto::{
    BraceOpener, CompletionEnv, Confidence, Delimiter, Diagnostic, DirectiveKind, EndBlockOpener,
    RegionKind, Severity, Suggestion, TokenKind, Unterminated,
};
use wasm_bindgen::prelude::*;

// ===========================================================================
// Flat view layout. Mirrored, field for field, by
// `apps/desktop/src/wasm/types.ts`; a change here without a change there is
// caught by `abi_version()` at load time.
// ===========================================================================

/// Version of the flat typed-array layout below.
///
/// `loader.ts` compares this against its own copy and refuses a module that
/// disagrees. Bump on any stride change, any field reordering, and any change
/// to the meaning of a `kind` or `flags` bit.
pub const WASM_ABI: u32 = 1;

/// `i32`s per region in [`Engine::regions_view`].
///
/// `[span_from, span_to, outer_from, outer_to, kind, entry_delim, head_line,
///   last_line, flags]` — CONTRACTS §14 verbatim.
pub const REGION_STRIDE: usize = 9;

/// `u64`s per region in [`Engine::region_hashes`]: `[hash_lo, hash_hi,
/// hash_ordinal]`.
pub const REGION_HASH_STRIDE: usize = 3;

/// `i32`s per token in [`Engine::tokens`]: `[from, to, tag]`.
pub const TOKEN_STRIDE: usize = 3;

/// `i32`s per section in [`Engine::sections`]: `[span_from, span_to, id,
/// title_from, title_to, marker_line]`.
///
/// The title travels as a byte range rather than a string: the webview already
/// holds the document, so slicing it there costs nothing and marshalling a
/// string per section costs a JS allocation per section per keystroke.
pub const SECTION_STRIDE: usize = 6;

/// `i32`s per narrative region in [`Engine::narrative_regions`]: `[from, to,
/// kind]`, where kind is [`NARRATIVE_LINE`] or [`NARRATIVE_BLOCK`].
pub const NARRATIVE_STRIDE: usize = 3;

/// A `//|` narrative comment run.
pub const NARRATIVE_LINE: i32 = 0;
/// A `/*md … */` narrative block.
pub const NARRATIVE_BLOCK: i32 = 1;

/// `flags` bit: the region can be sent to the engine as a run request. False for
/// [`RegionKind::Trivia`] and for [`RegionKind::Unterminated`] absent an
/// explicit override.
pub const FLAG_EXECUTABLE: i32 = 1 << 0;
/// `flags` bit: mirrors `RegionSummary::is_estimation`.
pub const FLAG_ESTIMATION: i32 = 1 << 1;
/// `flags` bit: mirrors `RegionSummary::has_macro_in_head`. Completion downgrades
/// to text and the canonical name is unavailable.
pub const FLAG_MACRO_IN_HEAD: i32 = 1 << 2;
/// `flags` bit: the delimiter in force *after* this region is `;`.
///
/// `RegionSummary` carries entry and exit delimiters; the flat row has a field
/// for the entry one only, so the exit rides here.
pub const FLAG_EXIT_SEMI: i32 = 1 << 3;
/// `flags` bit: the region opens a section (`RegionSummary::section` is set).
pub const FLAG_SECTION_HEAD: i32 = 1 << 4;

/// `entry_delim` / [`FLAG_EXIT_SEMI`] encoding of [`Delimiter::Cr`].
pub const DELIM_CR: i32 = 0;
/// `entry_delim` encoding of [`Delimiter::Semi`].
pub const DELIM_SEMI: i32 = 1;

// --- `kind` codec -----------------------------------------------------------
//
// `RegionKind` is a tagged union with payloads; the flat row has one i32 for it.
// The encoding is `(family << 8) | detail`, which keeps both halves readable in
// a debugger and leaves room for detail codes to grow without a stride change.
// `EndBlock { name }` loses its name — the webview slices it out of the document
// when it wants one, which is cheaper than marshalling a string per region.

const FAMILY_SHIFT: i32 = 8;
const DETAIL_MASK: i32 = 0xff;

/// `kind >> 8` for [`RegionKind::Simple`].
pub const FAMILY_SIMPLE: i32 = 0;
/// `kind >> 8` for [`RegionKind::Brace`]; detail is a [`BraceOpener`].
pub const FAMILY_BRACE: i32 = 1;
/// `kind >> 8` for [`RegionKind::EndBlock`]; detail is an [`EndBlockOpener`].
pub const FAMILY_END_BLOCK: i32 = 2;
/// `kind >> 8` for [`RegionKind::Directive`]; detail is a [`DirectiveKind`].
pub const FAMILY_DIRECTIVE: i32 = 3;
/// `kind >> 8` for [`RegionKind::Trivia`]; detail is `has_marker` as 0 or 1.
pub const FAMILY_TRIVIA: i32 = 4;
/// `kind >> 8` for [`RegionKind::Unterminated`]; detail is an [`Unterminated`].
pub const FAMILY_UNTERMINATED: i32 = 5;

/// Pack a [`RegionKind`] into the flat row's `kind` field.
#[must_use]
pub fn encode_kind(kind: &RegionKind) -> i32 {
    let (family, detail) = match kind {
        RegionKind::Simple => (FAMILY_SIMPLE, 0),
        RegionKind::Brace { opener } => (
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
        RegionKind::EndBlock { opener, .. } => (
            FAMILY_END_BLOCK,
            match opener {
                EndBlockOpener::Program => 0,
                EndBlockOpener::Input => 1,
                EndBlockOpener::Mata => 2,
                EndBlockOpener::Python => 3,
                EndBlockOpener::Java => 4,
            },
        ),
        RegionKind::Directive { directive } => (
            FAMILY_DIRECTIVE,
            match directive {
                DirectiveKind::DelimitCr => 0,
                DirectiveKind::DelimitSemi => 1,
                DirectiveKind::Other => 2,
            },
        ),
        RegionKind::Trivia { has_marker } => (FAMILY_TRIVIA, i32::from(*has_marker)),
        RegionKind::Unterminated { expected } => (
            FAMILY_UNTERMINATED,
            match expected {
                Unterminated::CloseBrace => 0,
                Unterminated::End => 1,
                Unterminated::BlockComment => 2,
                Unterminated::CompoundQuote => 3,
            },
        ),
    };
    (family << FAMILY_SHIFT) | detail
}

/// Unpack a `kind` field. `None` for a code this build does not know, which is
/// how an older webview survives a newer module within one [`WASM_ABI`].
///
/// [`RegionKind::EndBlock`] always decodes with `name: None`; the name is not in
/// the flat row.
#[must_use]
pub fn decode_kind(code: i32) -> Option<RegionKind> {
    let detail = code & DETAIL_MASK;
    match code >> FAMILY_SHIFT {
        FAMILY_SIMPLE => Some(RegionKind::Simple),
        FAMILY_BRACE => Some(RegionKind::Brace {
            opener: match detail {
                0 => BraceOpener::Foreach,
                1 => BraceOpener::Forvalues,
                2 => BraceOpener::While,
                3 => BraceOpener::IfElseChain,
                4 => BraceOpener::Capture,
                5 => BraceOpener::Quietly,
                6 => BraceOpener::Noisily,
                7 => BraceOpener::Anonymous,
                8 => BraceOpener::Other,
                _ => return None,
            },
        }),
        FAMILY_END_BLOCK => Some(RegionKind::EndBlock {
            opener: match detail {
                0 => EndBlockOpener::Program,
                1 => EndBlockOpener::Input,
                2 => EndBlockOpener::Mata,
                3 => EndBlockOpener::Python,
                4 => EndBlockOpener::Java,
                _ => return None,
            },
            name: None,
        }),
        FAMILY_DIRECTIVE => Some(RegionKind::Directive {
            directive: match detail {
                0 => DirectiveKind::DelimitCr,
                1 => DirectiveKind::DelimitSemi,
                2 => DirectiveKind::Other,
                _ => return None,
            },
        }),
        FAMILY_TRIVIA => Some(RegionKind::Trivia {
            has_marker: detail != 0,
        }),
        FAMILY_UNTERMINATED => Some(RegionKind::Unterminated {
            expected: match detail {
                0 => Unterminated::CloseBrace,
                1 => Unterminated::End,
                2 => Unterminated::BlockComment,
                3 => Unterminated::CompoundQuote,
                _ => return None,
            },
        }),
        _ => None,
    }
}

/// Encode a [`Delimiter`] for the flat row.
#[must_use]
pub const fn encode_delimiter(d: Delimiter) -> i32 {
    match d {
        Delimiter::Cr => DELIM_CR,
        Delimiter::Semi => DELIM_SEMI,
    }
}

// --- token `tag` codec ------------------------------------------------------
//
// The `tag` in an `[from, to, tag]` triple is a [`TokenKind`] as a small
// integer. The mapping is written out rather than derived from declaration
// order, because reordering `TokenKind` in proto — an additive, legal change
// under §15 — would otherwise silently repaint the editor.

/// Every [`TokenKind`], in `tag` order. Index is the wire tag.
const TOKEN_TAGS: [TokenKind; 20] = [
    TokenKind::Ident,
    TokenKind::Number,
    TokenKind::StrLit,
    TokenKind::CompoundQuote,
    TokenKind::MacroRef,
    TokenKind::Op,
    TokenKind::Comma,
    TokenKind::Colon,
    TokenKind::LParen,
    TokenKind::RParen,
    TokenKind::LBrace,
    TokenKind::RBrace,
    TokenKind::LBracket,
    TokenKind::RBracket,
    TokenKind::Comment,
    TokenKind::Whitespace,
    TokenKind::StatementBreak,
    TokenKind::Continuation,
    TokenKind::Directive,
    TokenKind::Unknown,
];

/// Encode a [`TokenKind`] as the `tag` of a token triple.
#[must_use]
pub fn encode_token_kind(k: TokenKind) -> i32 {
    // Linear over 20 entries, called once per token; the table is the contract,
    // so a lookup that cannot disagree with it is worth the scan.
    TOKEN_TAGS
        .iter()
        .position(|t| *t == k)
        .map_or_else(unreachable_tag, |i| i as i32)
}

/// Decode a token `tag`. `None` for a tag this build does not know.
#[must_use]
pub fn decode_token_kind(tag: i32) -> Option<TokenKind> {
    usize::try_from(tag)
        .ok()
        .and_then(|i| TOKEN_TAGS.get(i))
        .copied()
}

/// `TOKEN_TAGS` is exhaustive over `TokenKind`; the test below proves it, so
/// this is the arm that cannot be taken rather than a fallback.
fn unreachable_tag() -> i32 {
    TOKEN_TAGS.len() as i32 - 1 // Unknown
}

// ===========================================================================
// Completion payload.
//
// CONTRACTS §14 types `complete()`/`quick_fixes()`/`lints()` as `JsValue` and
// freezes no payload, so the shape below is this crate's, not proto's, and it is
// the one `apps/desktop/src/wasm/types.ts` mirrors. `quick_fixes` and `lints`
// deliberately reuse the frozen `Suggestion` and `Diagnostic` rather than
// inventing near-copies. See the return report: §14 wants a declared payload.
// ===========================================================================

/// What a completion item refers to. Drives the popup's icon and sort group.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionKind {
    /// A Stata command in command position.
    Command,
    /// An option name inside the current command's option list.
    Option,
    /// A variable in the active frame.
    Variable,
    /// A local macro, offered after `` ` ``.
    Local,
    /// A global macro, offered after `$`.
    Global,
    /// A scalar.
    Scalar,
    /// A matrix.
    Matrix,
    /// A frame name.
    Frame,
    /// A value-label name.
    ValueLabel,
    /// A stored estimate name.
    StoredEstimate,
    /// An `e()` or `r()` member.
    StoredResult,
    /// A built-in function in an expression.
    Function,
    /// A path completion inside a quoted filename.
    Path,
    /// A language keyword (`if`, `in`, `using`, `by`).
    Keyword,
}

/// One row in the completion popup.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct CompletionItem {
    /// The text shown in the popup.
    pub label: String,
    /// What it is.
    pub kind: CompletionKind,
    /// Right-aligned annotation: a storage type, a signature, a frame name.
    /// Never a variable *label* — those are fetched for visible rows only
    /// (A11), off the keystroke path.
    pub detail: Option<String>,
    /// Text to insert when it differs from `label` (`"strpos("` for a function).
    pub insert: Option<String>,
    /// Sort rank within `kind`, ascending. Ties break on `label`, so the popup
    /// is a total order and therefore reproducible.
    pub rank: i32,
}

/// The result of [`Engine::complete`].
#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct CompletionList {
    /// Byte range the accepted item replaces — the token under the cursor, not
    /// the cursor position, so `reg pri` completes `pri` rather than appending.
    pub from: u32,
    /// End of the replaced range.
    pub to: u32,
    /// Ordered; the popup renders them as given.
    pub items: Vec<CompletionItem>,
    /// True when the environment behind these items was itself capped (A11).
    /// The popup renders "`offered` of `total`" and offers "more…".
    pub truncated: bool,
    /// Candidates offered.
    pub offered: u32,
    /// Candidates that exist. Equal to `offered` unless `truncated`.
    pub total: u32,
}

// ===========================================================================
// The segmenter seam.
// ===========================================================================

/// One region, already flattened into the shape [`Engine::regions_view`] emits.
///
/// A backend fills these in; nothing here interprets them. The field order is
/// the row order on purpose — [`Segmentation::push`] is a memcpy in disguise.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RegionRow {
    /// Executable extent: first code byte .. last code byte.
    pub span: Range<u32>,
    /// `span` plus attached comments. Consecutive `outer`s tile the file.
    pub outer: Range<u32>,
    /// [`encode_kind`] of the region's [`RegionKind`].
    pub kind: i32,
    /// Delimiter in force at `span.start`, via [`encode_delimiter`].
    pub entry_delim: i32,
    /// 0-based first line of `span`.
    pub head_line: i32,
    /// 0-based last line of `span`.
    pub last_line: i32,
    /// `FLAG_*` bits.
    pub flags: i32,
    /// `CodeHash` bytes 8..16, big-endian.
    ///
    /// **NORMATIVE FOR W11b.** `CodeHash` is `[u8; 16]`, and the webview builds
    /// its `hashKey` as `format!("{hash_hi:016x}{hash_lo:016x}")`. For that
    /// string to equal the hash's canonical hex — which is what lets the editor
    /// match a region against `RegionSummary::code_hash` in the `BlockMap`
    /// arriving from the engine — the split must be
    /// `hi = u64::from_be_bytes(h[0..8])`, `lo = u64::from_be_bytes(h[8..16])`.
    /// Any other split silently breaks result-anchoring across a reconcile.
    pub hash_lo: u64,
    /// `CodeHash` bytes 0..8, big-endian. See [`RegionRow::hash_lo`].
    pub hash_hi: u64,
    /// 0-based occurrence index of this hash within the document. With
    /// `(hash_lo, hash_hi)` this is the frontend's pre-`BlockMap` key — and it
    /// is as close to identity as anything in this crate is allowed to get.
    pub hash_ordinal: u64,
}

/// A backend's output for one document.
#[derive(Clone, Default, Debug)]
pub struct Segmentation {
    /// Flat `i32` rows, [`REGION_STRIDE`] each.
    rows: Vec<i32>,
    /// Flat `u64` rows, [`REGION_HASH_STRIDE`] each.
    hashes: Vec<u64>,
    /// Flat `i32` rows, [`SECTION_STRIDE`] each.
    sections: Vec<i32>,
    /// Flat `i32` rows, [`NARRATIVE_STRIDE`] each.
    narrative: Vec<i32>,
    /// Parse diagnostics. Rare, so JSON is fine (§14).
    diagnostics: Vec<Diagnostic>,
}

impl Segmentation {
    /// Drop everything, keeping the allocations. Called once per resegment.
    pub fn clear(&mut self) {
        self.rows.clear();
        self.hashes.clear();
        self.sections.clear();
        self.narrative.clear();
        self.diagnostics.clear();
    }

    /// Append one region.
    pub fn push(&mut self, r: &RegionRow) {
        self.rows.extend_from_slice(&[
            i32_of(r.span.start),
            i32_of(r.span.end),
            i32_of(r.outer.start),
            i32_of(r.outer.end),
            r.kind,
            r.entry_delim,
            r.head_line,
            r.last_line,
            r.flags,
        ]);
        self.hashes
            .extend_from_slice(&[r.hash_lo, r.hash_hi, r.hash_ordinal]);
    }

    /// Append one section marker.
    pub fn push_section(&mut self, span: Range<u32>, id: i32, title: Range<u32>, line: i32) {
        self.sections.extend_from_slice(&[
            i32_of(span.start),
            i32_of(span.end),
            id,
            i32_of(title.start),
            i32_of(title.end),
            line,
        ]);
    }

    /// Append one narrative region ([`NARRATIVE_LINE`] or [`NARRATIVE_BLOCK`]).
    pub fn push_narrative(&mut self, span: Range<u32>, kind: i32) {
        self.narrative
            .extend_from_slice(&[i32_of(span.start), i32_of(span.end), kind]);
    }

    /// Append a parse diagnostic.
    pub fn push_diagnostic(&mut self, d: Diagnostic) {
        self.diagnostics.push(d);
    }

    /// Number of regions.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rows.len() / REGION_STRIDE
    }

    /// True when the document produced no regions at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

/// The single seam between the harness and the segmentation algorithm.
///
/// Exactly one real implementation will ever exist: W11b's `engine.rs`, wrapping
/// `stratum-parse` and `stratum-intel`. The trait exists so that this file can be
/// written, tested and shipped before that one is, not so that backends can
/// multiply.
pub trait Segmenter {
    /// Rebuild `out` from `doc`. Called at most once per transaction.
    fn resegment(&mut self, doc: &str, out: &mut Segmentation);

    /// Append `[from, to, tag]` triples for tokens overlapping `range`.
    fn tokens(&mut self, doc: &str, range: Range<usize>, out: &mut Vec<i32>);

    /// Deterministic completion at `pos`. HARD CONTRACT: < 2 ms (§14).
    fn complete(&self, doc: &str, env: &CompletionEnv, pos: usize) -> CompletionList;

    /// Deterministic fixes at `pos`.
    fn quick_fixes(&self, doc: &str, pos: usize) -> Vec<Suggestion>;

    /// Whole-document lints that need no session state.
    fn lints(&self, doc: &str) -> Vec<Diagnostic>;
}

/// The backend a build links while the real segmenter is still being written.
///
/// It runs the **same deliberately naive rule** as the TypeScript development
/// stub (`apps/desktop/src/wasm/stub/naive.ts`), row for row and hash for hash,
/// and it exists for one reason. `loader.ts`'s claim to be interchangeable
/// between the stub and the real module is a claim about a *data path* — flat
/// rows written here, read out of wasm linear memory, decoded by `segmenter.ts`
/// — and a backend that emits nothing never puts a byte down that path. With
/// this linked, `conformance.ts` compares the two backends' regions and a
/// stride, an ordering or a sign error fails CI. Without it that comparison has
/// nothing to compare, and W11a's first acceptance bullet reduces to "it
/// compiles".
///
/// # Why this is not a second segmenter
///
/// [`engine_linked`] stays `false`. `loader.ts` refuses any module that reports
/// `false` unless the caller passes the test-only `allowUnlinked`, so this code
/// is fenced by the same discipline as the TypeScript stub and by a stronger
/// mechanism: the stub is fenced by tree-shaking, this is fenced by a runtime
/// refusal that `conformance.ts` asserts fires. Every `resegment` still records
/// [`unlinked_diagnostic`] on top of the rows, so anything that does reach it
/// says so on every pass.
///
/// The one *segmentation algorithm* is still `stratum-parse`'s. This is an
/// approximation with a delete-by date: W11b removes [`mod reference`] in the
/// same commit that links `engine::ParseSegmenter`.
#[derive(Debug, Default)]
pub struct ReferenceSegmenter;

impl Segmenter for ReferenceSegmenter {
    fn resegment(&mut self, doc: &str, out: &mut Segmentation) {
        // The diagnostic goes first so that a webview showing only the first
        // diagnostic still shows the one that matters.
        out.push_diagnostic(unlinked_diagnostic());
        reference::segment(doc.as_bytes(), out);
    }
    fn tokens(&mut self, doc: &str, range: Range<usize>, out: &mut Vec<i32>) {
        reference::tokenize(doc.as_bytes(), range, out);
    }
    fn complete(&self, doc: &str, _env: &CompletionEnv, pos: usize) -> CompletionList {
        // No candidates, deliberately, where segmentation is deliberately
        // approximate: a candidate list needs W04b's command table and W20's
        // dataflow index, and inventing plausible-looking completions is the one
        // failure mode worse than offering none. An empty list still exercises
        // the JSON half of §14 and still carries the environment's truncation
        // (A11), which is stamped by `Engine::complete`, not by the backend.
        //
        // The *range* is not a candidate, though — it is the token under the
        // cursor, and it is offset arithmetic that crosses the wasm boundary. So
        // it is computed the same way `completeAt` in `stub/completion.ts`
        // computes it, and the generated sessions in `differential.ts` compare
        // the two. It reported `65..71` against `66..66` on the first document
        // with a word under the cursor, which is a coordinate bug hiding behind
        // an empty list.
        let token = reference::token_range(doc.as_bytes(), pos);
        CompletionList {
            from: token.start as u32,
            to: token.end as u32,
            ..CompletionList::default()
        }
    }
    fn quick_fixes(&self, _doc: &str, _pos: usize) -> Vec<Suggestion> {
        Vec::new()
    }
    fn lints(&self, _doc: &str) -> Vec<Diagnostic> {
        Vec::new()
    }
}

/// The naive line splitter behind [`ReferenceSegmenter`].
///
/// **This is not a segmenter.** It is a placeholder shaped like one. It gets
/// nested `///`, `#delimit ;` interacting with `/* */`, compound quotes and Mata
/// inside `program define` **wrong**, on purpose — the moment it looked almost
/// right, someone would try to ship it.
///
/// It is a transliteration of `apps/desktop/src/wasm/stub/naive.ts` and must
/// stay one: `conformance.ts` asserts the two produce identical regions, which
/// is the whole point of having it. Every function below has a same-named
/// counterpart there. Both work on UTF-8 **bytes** rather than characters,
/// because every structural character in Stata is ASCII and UTF-8 is
/// self-synchronising, so a byte scanner cannot disagree with the other side
/// about where an offset is.
mod reference {
    use std::collections::BTreeMap;
    use std::ops::Range;

    use stratum_proto::{
        BraceOpener, DirectiveKind, EndBlockOpener, RegionKind, TokenKind, Unterminated,
    };

    use super::{
        encode_kind, encode_token_kind, RegionRow, Segmentation, DELIM_CR, DELIM_SEMI,
        FLAG_ESTIMATION, FLAG_EXECUTABLE, FLAG_EXIT_SEMI, FLAG_MACRO_IN_HEAD, FLAG_SECTION_HEAD,
        NARRATIVE_BLOCK, NARRATIVE_LINE,
    };

    const LF: u8 = b'\n';
    const CR: u8 = b'\r';
    const TAB: u8 = b'\t';
    const SPACE: u8 = b' ';
    const SLASH: u8 = b'/';
    const STAR: u8 = b'*';
    const LBRACE: u8 = b'{';
    const RBRACE: u8 = b'}';
    const SEMI: u8 = b';';
    const PIPE: u8 = b'|';
    const PERCENT: u8 = b'%';
    const DQUOTE: u8 = b'"';
    const BACKTICK: u8 = b'`';
    const DOLLAR: u8 = b'$';
    const HASH: u8 = b'#';
    const DOT: u8 = b'.';

    /// Estimation commands the stub knows. A short, honest list: the real one is
    /// `stratum-effects`' registry, and a long list here would only make the
    /// approximation look more authoritative than it is.
    const ESTIMATION_WORDS: [&str; 13] = [
        "regress",
        "reg",
        "logit",
        "probit",
        "poisson",
        "areg",
        "ivregress",
        "xtreg",
        "mixed",
        "anova",
        "nbreg",
        "tobit",
        "heckman",
    ];

    fn is_space(b: u8) -> bool {
        b == SPACE || b == TAB || b == CR
    }

    fn is_word_byte(b: u8) -> bool {
        b.is_ascii_alphanumeric() || b == b'_'
    }

    /// The identifier-ish token the cursor sits in, as a byte range.
    ///
    /// The counterpart of `tokenAt` in `stub/completion.ts`, byte for byte. An
    /// accepted completion replaces the whole word under the cursor rather than
    /// only the text behind it, so the two backends have to agree about which
    /// bytes those are even when neither offers a candidate.
    pub fn token_range(buf: &[u8], pos: usize) -> Range<usize> {
        let at = pos.min(buf.len());
        let mut from = at;
        while from > 0 && is_word_byte(buf[from - 1]) {
            from -= 1;
        }
        let mut to = at;
        while to < buf.len() && is_word_byte(buf[to]) {
            to += 1;
        }
        from..to
    }

    /// The ASCII word starting at the first non-blank byte of `[from, to)`, with
    /// the offset it started at.
    fn first_word(buf: &[u8], from: usize, to: usize) -> (String, usize) {
        let to = to.min(buf.len());
        let mut i = from;
        while i < to && is_space(buf[i]) {
            i += 1;
        }
        let at = i;
        let mut word = String::new();
        // `#delimit` leads with a non-word byte, so the sigil joins the word.
        if i < to && buf[i] == HASH {
            word.push('#');
            i += 1;
        }
        while i < to && is_word_byte(buf[i]) {
            word.push(char::from(buf[i].to_ascii_lowercase()));
            i += 1;
        }
        (word, at)
    }

    /// End of the physical line containing `from`, before its LF.
    fn line_end(buf: &[u8], from: usize) -> usize {
        let mut i = from.min(buf.len());
        while i < buf.len() && buf[i] != LF {
            i += 1;
        }
        i
    }

    /// True when the line's last non-blank bytes are `///`.
    fn continues(buf: &[u8], from: usize, to: usize) -> bool {
        let mut i = to.min(buf.len());
        while i > from && is_space(buf[i - 1]) {
            i -= 1;
        }
        i - from >= 3 && buf[i - 1] == SLASH && buf[i - 2] == SLASH && buf[i - 3] == SLASH
    }

    /// Comment shape of a line that is nothing but a comment.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum CommentShape {
        /// The line carries code.
        None,
        /// Whitespace only.
        Blank,
        /// A whole-line `*` or `//` comment.
        Line,
        /// A `//|` narrative comment.
        NarrativeLine,
        /// A `%%` section marker, with the title's byte range.
        Section { title_from: usize, title_to: usize },
        /// A `/*` block comment opener; `narrative` for `/*md`.
        BlockOpen { narrative: bool },
    }

    fn classify_comment(buf: &[u8], from: usize, to: usize) -> CommentShape {
        let to = to.min(buf.len());
        let mut i = from;
        while i < to && is_space(buf[i]) {
            i += 1;
        }
        if i >= to {
            return CommentShape::Blank;
        }
        let b0 = buf[i];
        let b1 = if i + 1 < to { buf[i + 1] } else { 0 };
        let b2 = if i + 2 < to { buf[i + 2] } else { 0 };

        // `* …` is a comment only in command position, which is where we are.
        if b0 == STAR {
            return section_title(buf, i + 1, to).unwrap_or(CommentShape::Line);
        }
        if b0 == SLASH && b1 == SLASH {
            if i + 2 < to && b2 == PIPE {
                return CommentShape::NarrativeLine;
            }
            return section_title(buf, i + 2, to).unwrap_or(CommentShape::Line);
        }
        if b0 == SLASH && b1 == STAR {
            // `/*md` opens a narrative block; anything else is a plain comment.
            let narrative = i + 2 < to && b2 == b'm' && i + 3 < to && buf[i + 3] == b'd';
            return CommentShape::BlockOpen { narrative };
        }
        CommentShape::None
    }

    /// `%% Label` after a comment sigil — 06 §4.8's section marker.
    fn section_title(buf: &[u8], from: usize, to: usize) -> Option<CommentShape> {
        let mut i = from;
        while i < to && is_space(buf[i]) {
            i += 1;
        }
        if i + 1 >= to || buf[i] != PERCENT || buf[i + 1] != PERCENT {
            return None;
        }
        i += 2;
        while i < to && is_space(buf[i]) {
            i += 1;
        }
        let mut end = to;
        while end > i && is_space(buf[end - 1]) {
            end -= 1;
        }
        Some(CommentShape::Section {
            title_from: i,
            title_to: end,
        })
    }

    // --- hashing ------------------------------------------------------------
    //
    // A 32-bit FNV-1a run four times with different offset bases, packed into
    // the two 64-bit halves the flat row carries. It is not blake3 and does not
    // pretend to be: nothing compares one of these with an engine `CodeHash`.
    // What it must be is a stable, collision-shy function of the region's
    // canonical text, because the editor keys result cards on it between edits.

    const FNV_PRIME: u32 = 0x0100_0193;
    const FNV_BASES: [u32; 4] = [0x811c_9dc5, 0x0100_0193, 0x27d4_eb2f, 0x1656_67b1];

    fn fnv(buf: &[u8], from: usize, to: usize, basis: u32) -> u32 {
        let mut h = basis;
        let mut pending_space = false;
        let mut started = false;
        let mut i = from;
        let to = to.min(buf.len());
        while i < to {
            let b = buf[i];
            i += 1;
            if is_space(b) || b == LF {
                // Insignificant whitespace collapses, so re-indenting a block
                // does not orphan its result card.
                if started {
                    pending_space = true;
                }
                continue;
            }
            if pending_space {
                h = (h ^ u32::from(SPACE)).wrapping_mul(FNV_PRIME);
                pending_space = false;
            }
            started = true;
            h = (h ^ u32::from(b)).wrapping_mul(FNV_PRIME);
        }
        h
    }

    /// `(hash_lo, hash_hi)` in flat-row slot order. The names are the slots'
    /// (CONTRACTS §14 spells the u64 view `[hash_lo, hash_hi, hash_ordinal]`),
    /// not a claim about which half of some canonical 128-bit value they are —
    /// there is no canonical value here, only four independent 32-bit runs.
    fn hash_pair(buf: &[u8], from: usize, to: usize) -> (u64, u64) {
        let h: Vec<u64> = FNV_BASES
            .iter()
            .map(|basis| u64::from(fnv(buf, from, to, *basis)))
            .collect();
        ((h[0] << 32) | h[1], (h[2] << 32) | h[3])
    }

    // --- the splitter -------------------------------------------------------

    /// One region before outer spans and ordinals are assigned.
    struct Pending {
        code_from: usize,
        code_to: usize,
        kind: RegionKind,
        entry_delim: i32,
        exit_delim: i32,
        head_line: i32,
        last_line: i32,
        executable: bool,
        estimation: bool,
        macro_in_head: bool,
        section_head: bool,
    }

    /// A section marker before its span end is known.
    struct PendingSection {
        start: usize,
        id: i32,
        title: Range<usize>,
        line: i32,
    }

    /// Split `buf` into regions, appending everything to `out`.
    pub fn segment(buf: &[u8], out: &mut Segmentation) {
        let mut pending: Vec<Pending> = Vec::new();
        let mut sections: Vec<PendingSection> = Vec::new();
        let mut narrative: Vec<(usize, usize, i32)> = Vec::new();
        let mut section_id = 1;
        let mut delim = DELIM_CR;
        let mut pos = 0;
        let mut line = 0;

        while pos < buf.len() {
            let start_line = line;
            let start = pos;
            let end = line_end(buf, pos);
            let mut logical_end = end;

            let comment = classify_comment(buf, start, end);

            if matches!(
                comment,
                CommentShape::Blank | CommentShape::Line | CommentShape::Section { .. }
            ) {
                // Merge a run of consecutive blank/comment lines into one Trivia
                // region, so the gutter shows one inert band rather than a
                // stripe per line.
                let has_marker = matches!(comment, CommentShape::Section { .. });
                if let CommentShape::Section {
                    title_from,
                    title_to,
                } = comment
                {
                    sections.push(PendingSection {
                        start,
                        id: section_id,
                        title: title_from..title_to,
                        line: start_line,
                    });
                    section_id += 1;
                }
                let mut cursor = end;
                let mut cursor_line = line;
                while cursor < buf.len() {
                    let next_start = cursor + 1;
                    if next_start > buf.len() {
                        break;
                    }
                    let next_end = line_end(buf, next_start);
                    if matches!(
                        classify_comment(buf, next_start, next_end),
                        CommentShape::Blank | CommentShape::Line
                    ) {
                        cursor = next_end;
                        cursor_line += 1;
                        continue;
                    }
                    break;
                }
                pending.push(Pending {
                    code_from: start,
                    code_to: cursor,
                    kind: RegionKind::Trivia { has_marker },
                    entry_delim: delim,
                    exit_delim: delim,
                    head_line: start_line,
                    last_line: cursor_line,
                    executable: false,
                    estimation: false,
                    macro_in_head: false,
                    section_head: has_marker,
                });
                pos = cursor + 1;
                line = cursor_line + 1;
                continue;
            }

            if comment == CommentShape::NarrativeLine {
                let mut cursor = end;
                let mut cursor_line = line;
                while cursor < buf.len() {
                    let next_start = cursor + 1;
                    let next_end = line_end(buf, next_start);
                    if classify_comment(buf, next_start, next_end) != CommentShape::NarrativeLine {
                        break;
                    }
                    cursor = next_end;
                    cursor_line += 1;
                }
                narrative.push((start, cursor, NARRATIVE_LINE));
                pending.push(Pending {
                    code_from: start,
                    code_to: cursor,
                    kind: RegionKind::Trivia { has_marker: false },
                    entry_delim: delim,
                    exit_delim: delim,
                    head_line: start_line,
                    last_line: cursor_line,
                    executable: false,
                    estimation: false,
                    macro_in_head: false,
                    section_head: false,
                });
                pos = cursor + 1;
                line = cursor_line + 1;
                continue;
            }

            if let CommentShape::BlockOpen { narrative: is_md } = comment {
                let (close_at, close_lines) = find_block_close(buf, start);
                let stop = close_at.unwrap_or(buf.len());
                if is_md {
                    narrative.push((start, stop, NARRATIVE_BLOCK));
                }
                pending.push(Pending {
                    code_from: start,
                    code_to: stop,
                    kind: if close_at.is_some() {
                        RegionKind::Trivia { has_marker: false }
                    } else {
                        RegionKind::Unterminated {
                            expected: Unterminated::BlockComment,
                        }
                    },
                    entry_delim: delim,
                    exit_delim: delim,
                    head_line: start_line,
                    last_line: line + close_lines,
                    executable: false,
                    estimation: false,
                    macro_in_head: false,
                    section_head: false,
                });
                pos = stop + 1;
                line += close_lines + 1;
                continue;
            }

            // A code line. Fold `///` continuations, then `#delimit ;`
            // statements.
            let mut lines = 0;
            while continues(buf, start, logical_end) && logical_end < buf.len() {
                logical_end = line_end(buf, logical_end + 1);
                lines += 1;
            }
            if delim == DELIM_SEMI {
                while logical_end < buf.len() && !has_semicolon(buf, start, logical_end) {
                    logical_end = line_end(buf, logical_end + 1);
                    lines += 1;
                }
            }

            let (word, word_at) = first_word(buf, start, logical_end);
            let mut kind = RegionKind::Simple;
            let mut executable = true;
            let mut exit_delim = delim;

            if word == "#delimit" {
                // `#delimit` is 8 bytes; what follows decides the mode.
                let (arg, _) = first_word(buf, word_at + 8, logical_end);
                if arg == "cr" {
                    exit_delim = DELIM_CR;
                    kind = RegionKind::Directive {
                        directive: DirectiveKind::DelimitCr,
                    };
                } else if next_non_blank(buf, word_at + 8, logical_end) == i32::from(SEMI) {
                    exit_delim = DELIM_SEMI;
                    kind = RegionKind::Directive {
                        directive: DirectiveKind::DelimitSemi,
                    };
                } else {
                    kind = RegionKind::Directive {
                        directive: DirectiveKind::Other,
                    };
                }
            } else if let Some(opener) = end_block_opener(&word) {
                let (close_at, close_lines) = find_end(buf, logical_end);
                kind = if close_at.is_some() {
                    RegionKind::EndBlock { opener, name: None }
                } else {
                    RegionKind::Unterminated {
                        expected: Unterminated::End,
                    }
                };
                if let Some(at) = close_at {
                    logical_end = at;
                    lines += close_lines;
                } else {
                    logical_end = buf.len();
                    lines += close_lines;
                    executable = false;
                }
            } else {
                let depth = brace_delta(buf, start, logical_end);
                if depth > 0 {
                    let (close_at, close_lines) = find_brace_close(buf, logical_end, depth);
                    kind = if close_at.is_some() {
                        RegionKind::Brace {
                            opener: brace_opener(&word),
                        }
                    } else {
                        RegionKind::Unterminated {
                            expected: Unterminated::CloseBrace,
                        }
                    };
                    if let Some(at) = close_at {
                        logical_end = at;
                        lines += close_lines;
                    } else {
                        logical_end = buf.len();
                        lines += close_lines;
                        executable = false;
                    }
                }
            }

            let head_byte = next_non_blank(buf, start, logical_end);
            pending.push(Pending {
                code_from: word_at,
                code_to: trim_end(buf, start, logical_end),
                kind,
                entry_delim: delim,
                exit_delim,
                head_line: start_line,
                last_line: start_line + lines,
                executable,
                estimation: ESTIMATION_WORDS.contains(&word.as_str()),
                macro_in_head: head_byte == i32::from(BACKTICK) || head_byte == i32::from(DOLLAR),
                section_head: false,
            });
            delim = exit_delim;
            pos = logical_end + 1;
            line = start_line + lines + 1;
        }

        flatten(buf, &pending, &sections, &narrative, out);
    }

    fn end_block_opener(word: &str) -> Option<EndBlockOpener> {
        Some(match word {
            "program" => EndBlockOpener::Program,
            "input" => EndBlockOpener::Input,
            "mata" => EndBlockOpener::Mata,
            "python" => EndBlockOpener::Python,
            "java" => EndBlockOpener::Java,
            _ => return None,
        })
    }

    /// Commands that habitually open a brace block. A brace block opened by
    /// anything else is `Anonymous`, never `Other`: the stub cannot tell the
    /// difference and says the less specific thing.
    fn brace_opener(word: &str) -> BraceOpener {
        match word {
            "foreach" => BraceOpener::Foreach,
            "forvalues" | "forval" => BraceOpener::Forvalues,
            "while" => BraceOpener::While,
            "if" | "else" => BraceOpener::IfElseChain,
            "capture" | "cap" => BraceOpener::Capture,
            "quietly" | "qui" => BraceOpener::Quietly,
            "noisily" | "noi" => BraceOpener::Noisily,
            _ => BraceOpener::Anonymous,
        }
    }

    fn next_non_blank(buf: &[u8], from: usize, to: usize) -> i32 {
        let to = to.min(buf.len());
        let mut i = from;
        while i < to && is_space(buf[i]) {
            i += 1;
        }
        if i < to {
            i32::from(buf[i])
        } else {
            -1
        }
    }

    fn trim_end(buf: &[u8], from: usize, to: usize) -> usize {
        let mut i = to.min(buf.len());
        while i > from && (is_space(buf[i - 1]) || buf[i - 1] == LF) {
            i -= 1;
        }
        i
    }

    fn has_semicolon(buf: &[u8], from: usize, to: usize) -> bool {
        buf[from.min(buf.len())..to.min(buf.len())].contains(&SEMI)
    }

    fn brace_delta(buf: &[u8], from: usize, to: usize) -> i32 {
        let mut depth = 0;
        for &b in &buf[from.min(buf.len())..to.min(buf.len())] {
            if b == LBRACE {
                depth += 1;
            } else if b == RBRACE {
                depth -= 1;
            }
        }
        depth
    }

    fn find_brace_close(buf: &[u8], from: usize, depth: i32) -> (Option<usize>, i32) {
        let mut d = depth;
        let mut lines = 0;
        let mut i = from;
        while i < buf.len() {
            let b = buf[i];
            if b == LF {
                lines += 1;
            } else if b == LBRACE {
                d += 1;
            } else if b == RBRACE {
                d -= 1;
                if d == 0 {
                    return (Some(i + 1), lines);
                }
            }
            i += 1;
        }
        (None, lines)
    }

    fn find_block_close(buf: &[u8], from: usize) -> (Option<usize>, i32) {
        let mut lines = 0;
        let mut i = from + 2;
        while i + 1 < buf.len() {
            if buf[i] == LF {
                lines += 1;
            }
            if buf[i] == STAR && buf[i + 1] == SLASH {
                return (Some(i + 2), lines);
            }
            i += 1;
        }
        (None, lines)
    }

    fn find_end(buf: &[u8], from: usize) -> (Option<usize>, i32) {
        let mut lines = 0;
        let mut pos = from;
        while pos < buf.len() {
            let start = pos + 1;
            if start > buf.len() {
                break;
            }
            let end = line_end(buf, start);
            lines += 1;
            if first_word(buf, start, end).0 == "end" {
                return (Some(end), lines);
            }
            pos = end;
        }
        (None, lines)
    }

    /// Turn the pending list into the flat rows the raw surface exposes.
    ///
    /// Outer spans are assigned here, and they **tile the document exactly**:
    /// each region's outer runs from the previous region's outer end to the
    /// start of the next region, with the last one reaching the end of the
    /// buffer. CONTRACTS §2 makes that tiling normative, and the editor's "which
    /// region is the cursor in" lookup is a binary search that assumes it.
    fn flatten(
        buf: &[u8],
        pending: &[Pending],
        sections: &[PendingSection],
        narrative: &[(usize, usize, i32)],
        out: &mut Segmentation,
    ) {
        let mut ordinals: BTreeMap<(u64, u64), u64> = BTreeMap::new();
        let mut outer_from = 0;

        for (i, p) in pending.iter().enumerate() {
            let outer_to = pending.get(i + 1).map_or(buf.len(), |next| next.code_from);
            let (hash_lo, hash_hi) = hash_pair(buf, p.code_from, p.code_to);
            let ordinal = ordinals.entry((hash_lo, hash_hi)).or_insert(0);
            let hash_ordinal = *ordinal;
            *ordinal += 1;

            let mut flags = 0;
            if p.executable {
                flags |= FLAG_EXECUTABLE;
            }
            if p.estimation {
                flags |= FLAG_ESTIMATION;
            }
            if p.macro_in_head {
                flags |= FLAG_MACRO_IN_HEAD;
            }
            if p.exit_delim == DELIM_SEMI {
                flags |= FLAG_EXIT_SEMI;
            }
            if p.section_head {
                flags |= FLAG_SECTION_HEAD;
            }

            out.push(&RegionRow {
                span: p.code_from as u32..p.code_to as u32,
                outer: outer_from as u32..outer_to as u32,
                kind: encode_kind(&p.kind),
                entry_delim: p.entry_delim,
                head_line: p.head_line,
                last_line: p.last_line,
                flags,
                hash_lo,
                hash_hi,
                hash_ordinal,
            });
            outer_from = outer_to;
        }

        // Sections run to the start of the next section, or to EOF.
        for (i, s) in sections.iter().enumerate() {
            let end = sections.get(i + 1).map_or(buf.len(), |next| next.start);
            out.push_section(
                s.start as u32..end as u32,
                s.id,
                s.title.start as u32..s.title.end as u32,
                s.line,
            );
        }

        for &(from, to, kind) in narrative {
            out.push_narrative(from as u32..to as u32, kind);
        }
    }

    // --- tokens -------------------------------------------------------------

    /// Naive tokens overlapping `range`, as flat `[from, to, tag]` triples.
    pub fn tokenize(buf: &[u8], range: Range<usize>, out: &mut Vec<i32>) {
        let mut i = range.start;
        let end = range.end.min(buf.len());
        while i < end {
            let b = buf[i];
            let start = i;
            let next = buf.get(i + 1).copied();

            if is_space(b) || b == LF {
                while i < end && (is_space(buf[i]) || buf[i] == LF) {
                    i += 1;
                }
                push(out, start, i, TokenKind::Whitespace);
                continue;
            }
            if b == SLASH && next == Some(SLASH) {
                while i < end && buf[i] != LF {
                    i += 1;
                }
                push(out, start, i, TokenKind::Comment);
                continue;
            }
            if b == SLASH && next == Some(STAR) {
                i += 2;
                while i + 1 < end && !(buf[i] == STAR && buf[i + 1] == SLASH) {
                    i += 1;
                }
                i = (i + 2).min(end);
                push(out, start, i, TokenKind::Comment);
                continue;
            }
            if b == DQUOTE {
                i += 1;
                while i < end && buf[i] != DQUOTE && buf[i] != LF {
                    i += 1;
                }
                i = (i + 1).min(end);
                push(out, start, i, TokenKind::StrLit);
                continue;
            }
            if b == BACKTICK || b == DOLLAR {
                i += 1;
                while i < end && (is_word_byte(buf[i]) || buf[i] == LBRACE || buf[i] == RBRACE) {
                    i += 1;
                }
                if i < end && buf[i] == b'\'' {
                    i += 1;
                }
                push(out, start, i, TokenKind::MacroRef);
                continue;
            }
            if b.is_ascii_digit() || (b == DOT && next.is_some_and(|n| n.is_ascii_digit())) {
                while i < end && (buf[i].is_ascii_digit() || buf[i] == DOT) {
                    i += 1;
                }
                push(out, start, i, TokenKind::Number);
                continue;
            }
            if is_word_byte(b) {
                while i < end && is_word_byte(buf[i]) {
                    i += 1;
                }
                push(out, start, i, TokenKind::Ident);
                continue;
            }
            i += 1;
            push(out, start, i, punctuation(b));
        }
    }

    fn push(out: &mut Vec<i32>, from: usize, to: usize, kind: TokenKind) {
        out.extend_from_slice(&[from as i32, to as i32, encode_token_kind(kind)]);
    }

    fn punctuation(b: u8) -> TokenKind {
        match b {
            b',' => TokenKind::Comma,
            b':' => TokenKind::Colon,
            b'(' => TokenKind::LParen,
            b')' => TokenKind::RParen,
            LBRACE => TokenKind::LBrace,
            RBRACE => TokenKind::RBrace,
            b'[' => TokenKind::LBracket,
            b']' => TokenKind::RBracket,
            SEMI => TokenKind::StatementBreak,
            HASH => TokenKind::Directive,
            _ => TokenKind::Op,
        }
    }
}

// ---------------------------------------------------------------------------
// THE SEAM. W11a wrote the two items below and reserved this edit for W11b:
//
//     mod engine;
//     type Backend = engine::ParseSegmenter;
//     const ENGINE_LINKED: bool = true;
//
// W11b has now made it. The four modules beside `engine` are the rest of W11b's
// half of the crate (`docs/ownership.toml`): the flat-row projection, the token
// projection, the completer, and the A11 bound on the environment.
//
// `ReferenceSegmenter` and `mod reference` are KEPT rather than deleted, which
// is the one place W11b departs from the note W11a left here. They are no
// longer reachable from `Engine` — nothing but this `type` alias ever named
// them — so the "one segmentation algorithm" rule holds on the shipped path,
// and they remain what six of this file's own tests exercise and what
// `conformance.ts` documents as the same-rule counterpart of
// `stub/naive.ts`. Deleting them is W11a's call on W11a's file, not a change
// W11b should make while another agent may be editing it. See W11b's return.
// ---------------------------------------------------------------------------

mod complete;
mod engine;
mod regions;
mod tokens;

/// A11's read-side bound on `CompletionEnv` (W11b).
///
/// Public because it is where §14's 2 ms completion contract is expressed as a
/// counter (ADR-017), and `crates/stratum-wasm/tests/parity.rs` — a separate
/// crate, and the same file that runs under `wasm-bindgen-test` — is what
/// asserts it.
pub mod env;

// The three items W11b's gates need from OUTSIDE this crate. `tests/parity.rs`
// and `benches/resegment.rs` are separate crates and the same `parity.rs` also
// runs under `wasm-bindgen-test`, so neither can reach a private module or a
// private field. Re-exporting exactly these three keeps the modules themselves
// private — nothing else in the crate's public surface changes.
pub use engine::{ParseSegmenter, PassStats};
pub use regions::golden_json;

/// The segmentation backend this build links.
type Backend = engine::ParseSegmenter;

/// Whether [`Backend`] is the real segmenter. Reported by [`engine_linked`].
const ENGINE_LINKED: bool = true;

// ===========================================================================
// The document buffer.
// ===========================================================================

/// Document text plus the scratch buffer JS writes edits into.
///
/// Splitting scratch from the document is what makes an edit one `TextEncoder`
/// write into linear memory instead of a string copy across the boundary.
#[derive(Debug, Default)]
pub struct Doc {
    text: String,
    scratch: Vec<u8>,
}

/// Why a [`Doc::splice`] was rejected. Every variant means "JS and wasm disagree
/// about the document", which is a bug in the caller, so each one becomes a
/// diagnostic the webview can surface rather than a silent no-op.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SpliceError {
    /// `from > to`, or `to` past the end of the document.
    Range,
    /// `from` or `to` fell inside a multi-byte character.
    NotCharBoundary,
    /// `src .. src + len` is not inside the scratch buffer.
    Scratch,
    /// The inserted bytes are not valid UTF-8.
    NotUtf8,
}

impl SpliceError {
    /// Stable diagnostic code, from the `PARSE`/`L` registry's wasm range.
    const fn code(self) -> &'static str {
        match self {
            SpliceError::Range => "WASM0001",
            SpliceError::NotCharBoundary => "WASM0002",
            SpliceError::Scratch => "WASM0003",
            SpliceError::NotUtf8 => "WASM0004",
        }
    }

    const fn message(self) -> &'static str {
        match self {
            SpliceError::Range => "splice range is outside the document",
            SpliceError::NotCharBoundary => "splice range splits a character",
            SpliceError::Scratch => "splice source is outside the scratch buffer",
            SpliceError::NotUtf8 => "spliced bytes are not valid UTF-8",
        }
    }
}

impl Doc {
    /// Grow the scratch buffer to at least `bytes` and return a pointer to it.
    ///
    /// The pointer is invalidated by the next `reserve`, so JS must rebuild its
    /// `Uint8Array` after every call — wasm memory growth relocates the whole
    /// heap and any view built before the call would silently read the old page.
    pub fn reserve(&mut self, bytes: usize) -> *mut u8 {
        if self.scratch.len() < bytes {
            self.scratch.resize(bytes, 0);
        }
        self.scratch.as_mut_ptr()
    }

    /// Replace `text[from..to)` with `scratch[src..src + len)`.
    ///
    /// # Errors
    /// Returns without touching the document if the request is inconsistent; see
    /// [`SpliceError`].
    pub fn splice(
        &mut self,
        from: usize,
        to: usize,
        src: usize,
        len: usize,
    ) -> Result<(), SpliceError> {
        if from > to || to > self.text.len() {
            return Err(SpliceError::Range);
        }
        if !self.text.is_char_boundary(from) || !self.text.is_char_boundary(to) {
            return Err(SpliceError::NotCharBoundary);
        }
        let end = src.checked_add(len).ok_or(SpliceError::Scratch)?;
        let bytes = self.scratch.get(src..end).ok_or(SpliceError::Scratch)?;
        let insert = std::str::from_utf8(bytes).map_err(|_| SpliceError::NotUtf8)?;
        self.text.replace_range(from..to, insert);
        Ok(())
    }

    /// The document.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Length in bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.text.len()
    }

    /// True for an empty document.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }
}

// ===========================================================================
// The `#[wasm_bindgen]` surface — CONTRACTS §14.
// ===========================================================================

/// The per-document segmentation engine, one per open editor.
///
/// Runs on the **main thread**, synchronously, inside the CodeMirror transaction
/// cycle (06 §3): a worker would reintroduce the frame lag the whole design
/// exists to delete.
#[wasm_bindgen]
pub struct Engine {
    doc: Doc,
    seg: Segmentation,
    tokens: Vec<i32>,
    env: CompletionEnv,
    backend: Backend,
    generation: u32,
    /// Set by `splice`, cleared by `resegment`. Without it, a `resegment` per
    /// transaction on an unchanged document would burn the whole budget.
    dirty: bool,
    /// Splice and env-decode failures. Surfaced through `diagnostics()`.
    faults: Vec<Diagnostic>,
}

impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}

#[wasm_bindgen]
impl Engine {
    /// A fresh engine over an empty document, generation 0.
    #[wasm_bindgen(constructor)]
    #[must_use]
    pub fn new() -> Engine {
        Engine {
            doc: Doc::default(),
            seg: Segmentation::default(),
            tokens: Vec::new(),
            env: CompletionEnv::default(),
            backend: Backend::default(),
            generation: 0,
            // A fresh engine has an empty document and an empty segmentation,
            // which already agree. The first splice makes it dirty.
            dirty: false,
            faults: Vec::new(),
        }
    }

    /// Pointer into wasm memory for JS to write UTF-8 into. Grows on demand.
    ///
    /// See [`Doc::reserve`]: the returned pointer is valid until the next call.
    pub fn reserve(&mut self, bytes: u32) -> *mut u8 {
        self.doc.reserve(bytes as usize)
    }

    /// Apply one CM6 change: replace `[from, to)` with `len` bytes already
    /// written at `src` in the scratch buffer.
    ///
    /// Offsets are UTF-8 byte offsets. A rejected splice records a diagnostic and
    /// leaves the document unchanged rather than unwinding into the transaction.
    pub fn splice(&mut self, from: u32, to: u32, src: u32, len: u32) {
        match self
            .doc
            .splice(from as usize, to as usize, src as usize, len as usize)
        {
            Ok(()) => self.dirty = true,
            Err(e) => self.faults.push(fault(e)),
        }
    }

    /// Re-segment. Returns the generation, which increments only when the
    /// document actually changed — an unchanged document costs one branch.
    ///
    /// Budget: < 150 µs incremental, 3–8 ms for a cold 10 k-line pass.
    pub fn resegment(&mut self) -> u32 {
        if !self.dirty {
            return self.generation;
        }
        self.seg.clear();
        self.backend.resegment(self.doc.text(), &mut self.seg);
        self.tokens.clear();
        self.dirty = false;
        self.generation = self.generation.wrapping_add(1);
        self.generation
    }

    /// The current generation without re-segmenting.
    #[must_use]
    pub fn generation(&self) -> u32 {
        self.generation
    }

    /// Number of regions in the current segmentation.
    #[must_use]
    pub fn region_count(&self) -> u32 {
        self.seg.len() as u32
    }

    /// Flat `i32` view, [`REGION_STRIDE`] per region.
    #[must_use]
    pub fn regions_view(&self) -> js_sys::Int32Array {
        js_sys::Int32Array::from(&self.seg.rows[..])
    }

    /// Flat `u64` view, [`REGION_HASH_STRIDE`] per region.
    ///
    /// **THESE ARE HASHES, NOT IDENTITIES.** `BlockId` comes from the engine.
    #[must_use]
    pub fn region_hashes(&self) -> js_sys::BigUint64Array {
        js_sys::BigUint64Array::from(&self.seg.hashes[..])
    }

    /// Flat `i32` triples `[from, to, tag]` for the requested byte range only.
    ///
    /// Scoped to the visible range because a 10 k-line file has ~8 k tokens per
    /// screen and materialising the whole document's stream would cost more than
    /// the parse (06 §3.4).
    pub fn tokens(&mut self, from: u32, to: u32) -> js_sys::Int32Array {
        self.tokens.clear();
        let doc = self.doc.text();
        let from = (from as usize).min(doc.len());
        let to = (to as usize).clamp(from, doc.len());
        self.backend.tokens(doc, from..to, &mut self.tokens);
        js_sys::Int32Array::from(&self.tokens[..])
    }

    /// Flat `i32` view, [`SECTION_STRIDE`] per section.
    #[must_use]
    pub fn sections(&self) -> js_sys::Int32Array {
        js_sys::Int32Array::from(&self.seg.sections[..])
    }

    /// Flat `i32` view, [`NARRATIVE_STRIDE`] per region — `//|` and `/*md`.
    #[must_use]
    pub fn narrative_regions(&self) -> js_sys::Int32Array {
        js_sys::Int32Array::from(&self.seg.narrative[..])
    }

    /// Parse diagnostics plus any splice faults. Rare; JSON is fine (§14).
    ///
    /// Faults are drained: a splice error is reported once, to the transaction
    /// that caused it.
    pub fn diagnostics(&mut self) -> JsValue {
        let mut all = std::mem::take(&mut self.faults);
        all.extend_from_slice(&self.seg.diagnostics);
        to_js(&all)
    }

    /// Set the live environment pushed by the engine on `StateChanged`.
    ///
    /// Takes the engine's own msgpack bytes (§9/§10). A malformed payload keeps
    /// the previous environment — completing against a stale variable list is a
    /// far smaller failure than a popup that stops working.
    pub fn set_completion_env(&mut self, msgpack: &[u8]) {
        match rmp_serde::from_slice::<CompletionEnv>(msgpack) {
            Ok(env) => self.env = env,
            Err(e) => self.faults.push(Diagnostic {
                severity: Severity::Warning,
                code: "WASM0005".to_owned(),
                stata_rc: None,
                message: format!("completion environment could not be decoded: {e}"),
                file: None,
                span: None,
                offending_token: None,
                block: None,
                related: Vec::new(),
                suggestions: Vec::new(),
                notes: Vec::new(),
                confidence: Confidence::Exact,
            }),
        }
    }

    /// The generation of the environment currently loaded, so the webview can
    /// tell whether a `StateChanged` it just saw has been applied.
    #[must_use]
    pub fn completion_env_generation(&self) -> u64 {
        self.env.generation
    }

    /// Deterministic completion. HARD CONTRACT: < 2 ms, criterion-benched in CI.
    ///
    /// Truncation is stamped here rather than left to the backend: A11 is a
    /// property of the ENVIRONMENT the engine shed entries from, not of the
    /// candidate list, and a backend that forgot to propagate it would silently
    /// tell the user that 2 048 variables are all the variables there are.
    #[must_use]
    pub fn complete(&self, pos: u32) -> JsValue {
        let doc = self.doc.text();
        let pos = (pos as usize).min(doc.len());
        let mut list = self.backend.complete(doc, &self.env, pos);
        stamp_truncation(&mut list, &self.env);
        to_js(&list)
    }

    /// Deterministic quick fixes at `pos`, as frozen `Suggestion`s.
    #[must_use]
    pub fn quick_fixes(&self, pos: u32) -> JsValue {
        let doc = self.doc.text();
        let pos = (pos as usize).min(doc.len());
        to_js(&self.backend.quick_fixes(doc, pos))
    }

    /// Whole-document lints that need no session state, as frozen
    /// `Diagnostic`s. Lints that need live state come from the engine.
    #[must_use]
    pub fn lints(&self) -> JsValue {
        to_js(&self.backend.lints(self.doc.text()))
    }

    /// The document as JS sees it. Test and debug affordance — the editor is
    /// authoritative for text, never this buffer (06 §2, rule 2).
    #[must_use]
    pub fn doc_text(&self) -> String {
        self.doc.text().to_owned()
    }

    /// Document length in bytes. The webview asserts it against its own encoded
    /// length after each transaction; a mismatch means the two buffers have
    /// diverged and the wrapper resynchronises with a full replace.
    #[must_use]
    pub fn doc_len(&self) -> u32 {
        self.doc.len() as u32
    }
}

/// Version of the flat view layout this module was built with. `loader.ts`
/// refuses a module whose value differs from its own.
#[wasm_bindgen]
#[must_use]
pub fn abi_version() -> u32 {
    WASM_ABI
}

/// Whether a real segmenter is linked.
///
/// False for a harness-only build, which produces no regions at all. The loader
/// treats false as fatal in production and as "fall back to the fenced stub" in
/// development; without this, an unlinked module would look exactly like an
/// empty document.
#[wasm_bindgen]
#[must_use]
pub fn engine_linked() -> bool {
    ENGINE_LINKED
}

/// Install the panic hook, when this build has one. Called by wasm-bindgen at
/// module instantiation.
#[wasm_bindgen(start)]
pub fn start() {
    #[cfg(feature = "panic-hook")]
    console_error_panic_hook::set_once();
}

// ===========================================================================
// Helpers.
// ===========================================================================

/// Serialise for the JSON-shaped half of §14.
///
/// `serde_wasm_bindgen` builds JS values directly; `JSON.parse(to_string())`
/// would allocate a string per call on a path that already has a budget.
///
/// `serialize_missing_as_null` is the load-bearing part. By default a `None`
/// arrives in JS as `undefined`, and `types.ts` declares every one of these
/// fields as `T | null` — so `d.span === null` was true through the development
/// stub and false through this module, which is precisely the difference the
/// editor is not allowed to be able to see. The generated sessions in
/// `differential.ts` found it on the first document that produced a diagnostic.
fn to_js<T: Serialize>(value: &T) -> JsValue {
    let serializer = serde_wasm_bindgen::Serializer::new().serialize_missing_as_null(true);
    value.serialize(&serializer).unwrap_or(JsValue::NULL)
}

/// A `u32` offset as the `i32` the flat views carry.
///
/// Saturating rather than wrapping: a document at 2 GB is already in Large File
/// Mode (06 §3.3) and a negative offset in a CM6 decoration range throws, which
/// would take the editor down instead of degrading it.
fn i32_of(v: u32) -> i32 {
    i32::try_from(v).unwrap_or(i32::MAX)
}

/// Copy the environment's truncation onto a candidate list.
///
/// Split out of [`Engine::complete`] so it is testable natively: `complete`
/// itself ends in a `JsValue`, which only exists inside a JS runtime.
fn stamp_truncation(list: &mut CompletionList, env: &CompletionEnv) {
    if env.truncated {
        list.truncated = true;
        list.offered = env.varnames.len() as u32;
        list.total = env.var_total;
    }
}

fn fault(e: SpliceError) -> Diagnostic {
    Diagnostic {
        severity: Severity::Error,
        code: e.code().to_owned(),
        stata_rc: None,
        message: e.message().to_owned(),
        file: None,
        span: None,
        offending_token: None,
        block: None,
        related: Vec::new(),
        suggestions: Vec::new(),
        notes: Vec::new(),
        confidence: Confidence::Exact,
    }
}

fn unlinked_diagnostic() -> Diagnostic {
    Diagnostic {
        severity: Severity::Error,
        code: "WASM0006".to_owned(),
        stata_rc: None,
        message: "this stratum-wasm build has no segmenter linked; \
                  block segmentation is unavailable"
            .to_owned(),
        file: None,
        span: None,
        offending_token: None,
        block: None,
        related: Vec::new(),
        suggestions: Vec::new(),
        notes: vec!["build with the real backend (W11b) or run the fenced \
                     development stub"
            .to_owned()],
        confidence: Confidence::Exact,
    }
}

// ===========================================================================
// Tests. Native only: everything here is the harness, which is exactly the part
// that does not need a JS engine to be wrong.
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Write `s` into the scratch buffer the way JS does, and splice it in.
    fn splice_str(doc: &mut Doc, from: usize, to: usize, s: &str) -> Result<(), SpliceError> {
        let ptr = doc.reserve(s.len().max(1));
        // The pointer round-trip is what JS does; here it is a plain slice write.
        let _ = ptr;
        doc.scratch[..s.len()].copy_from_slice(s.as_bytes());
        doc.splice(from, to, 0, s.len())
    }

    #[test]
    fn splice_builds_a_document() {
        let mut doc = Doc::default();
        splice_str(&mut doc, 0, 0, "sysuse auto\n").unwrap();
        splice_str(&mut doc, 12, 12, "summarize price\n").unwrap();
        assert_eq!(doc.text(), "sysuse auto\nsummarize price\n");
        // Replace `price` with `mpg`.
        let from = doc.text().find("price").unwrap();
        splice_str(&mut doc, from, from + 5, "mpg").unwrap();
        assert_eq!(doc.text(), "sysuse auto\nsummarize mpg\n");
    }

    #[test]
    fn splice_rejects_a_split_character() {
        let mut doc = Doc::default();
        splice_str(&mut doc, 0, 0, "label var x \"café\"").unwrap();
        // `é` occupies bytes 16..18, so 17 is inside it. This is the failure a
        // UTF-16-counting editor produces when the offset conversion is wrong.
        let e_start = doc.text().find('é').unwrap();
        assert_eq!(e_start, 16);
        assert_eq!(
            splice_str(&mut doc, e_start + 1, e_start + 2, "x"),
            Err(SpliceError::NotCharBoundary)
        );
        assert_eq!(doc.text(), "label var x \"café\"");
    }

    #[test]
    fn splice_rejects_bad_utf8_without_touching_the_document() {
        let mut doc = Doc::default();
        splice_str(&mut doc, 0, 0, "list").unwrap();
        doc.reserve(2);
        doc.scratch[0] = 0xff;
        doc.scratch[1] = 0xfe;
        assert_eq!(doc.splice(4, 4, 0, 2), Err(SpliceError::NotUtf8));
        assert_eq!(doc.text(), "list");
    }

    #[test]
    fn splice_rejects_out_of_range() {
        let mut doc = Doc::default();
        splice_str(&mut doc, 0, 0, "list").unwrap();
        assert_eq!(doc.splice(0, 99, 0, 0), Err(SpliceError::Range));
        assert_eq!(doc.splice(3, 1, 0, 0), Err(SpliceError::Range));
        assert_eq!(doc.splice(0, 0, 5, 1), Err(SpliceError::Scratch));
    }

    #[test]
    fn reserve_never_shrinks_the_scratch_buffer() {
        let mut doc = Doc::default();
        doc.reserve(4096);
        doc.reserve(8);
        assert!(
            doc.scratch.len() >= 4096,
            "a smaller reserve must not shrink"
        );
    }

    #[test]
    fn kind_codec_round_trips_every_variant() {
        let all = [
            RegionKind::Simple,
            RegionKind::Brace {
                opener: BraceOpener::Foreach,
            },
            RegionKind::Brace {
                opener: BraceOpener::Forvalues,
            },
            RegionKind::Brace {
                opener: BraceOpener::While,
            },
            RegionKind::Brace {
                opener: BraceOpener::IfElseChain,
            },
            RegionKind::Brace {
                opener: BraceOpener::Capture,
            },
            RegionKind::Brace {
                opener: BraceOpener::Quietly,
            },
            RegionKind::Brace {
                opener: BraceOpener::Noisily,
            },
            RegionKind::Brace {
                opener: BraceOpener::Anonymous,
            },
            RegionKind::Brace {
                opener: BraceOpener::Other,
            },
            RegionKind::EndBlock {
                opener: EndBlockOpener::Program,
                name: None,
            },
            RegionKind::EndBlock {
                opener: EndBlockOpener::Input,
                name: None,
            },
            RegionKind::EndBlock {
                opener: EndBlockOpener::Mata,
                name: None,
            },
            RegionKind::EndBlock {
                opener: EndBlockOpener::Python,
                name: None,
            },
            RegionKind::EndBlock {
                opener: EndBlockOpener::Java,
                name: None,
            },
            RegionKind::Directive {
                directive: DirectiveKind::DelimitCr,
            },
            RegionKind::Directive {
                directive: DirectiveKind::DelimitSemi,
            },
            RegionKind::Directive {
                directive: DirectiveKind::Other,
            },
            RegionKind::Trivia { has_marker: false },
            RegionKind::Trivia { has_marker: true },
            RegionKind::Unterminated {
                expected: Unterminated::CloseBrace,
            },
            RegionKind::Unterminated {
                expected: Unterminated::End,
            },
            RegionKind::Unterminated {
                expected: Unterminated::BlockComment,
            },
            RegionKind::Unterminated {
                expected: Unterminated::CompoundQuote,
            },
        ];
        let mut seen = Vec::new();
        for kind in &all {
            let code = encode_kind(kind);
            assert!(
                !seen.contains(&code),
                "duplicate kind code {code} for {kind:?}"
            );
            seen.push(code);
            assert_eq!(
                decode_kind(code).as_ref(),
                Some(kind),
                "round trip {kind:?}"
            );
        }
    }

    #[test]
    fn token_tag_codec_is_exhaustive_and_round_trips() {
        // Exhaustive by construction: adding a `TokenKind` variant without a row
        // in TOKEN_TAGS fails to compile here, which is the whole point of
        // writing the table out instead of deriving it from declaration order.
        let all = [
            TokenKind::Ident,
            TokenKind::Number,
            TokenKind::StrLit,
            TokenKind::CompoundQuote,
            TokenKind::MacroRef,
            TokenKind::Op,
            TokenKind::Comma,
            TokenKind::Colon,
            TokenKind::LParen,
            TokenKind::RParen,
            TokenKind::LBrace,
            TokenKind::RBrace,
            TokenKind::LBracket,
            TokenKind::RBracket,
            TokenKind::Comment,
            TokenKind::Whitespace,
            TokenKind::StatementBreak,
            TokenKind::Continuation,
            TokenKind::Directive,
            TokenKind::Unknown,
        ];
        assert_eq!(all.len(), TOKEN_TAGS.len());
        for k in all {
            assert_eq!(decode_token_kind(encode_token_kind(k)), Some(k));
        }
        assert_eq!(decode_token_kind(-1), None);
        assert_eq!(decode_token_kind(TOKEN_TAGS.len() as i32), None);
        // Pinned: these are the tags apps/desktop/src/wasm/types.ts declares.
        assert_eq!(encode_token_kind(TokenKind::Ident), 0);
        assert_eq!(encode_token_kind(TokenKind::Comment), 14);
        assert_eq!(encode_token_kind(TokenKind::Unknown), 19);
    }

    #[test]
    fn decode_kind_rejects_codes_from_the_future() {
        assert_eq!(decode_kind(99 << FAMILY_SHIFT), None);
        assert_eq!(decode_kind((FAMILY_BRACE << FAMILY_SHIFT) | 200), None);
    }

    #[test]
    fn the_endblock_name_is_not_in_the_flat_row() {
        // Encoding is name-insensitive on purpose: the row is 9 i32s and the
        // webview slices a name out of the document when it wants one.
        let named = RegionKind::EndBlock {
            opener: EndBlockOpener::Program,
            name: Some("mysum".to_owned()),
        };
        let anon = RegionKind::EndBlock {
            opener: EndBlockOpener::Program,
            name: None,
        };
        assert_eq!(encode_kind(&named), encode_kind(&anon));
    }

    #[test]
    fn segmentation_rows_are_flattened_in_contract_order() {
        let mut seg = Segmentation::default();
        seg.push(&RegionRow {
            span: 4..17,
            outer: 0..18,
            kind: encode_kind(&RegionKind::Simple),
            entry_delim: DELIM_SEMI,
            head_line: 2,
            last_line: 3,
            flags: FLAG_EXECUTABLE | FLAG_ESTIMATION,
            hash_lo: 0x0123_4567_89ab_cdef,
            hash_hi: 0xfedc_ba98_7654_3210,
            hash_ordinal: 7,
        });
        assert_eq!(seg.len(), 1);
        assert_eq!(
            seg.rows,
            vec![
                4,
                17,
                0,
                18,
                0,
                DELIM_SEMI,
                2,
                3,
                FLAG_EXECUTABLE | FLAG_ESTIMATION
            ]
        );
        assert_eq!(
            seg.hashes,
            vec![0x0123_4567_89ab_cdef, 0xfedc_ba98_7654_3210, 7]
        );
        assert_eq!(seg.rows.len() % REGION_STRIDE, 0);
        assert_eq!(seg.hashes.len() % REGION_HASH_STRIDE, 0);
    }

    #[test]
    fn generation_advances_only_on_a_real_change() {
        let mut e = Engine::new();
        assert_eq!(
            e.resegment(),
            0,
            "a clean engine does not burn a generation"
        );
        let ptr = e.reserve(8);
        let _ = ptr;
        e.doc.scratch[..4].copy_from_slice(b"list");
        e.splice(0, 0, 0, 4);
        assert_eq!(e.resegment(), 1);
        assert_eq!(e.resegment(), 1, "no edit, no generation");
        assert_eq!(e.doc_text(), "list");
        assert_eq!(e.doc_len(), 4);
    }

    #[test]
    fn a_bad_splice_is_a_diagnostic_not_a_panic() {
        let mut e = Engine::new();
        e.splice(0, 99, 0, 0);
        assert_eq!(e.faults.len(), 1);
        assert_eq!(e.faults[0].code, "WASM0001");
        assert_eq!(e.doc_len(), 0);
        assert_eq!(e.resegment(), 0, "a rejected splice does not dirty the doc");
    }

    /// W11a wrote this as `an_unlinked_build_reports_itself`, a tripwire that
    /// fired the instant W11b flipped [`ENGINE_LINKED`]. It is now its dual, and
    /// it asserts both halves of what that flip means.
    ///
    /// `Engine` reaches `stratum-parse` and therefore stops stamping the
    /// approximation's WASM0006 on every pass, so `loader.ts` accepts the module
    /// without `allowUnlinked`. And [`ReferenceSegmenter`], reached directly,
    /// still says what it is — the diagnostic did not disappear, it stopped
    /// being reachable from the shipped path.
    #[test]
    fn a_linked_build_reports_itself() {
        assert!(
            engine_linked(),
            "ENGINE_LINKED must follow `Backend`; they are two lines apart"
        );
        let mut e = Engine::new();
        e.reserve(8);
        e.doc.scratch[..4].copy_from_slice(b"list");
        e.splice(0, 0, 0, 4);
        e.resegment();
        assert!(
            e.seg.diagnostics.iter().all(|d| d.code != "WASM0006"),
            "a linked build is still stamping the approximation's diagnostic"
        );
        assert_eq!(e.region_count(), 1, "a one-line document is one region");

        let mut seg = Segmentation::default();
        ReferenceSegmenter.resegment("list\n", &mut seg);
        assert_eq!(seg.diagnostics[0].code, "WASM0006");
        assert_eq!(seg.diagnostics[0].severity, Severity::Error);
    }

    /// The one structural promise the flat rows make to the editor, checked
    /// natively so it does not depend on the JS harness being run.
    ///
    /// `regionAt` in `segmenter.ts` is a binary search over `outer`; a gap or an
    /// overlap makes it return the wrong region rather than none, which is how a
    /// result card ends up attached to the block above the one that produced it.
    #[test]
    fn the_reference_rows_tile_the_document() {
        let doc = "// %% Load\nsysuse auto, clear\n\n// %% Model\nforeach v of varlist price mpg {\n    summarize `v'\n}\nregress price mpg weight\n\nprogram define mysum\n    display 1\nend\n";
        let mut seg = Segmentation::default();
        ReferenceSegmenter.resegment(doc, &mut seg);

        assert!(
            seg.len() > 1,
            "a 12-line do-file produced {} regions",
            seg.len()
        );
        let mut cursor = 0;
        for row in seg.rows.chunks_exact(REGION_STRIDE) {
            let (span_from, span_to, outer_from, outer_to) = (row[0], row[1], row[2], row[3]);
            assert_eq!(outer_from, cursor, "outer spans do not tile");
            assert!(outer_to >= outer_from, "inverted outer span");
            assert!(
                span_from >= outer_from && span_to <= outer_to,
                "span {span_from}..{span_to} escapes outer {outer_from}..{outer_to}"
            );
            cursor = outer_to;
        }
        assert_eq!(
            cursor as usize,
            doc.len(),
            "the tiling stopped short of EOF"
        );
        assert_eq!(seg.hashes.len(), seg.len() * REGION_HASH_STRIDE);
        // `// %% Load` and `// %% Model`.
        assert_eq!(seg.sections.len(), 2 * SECTION_STRIDE);
    }

    /// Two regions with the same canonical text differ only in `hash_ordinal` —
    /// the pre-`BlockMap` key the webview uses to keep two identical commands
    /// apart.
    #[test]
    fn identical_code_differs_only_by_ordinal() {
        let mut seg = Segmentation::default();
        ReferenceSegmenter.resegment("list\nlist\n", &mut seg);
        assert_eq!(seg.len(), 2);
        assert_eq!(seg.hashes[0], seg.hashes[3], "same text, different hash");
        assert_eq!(seg.hashes[1], seg.hashes[4]);
        assert_eq!((seg.hashes[2], seg.hashes[5]), (0, 1));
    }

    /// Whitespace is not part of the hash: re-indenting a block must not orphan
    /// its result card.
    #[test]
    fn the_hash_survives_reindentation() {
        let mut a = Segmentation::default();
        let mut b = Segmentation::default();
        ReferenceSegmenter.resegment("summarize price\n", &mut a);
        ReferenceSegmenter.resegment("   summarize    price\n", &mut b);
        assert_eq!(a.hashes[..2], b.hashes[..2]);
    }

    #[test]
    fn tokens_cover_the_requested_range_and_clamp_past_the_end() {
        let doc = "regress price mpg // fit\n";
        let mut out = Vec::new();
        ReferenceSegmenter.tokens(doc, 0..doc.len(), &mut out);
        assert_eq!(out.len() % TOKEN_STRIDE, 0);
        let mut cursor = 0;
        for t in out.chunks_exact(TOKEN_STRIDE) {
            assert_eq!(t[0], cursor, "token stream has a hole");
            cursor = t[1];
        }
        assert_eq!(cursor as usize, doc.len());

        out.clear();
        ReferenceSegmenter.tokens(doc, doc.len() + 500..doc.len() + 900, &mut out);
        assert!(out.is_empty(), "a range past EOF invented tokens");
    }

    #[test]
    fn completion_env_survives_a_malformed_push() {
        let mut e = Engine::new();
        let env = CompletionEnv {
            generation: 12,
            varnames: vec!["price".to_owned(), "mpg".to_owned()],
            ..CompletionEnv::default()
        };
        let bytes = rmp_serde::to_vec_named(&env).unwrap();
        e.set_completion_env(&bytes);
        assert_eq!(e.completion_env_generation(), 12);

        e.set_completion_env(&[0xc1, 0xc1, 0xc1]);
        assert_eq!(
            e.completion_env_generation(),
            12,
            "a malformed push keeps the previous environment"
        );
        assert_eq!(e.faults.len(), 1);
        assert_eq!(e.faults[0].code, "WASM0005");
    }

    #[test]
    fn completion_env_decodes_the_engine_wire_encoding() {
        // `to_vec_named` is what the engine emits (CONTRACTS §10); a positional
        // encoding here would decode into the wrong fields without erroring.
        let env = CompletionEnv {
            generation: 3,
            frame: "default".to_owned(),
            varnames: vec!["price".to_owned()],
            var_total: 32_767,
            truncated: true,
            ..CompletionEnv::default()
        };
        let bytes = rmp_serde::to_vec_named(&env).unwrap();
        let back: CompletionEnv = rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(back, env);
    }

    #[test]
    fn a_capped_environment_is_reported_however_the_backend_answers() {
        // A11 belongs to the environment, not to the candidate list: a backend
        // that never sets `truncated` must still produce a popup that says
        // "2 048 of 32 767" rather than implying 2 048 is all there is.
        let env = CompletionEnv {
            generation: 1,
            varnames: (0..2048).map(|i| format!("v{i}")).collect(),
            var_total: 32_767,
            truncated: true,
            ..CompletionEnv::default()
        };
        let mut list = ReferenceSegmenter.complete("", &env, 0);
        assert!(!list.truncated, "the backend itself reported nothing");
        stamp_truncation(&mut list, &env);
        assert!(list.truncated);
        assert_eq!((list.offered, list.total), (2048, 32_767));
    }

    #[test]
    fn an_uncapped_environment_is_left_alone() {
        let env = CompletionEnv {
            varnames: vec!["price".to_owned()],
            var_total: 1,
            ..CompletionEnv::default()
        };
        let mut list = CompletionList {
            offered: 9,
            total: 9,
            ..CompletionList::default()
        };
        stamp_truncation(&mut list, &env);
        assert!(!list.truncated);
        assert_eq!(
            (list.offered, list.total),
            (9, 9),
            "the backend's counts stand"
        );
    }

    #[test]
    fn offsets_saturate_rather_than_going_negative() {
        assert_eq!(i32_of(0), 0);
        assert_eq!(i32_of(i32::MAX as u32), i32::MAX);
        assert_eq!(i32_of(u32::MAX), i32::MAX);
    }

    #[test]
    fn the_completion_payload_matches_the_typescript_mirror() {
        // Field names here are what `apps/desktop/src/wasm/types.ts` declares.
        let list = CompletionList {
            from: 4,
            to: 7,
            items: vec![CompletionItem {
                label: "price".to_owned(),
                kind: CompletionKind::Variable,
                detail: Some("int".to_owned()),
                insert: None,
                rank: 0,
            }],
            truncated: true,
            offered: 2048,
            total: 32_767,
        };
        let json = serde_json::to_value(&list).unwrap();
        assert_eq!(json["items"][0]["kind"], "variable");
        assert_eq!(json["offered"], 2048);
        assert_eq!(json["total"], 32_767);
        assert_eq!(json["truncated"], true);
    }
}
