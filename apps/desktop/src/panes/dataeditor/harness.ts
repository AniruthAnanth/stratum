/**
 * Test fixtures for the Data Editor.
 *
 * Two things live here and they are deliberately different in kind.
 *
 * **The oracle.** `tests/fixtures/sdp1/auto_40x12.bin` is W00's, captured from
 * StataNow 18.5 MP, and every claim this unit makes about how Stata DISPLAYS a
 * value — `4,099` with its comma, `3.0` with its trailing zero, `Domestic`
 * rather than `0`, `.` for a missing `rep78` — is checked against those bytes
 * and never against our own formatter. If our output disagrees with it, we are
 * wrong.
 *
 * **The synthetic server.** A 10 M-row frame is not a fixture anybody is going
 * to commit, so `frameServer` answers a page request of any size by cycling the
 * oracle's forty observations. It is an SDP1 WRITER — a second implementation of
 * the format written from `CONTRACTS.md` §8.1 and the fixture README's four
 * rulings — and `harness.test.ts` proves it agrees with the committed bytes
 * before anything else uses it. A synthetic server that quietly disagreed with
 * the wire format would make every counter below meaningless.
 */

import { existsSync, readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { CanvasSurface, type GridPaintContext } from "../../grid/paint";
import { type DataPage, decodeDataPage } from "../../ipc/hand";

const repoRoot = ((): string => {
  let dir = process.cwd();
  while (!existsSync(resolve(dir, "tests/fixtures/sdp1"))) {
    const parent = dirname(dir);
    if (parent === dir) throw new Error("tests/fixtures/sdp1 not found above the cwd");
    dir = parent;
  }
  return dir;
})();

export function fixtureBytes(name: string): ArrayBuffer {
  const bytes = readFileSync(resolve(repoRoot, "tests/fixtures/sdp1", name));
  return bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength) as ArrayBuffer;
}

/** `auto_40x12.bin` decoded: 40 observations × 12 `RenderMode::Display` columns. */
export const autoDisplay: DataPage = decodeDataPage(fixtureBytes("auto_40x12.bin"));
/** `auto_40x12_edit.bin` decoded: the same page as raw values plus missing tags. */
export const autoEdit: DataPage = decodeDataPage(fixtureBytes("auto_40x12_edit.bin"));

/**
 * `auto.dta`'s twelve variables, from the fixture README's own table.
 *
 * Transcribed rather than fetched because `variables_list` is the engine's and
 * this unit does not run one. The storage types and formats are the same ones
 * `tests/golden/stata18/core_surface.log`'s `describe` prints.
 */
export const AUTO_VARS = [
  { idx: 0, name: "make", storage: "str18", format: "%-18s", label: "Make and model" },
  { idx: 1, name: "price", storage: "int", format: "%8.0gc", label: "Price" },
  { idx: 2, name: "mpg", storage: "int", format: "%8.0g", label: "Mileage (mpg)" },
  { idx: 3, name: "rep78", storage: "int", format: "%8.0g", label: "Repair record 1978" },
  { idx: 4, name: "headroom", storage: "float", format: "%6.1f", label: "Headroom (in.)" },
  { idx: 5, name: "trunk", storage: "int", format: "%8.0g", label: "Trunk space (cu. ft.)" },
  { idx: 6, name: "weight", storage: "int", format: "%8.0gc", label: "Weight (lbs.)" },
  { idx: 7, name: "length", storage: "int", format: "%8.0g", label: "Length (in.)" },
  { idx: 8, name: "turn", storage: "int", format: "%8.0g", label: "Turn circle (ft.)" },
  {
    idx: 9,
    name: "displacement",
    storage: "int",
    format: "%8.0g",
    label: "Displacement (cu. in.)",
  },
  { idx: 10, name: "gear_ratio", storage: "float", format: "%6.2f", label: "Gear ratio" },
  {
    idx: 11,
    name: "foreign",
    storage: "byte",
    format: "%8.0g",
    label: "Car origin",
    valueLabel: "origin",
  },
] as const;

// ---------------------------------------------------------------------------
// An SDP1 writer
// ---------------------------------------------------------------------------

interface WireColumn {
  idx: number;
  kind: "text" | "num";
  off: number;
  len: number;
  aux_off: number;
  aux_len: number;
}

const align = (n: number, to: number): number => (n % to === 0 ? n : n + (to - (n % to)));

/** One column as the writer takes it: `text` cells, or `num` values plus tags. */
export type PageColumnSpec =
  | { idx: number; cells: string[] }
  | { idx: number; values: number[]; tags: number[] };

export interface PageSpec {
  state: number;
  row0: number;
  seq: number;
  nrows: number;
  columns: PageColumnSpec[];
}

/**
 * Encodes one SDP1 page — either render mode, and mixed column kinds.
 *
 * Follows the fixture README exactly: compact JSON with the keys in §8.1's
 * order, space padding until `(8 + header_len) % 8 == 0`, offsets relative to
 * `8 + header_len`, columns laid out in the order given, and within a column the
 * two regions in the order §8.1's table lists them for that kind — `aux` then
 * `data` for `text` (4-aligned), `data` then `aux` for `num` (8-aligned).
 *
 * Mixed kinds are the whole reason this is one function: `auto_40x12_edit.bin`
 * is one `text` column followed by eleven `num` ones, and a writer that could
 * only do one kind at a time could not reproduce it — which
 * `harness.test.ts` requires, byte for byte, before the server is trusted to
 * answer an `edit` request.
 */
export function encodePage(opts: PageSpec): ArrayBuffer {
  const enc = new TextEncoder();
  const regions: { column: WireColumn; aux: Uint8Array; data: Uint8Array }[] = [];
  let cursor = 0;

  for (const spec of opts.columns) {
    if ("cells" in spec) {
      const encoded = spec.cells.map((c) => enc.encode(c));
      const offsets = new Uint32Array(opts.nrows + 1);
      let acc = 0;
      for (let i = 0; i < opts.nrows; i++) {
        offsets[i] = acc;
        acc += encoded[i]?.byteLength ?? 0;
      }
      offsets[opts.nrows] = acc;
      const arena = new Uint8Array(acc);
      let at = 0;
      for (const bytes of encoded) {
        arena.set(bytes, at);
        at += bytes.byteLength;
      }

      const auxOff = align(cursor, 4);
      const dataOff = auxOff + offsets.byteLength;
      cursor = dataOff + arena.byteLength;
      regions.push({
        column: {
          idx: spec.idx,
          kind: "text",
          off: dataOff,
          len: arena.byteLength,
          aux_off: auxOff,
          aux_len: offsets.byteLength,
        },
        aux: new Uint8Array(offsets.buffer),
        data: arena,
      });
      continue;
    }

    const values = Float64Array.from(spec.values);
    const tags = Uint8Array.from(spec.tags);
    const dataOff = align(cursor, 8);
    const auxOff = dataOff + values.byteLength;
    cursor = auxOff + tags.byteLength;
    regions.push({
      column: {
        idx: spec.idx,
        kind: "num",
        off: dataOff,
        len: values.byteLength,
        aux_off: auxOff,
        aux_len: tags.byteLength,
      },
      aux: tags,
      data: new Uint8Array(values.buffer),
    });
  }

  const header = `{"state":${opts.state},"row0":${opts.row0},"nrows":${opts.nrows},"seq":${opts.seq},"cols":[${regions
    .map(
      (r) =>
        `{"idx":${r.column.idx},"kind":"${r.column.kind}","off":${r.column.off},"len":${r.column.len},"aux_off":${r.column.aux_off},"aux_len":${r.column.aux_len}}`,
    )
    .join(",")}]}`;
  const headerBytes = enc.encode(header);
  let headerLen = headerBytes.byteLength;
  while ((8 + headerLen) % 8 !== 0) headerLen += 1;

  const buffer = new ArrayBuffer(8 + headerLen + cursor);
  const bytes = new Uint8Array(buffer);
  const view = new DataView(buffer);
  bytes.set(enc.encode("SDP1"), 0);
  view.setUint32(4, headerLen, true);
  bytes.set(headerBytes, 8);
  bytes.fill(0x20, 8 + headerBytes.byteLength, 8 + headerLen);

  const base = 8 + headerLen;
  for (const region of regions) {
    bytes.set(region.aux, base + region.column.aux_off);
    bytes.set(region.data, base + region.column.off);
  }
  return buffer;
}

/** {@link encodePage} for a page of `text` columns — `RenderMode::Display`. */
export function encodeDisplayPage(opts: {
  state: number;
  row0: number;
  seq: number;
  columns: { idx: number; cells: string[] }[];
  nrows: number;
}): ArrayBuffer {
  return encodePage(opts);
}

/** {@link encodePage} for a page of `num` columns — `RenderMode::Edit`. */
export function encodeEditPage(opts: {
  state: number;
  row0: number;
  seq: number;
  columns: { idx: number; values: number[]; tags: number[] }[];
  nrows: number;
}): ArrayBuffer {
  return encodePage(opts);
}

// ---------------------------------------------------------------------------
// The server
// ---------------------------------------------------------------------------

export interface ServedRequest {
  url: string;
  state: number;
  row0: number;
  nrows: number;
  cols: number[];
  order?: number;
  render: string;
  seq: number;
  aborted: boolean;
}

/**
 * Lets the page-decoding chain run to completion.
 *
 * `Response.arrayBuffer()` is backed by a stream, so the `.then` chain in
 * `PageSource.request` needs real task turns and not just microtasks. A test
 * that awaits three `Promise.resolve()`s and then asserts on a landed page is a
 * test that passes or fails on how many turns undici happens to take today.
 */
export async function settle(turns = 3): Promise<void> {
  for (let i = 0; i < turns; i++) {
    await new Promise<void>((resolve) => setTimeout(resolve, 0));
  }
}

export interface FrameServer {
  fetchAsset: (url: string, init?: { signal?: AbortSignal }) => Promise<Response>;
  requests: ServedRequest[];
  /** Resolves every held response, for the `manual` mode. */
  flush(): Promise<void>;
  aborts(): number;
}

export interface FrameServerOptions {
  /** Total observations the frame claims to have. Pages cycle the oracle's 40. */
  rows: number;
  state?: number;
  /** `manual` holds every response until `flush()`, so placeholders are testable. */
  mode?: "immediate" | "manual";
}

/**
 * A frame-page endpoint over the oracle.
 *
 * Row `r` of the synthetic frame is observation `r % 40` of `auto.dta`, so every
 * display string it serves came from Stata rather than from us — including the
 * two missing `rep78` cells, which land on rows `≡ 2, 6 (mod 40)`.
 */
export function frameServer(options: FrameServerOptions): FrameServer {
  const state = options.state ?? 17;
  const held: (() => void)[] = [];
  const requests: ServedRequest[] = [];

  const fetchAsset = (url: string, init?: { signal?: AbortSignal }): Promise<Response> => {
    const query = new URLSearchParams(url.slice(url.indexOf("?") + 1));
    const record: ServedRequest = {
      url,
      state: Number(query.get("state")),
      row0: Number(query.get("row0")),
      nrows: Number(query.get("nrows")),
      cols: (query.get("cols") ?? "").split(",").filter(Boolean).map(Number),
      render: query.get("render") ?? "display",
      seq: Number(query.get("seq")),
      aborted: false,
      ...(query.get("order") === null ? {} : { order: Number(query.get("order")) }),
    };
    requests.push(record);

    const build = (): Response => {
      // `render=edit` is answered from the EDIT oracle, so a value the pane
      // builds `replace` from is the one Stata stored (`4099`) and not the one
      // it displays (`4,099`). Serving display bytes for both modes would make
      // every edit test pass against a string that cannot be sent.
      const page = record.render === "edit" ? autoEdit : autoDisplay;
      const columns: PageColumnSpec[] = record.cols.map((idx) => {
        const source = page.column(idx);
        if (source?.kind === "num") {
          const values: number[] = [];
          const tags: number[] = [];
          for (let i = 0; i < record.nrows; i++) {
            const row = (record.row0 + i) % page.nrows;
            values.push(source.values[row] ?? Number.NaN);
            tags.push(source.tags[row] ?? 255);
          }
          return { idx, values, tags };
        }
        const cells: string[] = [];
        for (let i = 0; i < record.nrows; i++) {
          const row = (record.row0 + i) % page.nrows;
          cells.push(source?.kind === "text" ? source.cell(row) : "");
        }
        return { idx, cells };
      });
      const bytes = encodePage({
        state: record.state === 0 ? state : record.state,
        row0: record.row0,
        seq: record.seq,
        nrows: record.nrows,
        columns,
      });
      return new Response(bytes, { status: 200 });
    };

    return new Promise<Response>((resolvePromise, reject) => {
      const signal = init?.signal;
      const onAbort = (): void => {
        record.aborted = true;
        reject(new DOMException("aborted", "AbortError"));
      };
      if (signal?.aborted === true) {
        onAbort();
        return;
      }
      signal?.addEventListener("abort", onAbort, { once: true });
      const settle = (): void => {
        signal?.removeEventListener("abort", onAbort);
        if (signal?.aborted === true) return;
        resolvePromise(build());
      };
      if (options.mode === "manual") held.push(settle);
      else settle();
    });
  };

  return {
    fetchAsset,
    requests,
    async flush(): Promise<void> {
      const pending = held.splice(0, held.length);
      for (const resolveHeld of pending) resolveHeld();
      await settle();
    },
    aborts(): number {
      return requests.filter((r) => r.aborted).length;
    },
  };
}

// ---------------------------------------------------------------------------
// A 2D context that exists
// ---------------------------------------------------------------------------

/**
 * A `GridPaintContext` for a world without a canvas.
 *
 * jsdom's `HTMLCanvasElement.getContext("2d")` returns `null`, so
 * `CanvasSurface.create` correctly declines and the controller falls back to
 * `DomSurface` — which means the canvas painter, the surface 06 §15.3 actually
 * rules for, would otherwise never be executed by a test on this machine.
 *
 * It is not a mock of a canvas: it is a recorder of the calls the painter makes,
 * typed as `Pick<CanvasRenderingContext2D, …>` so it cannot drift from the real
 * signatures. `measureText` returns a plausible monospace advance rather than
 * jsdom's zero, because a zero advance makes every cell "too wide" and truncates
 * the whole grid to one ellipsis.
 */
export interface RecordingContext extends GridPaintContext {
  /** Every string drawn, in draw order, with where it went. */
  readonly texts: { text: string; x: number; y: number; align: string; fill: string }[];
  /** `stroke()` calls. 06 §15.3's "two draw calls for every rule in the grid". */
  strokes: number;
  fills: number;
  measures: number;
}

export function recordingContext(chPx = 7.2): RecordingContext {
  const texts: RecordingContext["texts"] = [];
  const ctx: RecordingContext = {
    texts,
    strokes: 0,
    fills: 0,
    measures: 0,
    fillStyle: "#000",
    strokeStyle: "#000",
    font: "",
    textAlign: "left" as CanvasTextAlign,
    textBaseline: "alphabetic" as CanvasTextBaseline,
    lineWidth: 1,
    save: () => {},
    restore: () => {},
    setTransform: () => {},
    clearRect: () => {},
    fillRect: () => {
      ctx.fills += 1;
    },
    strokeRect: () => {},
    fillText: (text: string, x: number, y: number) => {
      texts.push({
        text,
        x,
        y,
        align: String(ctx.textAlign),
        fill: String(ctx.fillStyle),
      });
    },
    measureText: (text: string) => {
      ctx.measures += 1;
      return { width: text.length * chPx } as TextMetrics;
    },
    beginPath: () => {},
    moveTo: () => {},
    lineTo: () => {},
    stroke: () => {
      ctx.strokes += 1;
    },
  };
  return ctx;
}

/** A `CanvasSurface` over {@link recordingContext}, since jsdom has no 2D context. */
export function canvasSurface(
  doc: Document,
  chPx?: number,
): { surface: CanvasSurface; ctx: RecordingContext } {
  const ctx = recordingContext(chPx);
  return { surface: new CanvasSurface(doc.createElement("canvas"), ctx), ctx };
}
