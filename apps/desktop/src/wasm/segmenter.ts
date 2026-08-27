/**
 * The one wrapper over a {@link RawModule}.
 *
 * There is exactly one implementation of the editor-facing segmenter, and both
 * backends — the real wasm module and the fenced development stub — go through
 * it. That is not tidiness: it is the reason W13 cannot tell them apart. If the
 * wrapper were duplicated per backend, "byte-compatible" would be a claim about
 * two code paths instead of a property of one.
 *
 * Its real job is coordinates. CodeMirror counts **UTF-16 code units**; the wasm
 * boundary counts **UTF-8 bytes**. Getting that wrong does not fail loudly — it
 * puts a block outline half a character off in exactly the documents (Danish
 * labels, Greek variable names, an em dash in a comment) that no one tests. So
 * the conversion lives here, once, and is exercised by `conformance.ts`.
 */

import {
  DELIM_SEMI,
  FLAG_ESTIMATION,
  FLAG_EXECUTABLE,
  FLAG_EXIT_SEMI,
  FLAG_MACRO_IN_HEAD,
  FLAG_SECTION_HEAD,
  NARRATIVE_BLOCK,
  NARRATIVE_STRIDE,
  REGION_HASH_STRIDE,
  REGION_STRIDE,
  R_ENTRY_DELIM,
  R_FLAGS,
  R_HEAD_LINE,
  R_KIND,
  R_LAST_LINE,
  R_OUTER_FROM,
  R_OUTER_TO,
  R_SPAN_FROM,
  R_SPAN_TO,
  SECTION_STRIDE,
  S_ID,
  S_MARKER_LINE,
  S_SPAN_FROM,
  S_SPAN_TO,
  S_TITLE_FROM,
  S_TITLE_TO,
  TOKEN_STRIDE,
  TOKEN_TAGS,
  decodeKind,
} from "./types.ts";
import type {
  CompletionList,
  Diagnostic,
  DocChange,
  NarrativeView,
  RawEngine,
  RawModule,
  RegionView,
  SectionView,
  SegmenterBackend,
  StratumSegmenter,
  Suggestion,
  TokenView,
} from "./types.ts";

const encoder = new TextEncoder();

/**
 * UTF-16 ⇄ UTF-8 offset translation for one document.
 *
 * An all-ASCII document — which is nearly every do-file — needs no translation
 * at all, so the common path is a boolean test and an identity return. The
 * moment one non-ASCII code unit appears, we build two dense lookup tables in a
 * single pass and translate in O(1) thereafter. Dense tables cost 4 bytes per
 * offset each way, which is ~3 MB on the 2 MB document where Large File Mode
 * (06 §3.3) has already taken over anyway; the alternative — binary searching a
 * line index and re-encoding a line per lookup — is O(line) per region field,
 * and a screenful of regions asks for thousands of those per keystroke.
 */
class OffsetMap {
  /** True while every code unit in the document is < U+0080. */
  ascii = true;
  private u16ToU8: Int32Array | null = null;
  private u8ToU16: Int32Array | null = null;
  private text = "";
  private stale = true;

  /** Point the map at a new document. Tables are rebuilt on first use. */
  reset(text: string, ascii: boolean): void {
    this.text = text;
    this.ascii = ascii;
    this.stale = true;
    this.u16ToU8 = null;
    this.u8ToU16 = null;
  }

  /** Byte length of the current document. */
  byteLength(): number {
    if (this.ascii) return this.text.length;
    this.build();
    // Guaranteed non-null by build().
    return (this.u16ToU8 as Int32Array)[this.text.length] as number;
  }

  /**
   * UTF-16 offset → UTF-8 byte offset.
   *
   * Both paths clamp, and that is not symmetry for its own sake: the table path
   * clamps because it indexes an array, and an ASCII path that passed `-1`
   * straight through made the same call answer differently depending on whether
   * the document happened to contain a non-ASCII character. `regionAt(-1)`
   * returned nothing on a plain do-file and region 0 as soon as someone typed an
   * accent — found by the generated sessions in `differential.ts`.
   */
  toBytes(unit: number): number {
    if (this.ascii) return Math.max(0, Math.min(unit, this.text.length));
    this.build();
    const t = this.u16ToU8 as Int32Array;
    return (t[Math.max(0, Math.min(unit, this.text.length))] as number) | 0;
  }

  /** UTF-8 byte offset → UTF-16 offset. */
  toUnits(byte: number): number {
    if (this.ascii) return byte;
    this.build();
    const t = this.u8ToU16 as Int32Array;
    return (t[Math.max(0, Math.min(byte, t.length - 1))] as number) | 0;
  }

  private build(): void {
    if (!this.stale) return;
    const text = this.text;
    // One pass. `codePointAt` folds a surrogate pair into one code point; a LONE
    // surrogate comes back as itself, and TextEncoder turns it into U+FFFD — 3
    // bytes, which is exactly what the `< 0x10000` arm below charges for it, so
    // a torn pair does not desynchronise the tables.
    const u16ToU8 = new Int32Array(text.length + 1);
    let bytes = 0;
    for (let i = 0; i < text.length; ) {
      const cp = text.codePointAt(i) as number;
      const units = cp > 0xffff ? 2 : 1;
      const width = cp < 0x80 ? 1 : cp < 0x800 ? 2 : cp < 0x10000 ? 3 : 4;
      for (let k = 0; k < units; k++) u16ToU8[i + k] = bytes;
      i += units;
      bytes += width;
    }
    u16ToU8[text.length] = bytes;

    const u8ToU16 = new Int32Array(bytes + 1);
    for (let i = 0, b = 0; i < text.length; ) {
      const cp = text.codePointAt(i) as number;
      const units = cp > 0xffff ? 2 : 1;
      const width = cp < 0x80 ? 1 : cp < 0x800 ? 2 : cp < 0x10000 ? 3 : 4;
      for (let k = 0; k < width; k++) u8ToU16[b + k] = i;
      i += units;
      b += width;
    }
    u8ToU16[bytes] = text.length;

    this.u16ToU8 = u16ToU8;
    this.u8ToU16 = u8ToU16;
    this.stale = false;
  }
}

/** Highest code unit for which a UTF-16 offset and a UTF-8 offset agree. */
const ASCII_MAX = 0x7f;

/**
 * True when every code unit is ASCII — the condition under which UTF-16 and
 * UTF-8 offsets are the same number, and therefore the condition that keeps the
 * conversion tables unbuilt.
 *
 * A plain scan rather than a regex: this runs over the whole document on open
 * and over the (short) inserted text on every keystroke, and a character-class
 * regex spelled with escapes is exactly the kind of thing that survives a copy
 * between files as the wrong bytes.
 */
function isAscii(s: string): boolean {
  for (let i = 0; i < s.length; i++) {
    if ((s.charCodeAt(i) as number) > ASCII_MAX) return false;
  }
  return true;
}

/** `hi`/`lo` as the 32-hex-digit key documented on `RegionRow::hash_lo`. */
function hashKey(hi: bigint, lo: bigint): string {
  return hi.toString(16).padStart(16, "0") + lo.toString(16).padStart(16, "0");
}

/**
 * Wrap a raw module. Prefer `loadSegmenter` in `loader.ts` — it performs the ABI
 * and linkage checks this constructor assumes have already passed.
 */
export function createSegmenter(module: RawModule, backend: SegmenterBackend): StratumSegmenter {
  return new WrappedSegmenter(module, backend);
}

class WrappedSegmenter implements StratumSegmenter {
  readonly backend: SegmenterBackend;
  readonly abi: number;
  generation = 0;

  private engine: RawEngine;
  private module: RawModule;
  /** The wrapper's mirror of the document, in JS string coordinates. */
  private text = "";
  private map = new OffsetMap();
  private disposed = false;

  constructor(module: RawModule, backend: SegmenterBackend) {
    this.module = module;
    this.backend = backend;
    this.abi = module.abi_version();
    this.engine = new module.Engine();
    this.map.reset("", true);
  }

  setDoc(text: string): void {
    this.assertLive();
    // One splice over the whole document. The engine keeps no incremental state
    // across a full replace, so there is nothing cheaper to do here.
    const oldBytes = this.map.byteLength();
    const bytes = encoder.encode(text);
    const ptr = this.engine.reserve(Math.max(bytes.length, 1));
    this.write(ptr, bytes);
    this.engine.splice(0, oldBytes, 0, bytes.length);
    this.text = text;
    this.map.reset(text, isAscii(text));
    this.checkSync();
  }

  applyChanges(changes: readonly DocChange[]): void {
    this.assertLive();
    if (changes.length === 0) return;

    // Byte offsets are resolved against the PRE-transaction document, because
    // that is the coordinate system `iterChanges` reports in. Resolving them
    // lazily, after the first splice has already moved the text, is the classic
    // way to get a two-change transaction subtly wrong.
    const spans = changes.map((c) => ({
      from: this.map.toBytes(c.from),
      to: this.map.toBytes(c.to),
      bytes: encoder.encode(c.insert),
    }));

    // One reserve for the whole transaction: `reserve` may grow linear memory,
    // which detaches every existing view, so doing it once keeps the pointer
    // valid across all the writes below.
    const total = spans.reduce((n, s) => n + s.bytes.length, 0);
    const ptr = this.engine.reserve(Math.max(total, 1));
    let cursor = 0;
    const view = this.memory();
    for (const s of spans) {
      view.set(s.bytes, ptr + cursor);
      cursor += s.bytes.length;
    }

    let delta = 0;
    cursor = 0;
    for (const s of spans) {
      this.engine.splice(s.from + delta, s.to + delta, cursor, s.bytes.length);
      delta += s.bytes.length - (s.to - s.from);
      cursor += s.bytes.length;
    }

    let out = "";
    let last = 0;
    let ascii = this.map.ascii;
    for (const c of changes) {
      out += this.text.slice(last, c.from) + c.insert;
      last = c.to;
      if (ascii && c.insert.length > 0 && !isAscii(c.insert)) ascii = false;
    }
    out += this.text.slice(last);
    this.text = out;
    // Deletion can return a document to pure ASCII; we do not detect that, and
    // deliberately: the only cost is staying on the table-driven path until the
    // next full replace, and re-scanning the document per keystroke to find out
    // would cost more than it saves.
    this.map.reset(out, ascii);
    this.checkSync();
  }

  resegment(): number {
    this.assertLive();
    this.generation = this.engine.resegment();
    return this.generation;
  }

  regionCount(): number {
    return this.engine.region_count();
  }

  regions(): RegionView[] {
    const rows = this.engine.regions_view();
    const hashes = this.engine.region_hashes();
    const n = Math.floor(rows.length / REGION_STRIDE);
    const out: RegionView[] = new Array(n);
    for (let i = 0; i < n; i++) out[i] = this.decodeRegion(rows, hashes, i);
    return out;
  }

  region(index: number): RegionView | null {
    const rows = this.engine.regions_view();
    const n = Math.floor(rows.length / REGION_STRIDE);
    if (index < 0 || index >= n) return null;
    return this.decodeRegion(rows, this.engine.region_hashes(), index);
  }

  regionAt(pos: number): RegionView | null {
    // Outer spans tile the file exactly (CONTRACTS §2), so a binary search over
    // their starts is total — every position lands in exactly one region.
    //
    // Except a negative one, which is not a position in any document: `toBytes`
    // clamps it to 0 and the search would then hand back region 0 as if the
    // caller had asked about the first character. `pos === length` is left
    // alone on purpose — that is the cursor at end of file, and it answers the
    // last region.
    if (pos < 0) return null;
    const rows = this.engine.regions_view();
    const n = Math.floor(rows.length / REGION_STRIDE);
    if (n === 0) return null;
    const byte = this.map.toBytes(pos);
    let lo = 0;
    let hi = n - 1;
    let found = -1;
    while (lo <= hi) {
      const mid = (lo + hi) >> 1;
      const start = rows[mid * REGION_STRIDE + R_OUTER_FROM] as number;
      if (start <= byte) {
        found = mid;
        lo = mid + 1;
      } else {
        hi = mid - 1;
      }
    }
    if (found < 0) return null;
    const end = rows[found * REGION_STRIDE + R_OUTER_TO] as number;
    if (byte > end) return null;
    return this.decodeRegion(rows, this.engine.region_hashes(), found);
  }

  tokens(from: number, to: number): TokenView[] {
    const flat = this.engine.tokens(this.map.toBytes(from), this.map.toBytes(to));
    const n = Math.floor(flat.length / TOKEN_STRIDE);
    const out: TokenView[] = new Array(n);
    for (let i = 0; i < n; i++) {
      const b = i * TOKEN_STRIDE;
      const tagCode = flat[b + 2] as number;
      out[i] = {
        from: this.map.toUnits(flat[b] as number),
        to: this.map.toUnits(flat[b + 1] as number),
        tag: TOKEN_TAGS[tagCode] ?? null,
        tagCode,
      };
    }
    return out;
  }

  sections(): SectionView[] {
    const flat = this.engine.sections();
    const n = Math.floor(flat.length / SECTION_STRIDE);
    const out: SectionView[] = new Array(n);
    for (let i = 0; i < n; i++) {
      const b = i * SECTION_STRIDE;
      out[i] = {
        id: flat[b + S_ID] as number,
        from: this.map.toUnits(flat[b + S_SPAN_FROM] as number),
        to: this.map.toUnits(flat[b + S_SPAN_TO] as number),
        titleFrom: this.map.toUnits(flat[b + S_TITLE_FROM] as number),
        titleTo: this.map.toUnits(flat[b + S_TITLE_TO] as number),
        markerLine: flat[b + S_MARKER_LINE] as number,
      };
    }
    return out;
  }

  narrativeRegions(): NarrativeView[] {
    const flat = this.engine.narrative_regions();
    const n = Math.floor(flat.length / NARRATIVE_STRIDE);
    const out: NarrativeView[] = new Array(n);
    for (let i = 0; i < n; i++) {
      const b = i * NARRATIVE_STRIDE;
      out[i] = {
        from: this.map.toUnits(flat[b] as number),
        to: this.map.toUnits(flat[b + 1] as number),
        kind: flat[b + 2] === NARRATIVE_BLOCK ? "block" : "line",
      };
    }
    return out;
  }

  diagnostics(): Diagnostic[] {
    const raw = this.engine.diagnostics();
    if (!Array.isArray(raw)) return [];
    return (raw as Diagnostic[]).map((d) => this.mapDiagnostic(d));
  }

  setCompletionEnv(msgpack: Uint8Array): void {
    this.engine.set_completion_env(msgpack);
  }

  completionEnvGeneration(): number {
    return Number(this.engine.completion_env_generation());
  }

  complete(pos: number): CompletionList {
    const raw = this.engine.complete(this.map.toBytes(pos)) as CompletionList | null;
    if (!raw) return { from: pos, to: pos, items: [], truncated: false, offered: 0, total: 0 };
    return {
      ...raw,
      from: this.map.toUnits(raw.from),
      to: this.map.toUnits(raw.to),
      items: raw.items ?? [],
    };
  }

  quickFixes(pos: number): Suggestion[] {
    const raw = this.engine.quick_fixes(this.map.toBytes(pos));
    if (!Array.isArray(raw)) return [];
    return (raw as Suggestion[]).map((s) => ({
      ...s,
      edits: (s.edits ?? []).map((e) => ({ ...e, span: this.mapSpan(e.span) })),
    }));
  }

  lints(): Diagnostic[] {
    const raw = this.engine.lints();
    if (!Array.isArray(raw)) return [];
    return (raw as Diagnostic[]).map((d) => this.mapDiagnostic(d));
  }

  docText(): string {
    return this.text;
  }

  destroy(): void {
    if (this.disposed) return;
    this.disposed = true;
    this.engine.free();
  }

  // --- internals -----------------------------------------------------------

  private decodeRegion(rows: Int32Array, hashes: BigUint64Array, i: number): RegionView {
    const b = i * REGION_STRIDE;
    const h = i * REGION_HASH_STRIDE;
    const flags = rows[b + R_FLAGS] as number;
    const kindCode = rows[b + R_KIND] as number;
    return {
      index: i,
      from: this.map.toUnits(rows[b + R_SPAN_FROM] as number),
      to: this.map.toUnits(rows[b + R_SPAN_TO] as number),
      outerFrom: this.map.toUnits(rows[b + R_OUTER_FROM] as number),
      outerTo: this.map.toUnits(rows[b + R_OUTER_TO] as number),
      kind: decodeKind(kindCode),
      kindCode,
      entryDelimiter: rows[b + R_ENTRY_DELIM] === DELIM_SEMI ? "semi" : "cr",
      exitDelimiter: (flags & FLAG_EXIT_SEMI) !== 0 ? "semi" : "cr",
      headLine: rows[b + R_HEAD_LINE] as number,
      lastLine: rows[b + R_LAST_LINE] as number,
      executable: (flags & FLAG_EXECUTABLE) !== 0,
      isEstimation: (flags & FLAG_ESTIMATION) !== 0,
      hasMacroInHead: (flags & FLAG_MACRO_IN_HEAD) !== 0,
      sectionHead: (flags & FLAG_SECTION_HEAD) !== 0,
      hashKey: hashKey((hashes[h + 1] ?? 0n) as bigint, (hashes[h] ?? 0n) as bigint),
      hashOrdinal: Number(hashes[h + 2] ?? 0n),
    };
  }

  private mapDiagnostic(d: Diagnostic): Diagnostic {
    return {
      ...d,
      span: this.mapSpan(d.span),
      suggestions: (d.suggestions ?? []).map((s) => ({
        ...s,
        edits: (s.edits ?? []).map((e) => ({ ...e, span: this.mapSpan(e.span) })),
      })),
      notes: d.notes ?? [],
    };
  }

  private mapSpan<T extends { start: number; end: number } | null>(span: T): T {
    if (!span) return span;
    return { ...span, start: this.map.toUnits(span.start), end: this.map.toUnits(span.end) } as T;
  }

  /** A fresh view: `reserve` can grow memory, which detaches the old buffer. */
  private memory(): Uint8Array {
    return new Uint8Array(this.module.memory.buffer);
  }

  private write(ptr: number, bytes: Uint8Array): void {
    this.memory().set(bytes, ptr);
  }

  /**
   * The two document mirrors must agree byte for byte.
   *
   * A divergence here means an offset conversion or a change-set application is
   * wrong, and every region from that point on is silently misplaced. Comparing
   * lengths costs one call and catches it on the transaction that caused it.
   */
  private checkSync(): void {
    const theirs = this.engine.doc_len();
    const ours = this.map.byteLength();
    if (theirs !== ours) {
      const sizes = `wasm ${theirs} bytes, host ${ours} bytes`;
      throw new Error(
        `stratum-wasm: document mirrors diverged (${sizes}). This is a bug in the change-set application, not in user input.`,
      );
    }
  }

  private assertLive(): void {
    if (this.disposed) throw new Error("stratum-wasm: segmenter used after destroy()");
  }
}
