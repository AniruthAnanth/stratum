/**
 * The hand-written half of the IPC surface — CONTRACTS.md §12, transcribed.
 *
 * Everything specta CAN express is generated into `commands.ts`, `events.ts`
 * and `types.ts` by `cargo test export_bindings`. This file holds only what it
 * cannot: nominal branding over `number`, a total order that lives in the UI's
 * display rule rather than on the wire, and a decoder for bytes that never pass
 * through serde at all.
 *
 * Nothing here may become a hand-written mirror of a Rust type. Where a
 * generated type is unavoidable — `worseOf` takes a `BlockStatus` — the
 * signature is written against the STRUCTURE the function actually reads (the
 * `state` discriminant), so the generated type substitutes without an import
 * cycle and without a second declaration of the same shape.
 */

// ---------------------------------------------------------------------------
// Branded ids
// ---------------------------------------------------------------------------

/** Branded ids — prevents passing a ResultId where a BlockId is expected. */
export type BlockId = number & { readonly __b: unique symbol };
export type ExecId = number & { readonly __e: unique symbol };
export type ResultId = number & { readonly __r: unique symbol };
export type DatasetStateId = number & { readonly __d: unique symbol };
export type SessionId = number & { readonly __s: unique symbol };
export type DocumentId = number & { readonly __doc: unique symbol };
export type RunId = number & { readonly __run: unique symbol };

/** 16 bytes, lowercase hex, 32 chars. */
export type CodeHash = string & { readonly __ch: unique symbol };

const HEX32 = /^[0-9a-f]{32}$/;

/**
 * The one sanctioned way into `CodeHash`. A brand that anything can be cast
 * into is decoration; the engine's hashes arrive as strings and this is where
 * the 32-lowercase-hex claim in §12 is actually checked.
 */
export function codeHash(s: string): CodeHash {
  if (!HEX32.test(s)) {
    throw new TypeError(`not a CodeHash: ${JSON.stringify(s)}`);
  }
  return s as CodeHash;
}

/** Narrowing predicate for untrusted input (a sidecar read off disk, say). */
export function isCodeHash(s: string): s is CodeHash {
  return HEX32.test(s);
}

/**
 * Ids are opaque integers on the wire. These exist so a call site that really
 * does hold an id of that kind can say so once, instead of scattering `as`.
 */
export const asBlockId = (n: number): BlockId => n as BlockId;
export const asExecId = (n: number): ExecId => n as ExecId;
export const asResultId = (n: number): ResultId => n as ResultId;
export const asDatasetStateId = (n: number): DatasetStateId => n as DatasetStateId;
export const asSessionId = (n: number): SessionId => n as SessionId;
export const asDocumentId = (n: number): DocumentId => n as DocumentId;
export const asRunId = (n: number): RunId => n as RunId;

// ---------------------------------------------------------------------------
// Client-side block identity (ARCHITECTURE C4)
// ---------------------------------------------------------------------------

/** The frontend's pre-BlockMap widget key (ARCHITECTURE C4). */
export interface ClientBlockKey {
  readonly hash: CodeHash;
  readonly ordinal: number;
}

export function clientKey(hash: CodeHash, ordinal: number): string {
  return `${hash}:${ordinal}`;
}

// ---------------------------------------------------------------------------
// The display rule (ARCHITECTURE C20, CONTRACTS §3)
// ---------------------------------------------------------------------------

/** Total order used by `worseOf`. Higher = healthier. */
export const STATUS_RANK = {
  never_run: 0,
  broken: 1,
  failed: 2,
  interrupted: 3,
  stale: 4,
  current_unverifiable: 5,
  current: 6,
  queued: 90,
  running: 91,
} as const;

/**
 * The `state` discriminant of the generated `BlockStatus`. Declared from
 * `STATUS_RANK` rather than beside it, so the table and the type cannot drift.
 */
export type BlockStatusState = keyof typeof STATUS_RANK;

/** The structural minimum `worseOf` reads. The generated `BlockStatus` satisfies it. */
export interface HasBlockState {
  readonly state: BlockStatusState;
}

/**
 * `displayed = worseOf(local, kernel)` — CONTRACTS §3's display rule.
 *
 * Generic rather than typed against `BlockStatus`, because `BlockStatus` is
 * generated and §12 forbids a hand-written mirror of it. The generic also
 * preserves the caller's payload: `worseOf` hands back one of its two
 * arguments, never a reconstruction, so `Failed { exec, rc }` keeps its `rc`.
 *
 * Queued and Running win outright in argument order, because they are facts
 * about the kernel rather than judgements about the text; a local
 * `Stale{CodeChanged}` must never hide a block that is running right now.
 */
export function worseOf<T extends HasBlockState>(a: T, b: T): T {
  if (STATUS_RANK[a.state] >= 90) return a;
  if (STATUS_RANK[b.state] >= 90) return b;
  return STATUS_RANK[a.state] <= STATUS_RANK[b.state] ? a : b;
}

// ---------------------------------------------------------------------------
// §8.1 — the SDP1 binary DataPage
// ---------------------------------------------------------------------------

export type ColumnKind = "text" | "num" | "blob";

/** One entry of the JSON header's `cols` array, exactly as §8.1 gives it. */
interface WireColumn {
  idx: number;
  kind: ColumnKind;
  off: number;
  len: number;
  aux_off: number;
  aux_len: number;
}

interface WireHeader {
  state: number;
  row0: number;
  nrows: number;
  seq: number;
  cols: WireColumn[];
}

/** `RenderMode::Display` strings, and `Edit` strings for a fixed-width column. */
export interface TextColumn {
  readonly kind: "text";
  readonly idx: number;
  /** `(nrows+1)` ascending offsets into `arena`; `[0] === 0`, `[nrows] === arena.length`. */
  readonly offsets: Uint32Array;
  readonly arena: Uint8Array;
  /** Decoded lazily: a 10 M-row page is never all on screen. */
  cell(row: number): string;
}

/** `RenderMode::Edit` numerics: the f64 payload plus the redundant missing tag. */
export interface NumColumn {
  readonly kind: "num";
  readonly idx: number;
  readonly values: Float64Array;
  /** 255 = not missing, 0 = `.`, 1..=26 = `.a`..`.z`. */
  readonly tags: Uint8Array;
  isMissing(row: number): boolean;
}

/** strL. Values may be arbitrary bytes; `isBinary` says which. */
export interface BlobColumn {
  readonly kind: "blob";
  readonly idx: number;
  readonly offsets: Uint32Array;
  readonly arena: Uint8Array;
  /** `ceil(nrows/8)` bytes, LSB first: a set bit is GSO type 129. */
  readonly binaryBitmap: Uint8Array;
  isBinary(row: number): boolean;
  bytes(row: number): Uint8Array;
  /** `undefined` when the bitmap marks the row binary — never mojibake. */
  cell(row: number): string | undefined;
}

export type DataColumn = TextColumn | NumColumn | BlobColumn;

export interface DataPage {
  readonly state: DatasetStateId;
  readonly row0: number;
  readonly nrows: number;
  readonly seq: number;
  readonly cols: readonly DataColumn[];
  /** Lookup by the variable's storage index, which is what `PageRequest.cols` names. */
  column(idx: number): DataColumn | undefined;
}

export class DataPageError extends Error {
  constructor(message: string) {
    super(`SDP1: ${message}`);
    this.name = "DataPageError";
  }
}

const MAGIC = 0x53_44_50_31; // "SDP1", read big-endian so the literal reads left to right
const utf8 = new TextDecoder("utf-8", { fatal: false });

function need(cond: boolean, what: string): asserts cond {
  if (!cond) throw new DataPageError(what);
}

/**
 * Decoder for the SDP1 binary DataPage of CONTRACTS §8.1. No dependencies.
 *
 * Input comes from `fetch("stratum-asset://localhost/frame/…")` — the single
 * frame transport (A13). The reference fixture both sides assert against is
 * `tests/fixtures/sdp1/auto_40x12.bin`, owned by W00.
 *
 * Zero-copy: every region is a typed-array VIEW over the caller's buffer, so a
 * 40-row page costs one allocation per column descriptor and no byte copies.
 * That is only legal because §2.1 of the fixture README pads the header until
 * the payload starts 8-aligned — `new Float64Array(buf, byteOffset, n)` throws
 * otherwise. This decoder re-checks the alignment rather than trusting it,
 * because the failure it prevents is a `RangeError` from deep inside a scroll
 * frame.
 */
export function decodeDataPage(buf: ArrayBuffer): DataPage {
  need(buf.byteLength >= 8, `truncated: ${buf.byteLength} bytes, need at least 8`);
  const view = new DataView(buf);

  need(view.getUint32(0, false) === MAGIC, "bad magic, expected 'SDP1'");
  const headerLen = view.getUint32(4, true);
  need(8 + headerLen <= buf.byteLength, `header_len ${headerLen} runs past the buffer`);

  const base = 8 + headerLen;
  need(base % 8 === 0, `payload starts at ${base}, which is not 8-aligned (fixture README §2.1)`);

  let header: WireHeader;
  try {
    // JSON tolerates the trailing space padding, so the raw slice parses.
    header = JSON.parse(utf8.decode(new Uint8Array(buf, 8, headerLen))) as WireHeader;
  } catch (cause) {
    throw new DataPageError(`header is not JSON: ${String(cause)}`);
  }
  need(Number.isInteger(header.nrows) && header.nrows >= 0, "header.nrows is not a row count");
  need(Array.isArray(header.cols), "header.cols is not an array");

  const nrows = header.nrows;
  const avail = buf.byteLength - base;
  const cols: DataColumn[] = [];

  for (const c of header.cols) {
    const where = `col ${c.idx} (${c.kind})`;
    need(c.off >= 0 && c.len >= 0 && c.off + c.len <= avail, `${where}: data region out of bounds`);
    need(
      c.aux_off >= 0 && c.aux_len >= 0 && c.aux_off + c.aux_len <= avail,
      `${where}: aux region out of bounds`,
    );

    switch (c.kind) {
      case "text":
      case "blob": {
        need(c.aux_off % 4 === 0, `${where}: aux is u32 and must be 4-aligned`);
        need(c.aux_len === 4 * (nrows + 1), `${where}: aux_len ${c.aux_len} != 4*(nrows+1)`);
        const offsets = new Uint32Array(buf, base + c.aux_off, nrows + 1);
        need(offsets[0] === 0, `${where}: aux[0] must be 0`);
        for (let i = 1; i <= nrows; i++) {
          // Equal neighbours are the empty cell, which is legal and load-bearing:
          // the fixture's third strL is empty precisely to catch a decoder that
          // reads "zero length" as "absent".
          need(
            (offsets[i] as number) >= (offsets[i - 1] as number),
            `${where}: aux is not ascending`,
          );
        }
        const end = offsets[nrows] as number;
        if (c.kind === "text") {
          need(end === c.len, `${where}: aux[nrows] ${end} != len ${c.len}`);
          const arena = new Uint8Array(buf, base + c.off, c.len);
          cols.push(textColumn(c.idx, offsets, arena));
        } else {
          const bitmapLen = (nrows + 7) >> 3;
          need(
            end + bitmapLen === c.len,
            `${where}: len ${c.len} != arena ${end} + bitmap ${bitmapLen} (fixture README §2.3)`,
          );
          const arena = new Uint8Array(buf, base + c.off, end);
          const bitmap = new Uint8Array(buf, base + c.off + end, bitmapLen);
          cols.push(blobColumn(c.idx, offsets, arena, bitmap));
        }
        break;
      }
      case "num": {
        need(c.off % 8 === 0, `${where}: f64 data must be 8-aligned`);
        need(c.len === 8 * nrows, `${where}: len ${c.len} != 8*nrows`);
        need(c.aux_len === nrows, `${where}: aux_len ${c.aux_len} != nrows`);
        const values = new Float64Array(buf, base + c.off, nrows);
        const tags = new Uint8Array(buf, base + c.aux_off, nrows);
        cols.push(numColumn(c.idx, values, tags));
        break;
      }
      default:
        throw new DataPageError(`${where}: unknown kind`);
    }
  }

  const byIdx = new Map<number, DataColumn>(cols.map((c) => [c.idx, c]));
  return {
    state: header.state as DatasetStateId,
    row0: header.row0,
    nrows,
    seq: header.seq,
    cols,
    column: (idx) => byIdx.get(idx),
  };
}

function textColumn(idx: number, offsets: Uint32Array, arena: Uint8Array): TextColumn {
  return {
    kind: "text",
    idx,
    offsets,
    arena,
    cell(row) {
      const lo = offsets[row];
      const hi = offsets[row + 1];
      if (lo === undefined || hi === undefined) return "";
      return utf8.decode(arena.subarray(lo, hi));
    },
  };
}

function numColumn(idx: number, values: Float64Array, tags: Uint8Array): NumColumn {
  return {
    kind: "num",
    idx,
    values,
    tags,
    // The tag is redundant with the f64's sentinel bits by construction, and we
    // read the tag: JS cannot cheaply pattern-match an f64 bit pattern, which is
    // the entire reason §8.1 carries the tag column at all.
    isMissing: (row) => tags[row] !== 255,
  };
}

function blobColumn(
  idx: number,
  offsets: Uint32Array,
  arena: Uint8Array,
  binaryBitmap: Uint8Array,
): BlobColumn {
  const isBinary = (row: number) => (((binaryBitmap[row >> 3] ?? 0) >> (row & 7)) & 1) === 1;
  const bytes = (row: number) => {
    const lo = offsets[row];
    const hi = offsets[row + 1];
    if (lo === undefined || hi === undefined) return new Uint8Array(0);
    return arena.subarray(lo, hi);
  };
  return {
    kind: "blob",
    idx,
    offsets,
    arena,
    binaryBitmap,
    isBinary,
    bytes,
    cell: (row) => (isBinary(row) ? undefined : utf8.decode(bytes(row))),
  };
}

// ---------------------------------------------------------------------------
// Layout serialization (UI-owned; wraps dockview's opaque blob)
// ---------------------------------------------------------------------------

export interface LayoutSpec {
  schema: 3;
  id: "modern" | "classic" | "classic-sidebar" | "focus" | `user:${string}`;
  name: string;
  basedOn?: string;
  chrome: { topBar: "full" | "compact" | "auto-hide"; statusBar: boolean };
  defaults: {
    inlineResults: "always" | "editor-run" | "compact" | "off";
    docView: boolean;
    commandBar: "docked-bottom" | "overlay" | "pane";
    theme?: "light" | "dark" | "system";
  };
  windows: WindowSpec[]; // [0] is always the main window
  panes: Partial<Record<PaneId, unknown>>;
}

export interface WindowSpec {
  role: WindowRole;
  label: string; // `${project}:${role}[:${instance}]`
  bounds?: { x: number; y: number; w: number; h: number; monitor?: string };
  dock: unknown; // dockview's SerializedDockview, opaque
}

export type WindowRole = "main" | "editor" | "data" | "graph" | "pane" | "viewer" | "prefs";

export type PaneId =
  | "editor"
  | "results"
  | "history"
  | "variables"
  | "properties"
  | "project"
  | "assistant"
  | "graphs"
  | "compare"
  | "dataeditor"
  | "sections"
  | "viewer"
  | "repro";

/** The 13 `PaneId`s as a value, for validation and for `Mod+1..9` ordering. */
export const PANE_IDS = [
  "editor",
  "results",
  "history",
  "variables",
  "properties",
  "project",
  "assistant",
  "graphs",
  "compare",
  "dataeditor",
  "sections",
  "viewer",
  "repro",
] as const satisfies readonly PaneId[];

export function isPaneId(s: string): s is PaneId {
  return (PANE_IDS as readonly string[]).includes(s);
}

export type InlineResultsMode = LayoutSpec["defaults"]["inlineResults"];
export type ThemeChoice = NonNullable<LayoutSpec["defaults"]["theme"]>;

// ---------------------------------------------------------------------------
// The durable sidecar
// ---------------------------------------------------------------------------

/** The durable sidecar (committed). Sorted keys, LF, NO timestamps, NO output. */
export interface DurableSidecar {
  schema: 1;
  sections: { id: number; title: string; span: [number, number] }[];
  collapsed: CodeHash[]; // collapse INTENT, keyed by code hash
  inlineResults?: LayoutSpec["defaults"]["inlineResults"];
  docView?: boolean;
  pinnedComparisons: { name: string; results: string[] }[];
  autoCommentAnchors: { blockHash: CodeHash; commentHash: CodeHash }[];
  aiConversations: { blockHash: CodeHash; conversationId: string }[];
  eol: "lf" | "crlf";
  bom: boolean;
}
