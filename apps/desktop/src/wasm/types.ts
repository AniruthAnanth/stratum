/**
 * The wasm segmenter's TypeScript contract.
 *
 * Two layers live here, and keeping them apart is the point of this file:
 *
 * 1. {@link RawModule} / {@link RawEngine} — CONTRACTS.md §14, method for
 *    method, name for name. Byte offsets, flat typed arrays, snake_case,
 *    exactly what `wasm-bindgen` emits from `crates/stratum-wasm/src/lib.rs`.
 *    The fenced development stub implements *this*, which is what makes the
 *    stub and the real module interchangeable rather than merely similar.
 * 2. {@link StratumSegmenter} — what the editor uses. UTF-16 offsets (what
 *    CodeMirror counts), decoded objects, camelCase. There is exactly one
 *    implementation, `segmenter.ts`, wrapping whichever raw module it was
 *    handed.
 *
 * The editor imports layer 2 and never layer 1. That is the whole reason W13
 * cannot tell the backends apart: it is not looking at either of them.
 *
 * Every constant below mirrors a `pub const` in `crates/stratum-wasm/src/lib.rs`
 * and is guarded by {@link WASM_ABI}, which the loader checks against the
 * module's own `abi_version()` before handing anything to the editor.
 */

// ---------------------------------------------------------------------------
// Flat view layout — mirrors crates/stratum-wasm/src/lib.rs
// ---------------------------------------------------------------------------

/** Layout version. Must equal the module's `abi_version()`. */
export const WASM_ABI = 1;

/** `i32`s per region in `regions_view()`. */
export const REGION_STRIDE = 9;
/** `u64`s per region in `region_hashes()`. */
export const REGION_HASH_STRIDE = 3;
/** `i32`s per token in `tokens()`. */
export const TOKEN_STRIDE = 3;
/** `i32`s per section in `sections()`. */
export const SECTION_STRIDE = 6;
/** `i32`s per narrative region in `narrative_regions()`. */
export const NARRATIVE_STRIDE = 3;

/** Field positions inside one region row. CONTRACTS §14 order. */
export const R_SPAN_FROM = 0;
/** @see R_SPAN_FROM */
export const R_SPAN_TO = 1;
/** @see R_SPAN_FROM */
export const R_OUTER_FROM = 2;
/** @see R_SPAN_FROM */
export const R_OUTER_TO = 3;
/** @see R_SPAN_FROM */
export const R_KIND = 4;
/** @see R_SPAN_FROM */
export const R_ENTRY_DELIM = 5;
/** @see R_SPAN_FROM */
export const R_HEAD_LINE = 6;
/** @see R_SPAN_FROM */
export const R_LAST_LINE = 7;
/** @see R_SPAN_FROM */
export const R_FLAGS = 8;

/** Field positions inside one section row. */
export const S_SPAN_FROM = 0;
/** @see S_SPAN_FROM */
export const S_SPAN_TO = 1;
/** @see S_SPAN_FROM */
export const S_ID = 2;
/** @see S_SPAN_FROM */
export const S_TITLE_FROM = 3;
/** @see S_SPAN_FROM */
export const S_TITLE_TO = 4;
/** @see S_SPAN_FROM */
export const S_MARKER_LINE = 5;

/** `flags` bit: the region can be sent to the engine as a run request. */
export const FLAG_EXECUTABLE = 1 << 0;
/** `flags` bit: the region is an estimation command (spec §19). */
export const FLAG_ESTIMATION = 1 << 1;
/** `flags` bit: a macro reference sits in command position. */
export const FLAG_MACRO_IN_HEAD = 1 << 2;
/** `flags` bit: the delimiter in force after this region is `;`. */
export const FLAG_EXIT_SEMI = 1 << 3;
/** `flags` bit: the region opens a section. */
export const FLAG_SECTION_HEAD = 1 << 4;

/** `entry_delim` value for carriage-return delimiting. */
export const DELIM_CR = 0;
/** `entry_delim` value for `#delimit ;`. */
export const DELIM_SEMI = 1;

/** `narrative_regions()` kind: a `//|` comment run. */
export const NARRATIVE_LINE = 0;
/** `narrative_regions()` kind: a `/*md … *​/` block. */
export const NARRATIVE_BLOCK = 1;

// --- kind codec -------------------------------------------------------------

/** `kind >> 8` families. Mirrors `FAMILY_*` in lib.rs. */
export const FAMILY_SIMPLE = 0;
/** @see FAMILY_SIMPLE */
export const FAMILY_BRACE = 1;
/** @see FAMILY_SIMPLE */
export const FAMILY_END_BLOCK = 2;
/** @see FAMILY_SIMPLE */
export const FAMILY_DIRECTIVE = 3;
/** @see FAMILY_SIMPLE */
export const FAMILY_TRIVIA = 4;
/** @see FAMILY_SIMPLE */
export const FAMILY_UNTERMINATED = 5;

const FAMILY_SHIFT = 8;
const DETAIL_MASK = 0xff;

/** Detail codes for `RegionKind::Brace`, in `BraceOpener` tag order. */
export const BRACE_OPENERS = [
  "foreach",
  "forvalues",
  "while",
  "if_else_chain",
  "capture",
  "quietly",
  "noisily",
  "anonymous",
  "other",
] as const;
/** One of {@link BRACE_OPENERS}. */
export type BraceOpener = (typeof BRACE_OPENERS)[number];

/** Detail codes for `RegionKind::EndBlock`, in `EndBlockOpener` tag order. */
export const END_BLOCK_OPENERS = ["program", "input", "mata", "python", "java"] as const;
/** One of {@link END_BLOCK_OPENERS}. */
export type EndBlockOpener = (typeof END_BLOCK_OPENERS)[number];

/** Detail codes for `RegionKind::Directive`, in `DirectiveKind` tag order. */
export const DIRECTIVE_KINDS = ["delimit_cr", "delimit_semi", "other"] as const;
/** One of {@link DIRECTIVE_KINDS}. */
export type DirectiveKind = (typeof DIRECTIVE_KINDS)[number];

/** Detail codes for `RegionKind::Unterminated`, in `Unterminated` tag order. */
export const UNTERMINATED = ["close_brace", "end", "block_comment", "compound_quote"] as const;
/** One of {@link UNTERMINATED}. */
export type Unterminated = (typeof UNTERMINATED)[number];

/**
 * `RegionKind` as proto serialises it (`#[serde(tag = "kind")]`), so the editor
 * and the wire speak one vocabulary.
 *
 * `end_block.name` is always `null` out of the flat view — the name is not in
 * the row. Slice it out of the document when a label is needed.
 */
export type RegionKind =
  | { kind: "simple" }
  | { kind: "brace"; opener: BraceOpener }
  | { kind: "end_block"; opener: EndBlockOpener; name: string | null }
  | { kind: "directive"; directive: DirectiveKind }
  | { kind: "trivia"; has_marker: boolean }
  | { kind: "unterminated"; expected: Unterminated };

/** Pack a {@link RegionKind} the way `encode_kind` in lib.rs does. */
export function encodeKind(k: RegionKind): number {
  switch (k.kind) {
    case "simple":
      return FAMILY_SIMPLE << FAMILY_SHIFT;
    case "brace":
      return (FAMILY_BRACE << FAMILY_SHIFT) | BRACE_OPENERS.indexOf(k.opener);
    case "end_block":
      return (FAMILY_END_BLOCK << FAMILY_SHIFT) | END_BLOCK_OPENERS.indexOf(k.opener);
    case "directive":
      return (FAMILY_DIRECTIVE << FAMILY_SHIFT) | DIRECTIVE_KINDS.indexOf(k.directive);
    case "trivia":
      return (FAMILY_TRIVIA << FAMILY_SHIFT) | (k.has_marker ? 1 : 0);
    case "unterminated":
      return (FAMILY_UNTERMINATED << FAMILY_SHIFT) | UNTERMINATED.indexOf(k.expected);
  }
}

/**
 * Unpack a `kind` field. `null` for a code this build does not know — which is
 * how an older webview survives a newer module inside one {@link WASM_ABI}.
 */
export function decodeKind(code: number): RegionKind | null {
  const detail = code & DETAIL_MASK;
  switch (code >> FAMILY_SHIFT) {
    case FAMILY_SIMPLE:
      return { kind: "simple" };
    case FAMILY_BRACE: {
      const opener = BRACE_OPENERS[detail];
      return opener ? { kind: "brace", opener } : null;
    }
    case FAMILY_END_BLOCK: {
      const opener = END_BLOCK_OPENERS[detail];
      return opener ? { kind: "end_block", opener, name: null } : null;
    }
    case FAMILY_DIRECTIVE: {
      const directive = DIRECTIVE_KINDS[detail];
      return directive ? { kind: "directive", directive } : null;
    }
    case FAMILY_TRIVIA:
      return { kind: "trivia", has_marker: detail !== 0 };
    case FAMILY_UNTERMINATED: {
      const expected = UNTERMINATED[detail];
      return expected ? { kind: "unterminated", expected } : null;
    }
    default:
      return null;
  }
}

/**
 * Token tags, index-aligned with `TOKEN_TAGS` in lib.rs, which is itself
 * `stratum_proto::TokenKind`. The index IS the wire tag; never reorder.
 */
export const TOKEN_TAGS = [
  "ident",
  "number",
  "str_lit",
  "compound_quote",
  "macro_ref",
  "op",
  "comma",
  "colon",
  "l_paren",
  "r_paren",
  "l_brace",
  "r_brace",
  "l_bracket",
  "r_bracket",
  "comment",
  "whitespace",
  "statement_break",
  "continuation",
  "directive",
  "unknown",
] as const;
/** One of {@link TOKEN_TAGS}. */
export type TokenTag = (typeof TOKEN_TAGS)[number];

// ---------------------------------------------------------------------------
// Layer 1: the raw module — CONTRACTS §14 verbatim.
// ---------------------------------------------------------------------------

/**
 * The `#[wasm_bindgen]` `Engine`. All offsets are **UTF-8 byte offsets**.
 *
 * `wasm-bindgen` does not camel-case Rust names, so these are the JS names the
 * generated glue actually exports. Do not "tidy" them: the stub matches this
 * shape so that the two are substitutable, and the loader's ABI check is the
 * only thing standing between a renamed method and a blank editor.
 */
export interface RawEngine {
  /** Grow the scratch buffer and return a pointer into linear memory. */
  reserve(bytes: number): number;
  /** Replace `[from, to)` with `len` bytes at `src` in the scratch buffer. */
  splice(from: number, to: number, src: number, len: number): void;
  /** Re-segment; returns the generation, unchanged if nothing changed. */
  resegment(): number;
  /** The current generation without re-segmenting. */
  generation(): number;
  /** Regions in the current segmentation. */
  region_count(): number;
  /** Flat `i32` rows, {@link REGION_STRIDE} each. */
  regions_view(): Int32Array;
  /** Flat `u64` rows, {@link REGION_HASH_STRIDE} each. Hashes, NOT identities. */
  region_hashes(): BigUint64Array;
  /** Flat `[from, to, tag]` triples overlapping the byte range. */
  tokens(from: number, to: number): Int32Array;
  /** Flat `i32` rows, {@link SECTION_STRIDE} each. */
  sections(): Int32Array;
  /** Flat `[from, to, kind]` triples for `//|` and `/*md` regions. */
  narrative_regions(): Int32Array;
  /** Parse diagnostics plus splice faults; drains the faults. */
  diagnostics(): unknown;
  /** Load the engine's own msgpack `CompletionEnv`. */
  set_completion_env(msgpack: Uint8Array): void;
  /** Generation of the loaded environment; `u64`, so a BigInt. */
  completion_env_generation(): bigint;
  /** Deterministic completion at a byte offset. */
  complete(pos: number): unknown;
  /** Deterministic fixes at a byte offset. */
  quick_fixes(pos: number): unknown;
  /** Whole-document lints. */
  lints(): unknown;
  /** The module's copy of the document. Test affordance. */
  doc_text(): string;
  /** The module's document length in bytes. */
  doc_len(): number;
  /** Release the wasm-side allocation. */
  free(): void;
}

/**
 * The module object: what `wasm-bindgen --target web` default-exports after its
 * init promise resolves, and what the stub fabricates.
 */
export interface RawModule {
  /** Constructor for {@link RawEngine}. */
  Engine: new () => RawEngine;
  /** Flat view layout version; must equal {@link WASM_ABI}. */
  abi_version(): number;
  /**
   * Whether a real segmenter is linked. A harness-only build returns false and
   * produces no regions at all — the loader refuses it in production rather
   * than letting it look like an empty document.
   */
  engine_linked(): boolean;
  /** Linear memory. The stub fabricates an object with a `buffer`. */
  memory: { readonly buffer: ArrayBufferLike };
}

// ---------------------------------------------------------------------------
// Layer 2: what the editor uses. UTF-16 offsets throughout.
// ---------------------------------------------------------------------------

/**
 * One CodeMirror change, in the coordinates `ChangeSet.iterChanges` reports:
 * `from`/`to` index the document **before** the transaction, and the changes
 * arrive in ascending, non-overlapping order.
 *
 * ```ts
 * const changes: DocChange[] = [];
 * tr.changes.iterChanges((fromA, toA, _fromB, _toB, ins) =>
 *   changes.push({ from: fromA, to: toA, insert: ins.toString() }));
 * ```
 */
export interface DocChange {
  /** Start offset in the pre-transaction document, in UTF-16 code units. */
  from: number;
  /** End offset in the pre-transaction document, in UTF-16 code units. */
  to: number;
  /** Replacement text. */
  insert: string;
}

/** A decoded region. All offsets are UTF-16 code units. */
export interface RegionView {
  /** Position in the region vector. NOT stable across edits. */
  index: number;
  /** Executable extent: first code unit .. last code unit. */
  from: number;
  /** End of the executable extent. */
  to: number;
  /** `from`/`to` widened to attached comments. Consecutive outers tile the file. */
  outerFrom: number;
  /** End of the outer extent. */
  outerTo: number;
  /** Decoded {@link RegionKind}, or `null` for a code this build cannot read. */
  kind: RegionKind | null;
  /** Raw `kind` field, for a caller that wants the unknown code. */
  kindCode: number;
  /** Delimiter in force at `from`. */
  entryDelimiter: "cr" | "semi";
  /** Delimiter in force after `to`. */
  exitDelimiter: "cr" | "semi";
  /** 0-based first line of the executable extent. */
  headLine: number;
  /** 0-based last line of the executable extent. */
  lastLine: number;
  /** The region has a run affordance. */
  executable: boolean;
  /** Spec §19 "Compare models" applies. */
  isEstimation: boolean;
  /** A macro sits in command position; completion downgrades to text. */
  hasMacroInHead: boolean;
  /** The region opens a section. */
  sectionHead: boolean;
  /**
   * `CodeHash` as 32 lowercase hex digits.
   *
   * **This is a hash, not an identity.** `BlockId` arrives from the engine in a
   * `BlockMap`; `(hashKey, hashOrdinal)` is only the pre-`BlockMap` key the
   * editor uses to keep a card attached for the one frame before the map lands.
   */
  hashKey: string;
  /** 0-based occurrence index of `hashKey` within the document. */
  hashOrdinal: number;
}

/** A decoded token. Offsets are UTF-16 code units. */
export interface TokenView {
  /** Start offset. */
  from: number;
  /** End offset. */
  to: number;
  /** Decoded tag, or `null` for a tag this build cannot read. */
  tag: TokenTag | null;
  /** Raw tag value. */
  tagCode: number;
}

/** A decoded section marker (`// %% Label`). Offsets are UTF-16 code units. */
export interface SectionView {
  /** Section id, stable within one segmentation. */
  id: number;
  /** Start of the section's extent. */
  from: number;
  /** End of the section's extent. */
  to: number;
  /** Start of the label text inside the marker comment. */
  titleFrom: number;
  /** End of the label text. */
  titleTo: number;
  /** 0-based line the marker sits on. */
  markerLine: number;
}

/** A decoded narrative region. Offsets are UTF-16 code units. */
export interface NarrativeView {
  /** Start offset. */
  from: number;
  /** End offset. */
  to: number;
  /** `"line"` for `//|`, `"block"` for `/*md … *​/`. */
  kind: "line" | "block";
}

// --- diagnostics and completion (mirrors of proto / lib.rs) -----------------

/** `stratum_proto::Severity`. */
export type Severity = "error" | "warning" | "note" | "help";

/** `stratum_proto::Edit`. */
export interface Edit {
  /** Byte span in the original source. */
  span: { start: number; end: number };
  /** Replacement text. */
  text: string;
}

/** `stratum_proto::SuggestionKind`. */
export type SuggestionKind =
  | "rename"
  | "insert_option"
  | "remove_option"
  | "rewrite"
  | "insert_line"
  | "change_path"
  | "explain";

/** `stratum_proto::Suggestion` — what `quickFixes` returns. */
export interface Suggestion {
  /** "Did you mean `income`?" */
  label: string;
  /** What kind of edit this is. */
  kind: SuggestionKind;
  /** Applying ALL edits atomically is the fix. Empty means informational. */
  edits: Edit[];
}

/** `stratum_proto::Confidence`. */
export type Confidence = "exact" | "probable" | "speculative";

/** `stratum_proto::Related` — another span worth showing with a diagnostic. */
export interface Related {
  /** Byte span in the original source. */
  span: { start: number; end: number };
  /** File the span is in, when it is a different one. */
  file: string | null;
  /** What to say about it. */
  message: string;
}

/**
 * `stratum_proto::Diagnostic`, as far as the segmenter can populate it.
 *
 * **Every field proto has, including the ones the wasm side never sets.** They
 * are here so the editor renders engine diagnostics and segmenter diagnostics
 * through one path — and, since this module is `serde`-serialised straight from
 * `stratum_proto::Diagnostic`, a field left out here is not a field left out of
 * the object: it is a field the stub then forgets to produce and the two
 * backends stop being interchangeable. `related` and `confidence` were exactly
 * that until `differential.ts` compared the key sets.
 */
export interface Diagnostic {
  /** How loud. */
  severity: Severity;
  /** Stable machine code from the one registry (ARCHITECTURE C14). */
  code: string;
  /** The Stata return code, when there is one. */
  stata_rc: number | null;
  /** Human text. */
  message: string;
  /** Source file. The segmenter works on one buffer and never sets it. */
  file: string | null;
  /** Byte span in the original source. */
  span: { start: number; end: number } | null;
  /** The token the diagnostic is about, for "did you mean". */
  offending_token: string | null;
  /** Engine-allocated block. §14 allocates none, so always `null` here. */
  block: number | null;
  /** Secondary spans. */
  related: Related[];
  /** Deterministic fixes. */
  suggestions: Suggestion[];
  /** Extra context lines. */
  notes: string[];
  /** Set when the finding came from a conservative or approximate analysis. */
  confidence: Confidence;
}

/** What a completion item refers to. Mirrors `CompletionKind` in lib.rs. */
export type CompletionKind =
  | "command"
  | "option"
  | "variable"
  | "local"
  | "global"
  | "scalar"
  | "matrix"
  | "frame"
  | "value_label"
  | "stored_estimate"
  | "stored_result"
  | "function"
  | "path"
  | "keyword";

/** One row in the completion popup. Mirrors `CompletionItem` in lib.rs. */
export interface CompletionItem {
  /** Text shown in the popup. */
  label: string;
  /** What it is; drives icon and sort group. */
  kind: CompletionKind;
  /** Right-aligned annotation. Never a variable label (A11). */
  detail: string | null;
  /** Text to insert when it differs from `label`. */
  insert: string | null;
  /** Sort rank within `kind`, ascending; ties break on `label`. */
  rank: number;
}

/**
 * Result of `complete()`, with offsets already converted to UTF-16.
 *
 * CONTRACTS §14 types `complete()` as `JsValue` and freezes no payload, so this
 * shape is `crates/stratum-wasm/src/lib.rs`'s, not proto's. Treat unknown extra
 * fields as additive.
 */
export interface CompletionList {
  /** Start of the range an accepted item replaces. */
  from: number;
  /** End of that range. */
  to: number;
  /** Ordered; render as given. */
  items: CompletionItem[];
  /** The environment behind these items was itself capped (A11). */
  truncated: boolean;
  /** Candidates offered. */
  offered: number;
  /** Candidates that exist. */
  total: number;
}

// ---------------------------------------------------------------------------

/** Which backend a {@link StratumSegmenter} is driving. */
export type SegmenterBackend = "wasm" | "stub";

/**
 * The editor-facing segmenter. **All offsets are UTF-16 code units**, i.e. the
 * same units `EditorState.doc` counts in — the byte conversion happens inside.
 *
 * One implementation (`segmenter.ts`) over two backends. If you find yourself
 * branching on {@link backend} outside a dev-only affordance, the abstraction
 * has failed and the editor has learned which module it is talking to.
 */
export interface StratumSegmenter {
  /** Which module is behind this. Dev banner only. */
  readonly backend: SegmenterBackend;
  /** The module's `abi_version()`. */
  readonly abi: number;
  /** Generation of the last {@link resegment}. */
  readonly generation: number;

  /** Replace the whole document. Used on open and to resynchronise. */
  setDoc(text: string): void;
  /** Apply one transaction's changes, in `iterChanges` order. */
  applyChanges(changes: readonly DocChange[]): void;
  /** Re-segment if the document changed; returns the generation either way. */
  resegment(): number;

  /** Regions in the current segmentation. */
  regionCount(): number;
  /** Decode every region. */
  regions(): RegionView[];
  /** Decode one region by index, or `null` when out of range. */
  region(index: number): RegionView | null;
  /** The region whose outer extent contains `pos`, or `null`. */
  regionAt(pos: number): RegionView | null;

  /** Tokens overlapping `[from, to)` — pass the visible range, not the file. */
  tokens(from: number, to: number): TokenView[];
  /** Section markers. */
  sections(): SectionView[];
  /** `//|` and `/*md` regions. */
  narrativeRegions(): NarrativeView[];
  /** Parse diagnostics and splice faults; drains the faults. */
  diagnostics(): Diagnostic[];

  /** Push the engine's msgpack `CompletionEnv`. */
  setCompletionEnv(msgpack: Uint8Array): void;
  /** Generation of the loaded environment. */
  completionEnvGeneration(): number;
  /** Deterministic completion at `pos`. HARD CONTRACT: < 2 ms. */
  complete(pos: number): CompletionList;
  /** Deterministic fixes at `pos`. */
  quickFixes(pos: number): Suggestion[];
  /** Whole-document lints. */
  lints(): Diagnostic[];

  /** The wrapper's document mirror. Assert it against `state.doc` in tests. */
  docText(): string;
  /** Release the backend. */
  destroy(): void;
}
