/**
 * Paging — the only way frame bytes reach this pane.
 *
 * **One transport (A13).** `stratum-asset://localhost/frame/{session}/{frame}/page`
 * and nothing else. The `data_page` Tauri command is deleted from CONTRACTS §11
 * "because scrolling needs `AbortController` cancellation and HTTP caching, and
 * a Tauri command gives neither", and this file is the consumer that made that
 * true: a superseded page fetch is aborted the moment the window moves off it,
 * and the URL is the cache key.
 *
 * **The permutation never crosses the wire (A13).** Sorting and filtering are a
 * DECLARATION — `data_order_set` with an `OrderSpec` — answered with an
 * `OrderId(u32)` that every subsequent page request carries as seven characters
 * of query string. The pre-audit `PageRequest.order: Option<Vec<u64>>` meant
 * 80 MB of JSON per 40-row fetch on a sorted 10 M-row view. `maxRequestBytes`
 * is asserted below 4 KB at any dataset size, which is the counter form of that
 * amendment.
 *
 * **Scrolling never waits on data.** `cell()` is synchronous and returns
 * `undefined` for a row whose page is in flight; `paint.ts` draws `⋯` there.
 * Nothing on the scroll path awaits anything.
 *
 * Pages are cut on a fixed grid of `pageRows` observations and on fixed BANDS of
 * columns, so scrolling a few rows or a few columns re-uses what is already
 * resident instead of asking for a window nobody has. That is what keeps IPC
 * round-trips per page at exactly one.
 */

import {
  type DataColumn,
  type DataPage,
  type DatasetStateId,
  type SessionId,
  decodeDataPage,
} from "../ipc/hand";
import { type PageQuery, framePageUrl } from "../platform/asset";
import { bridge } from "../platform/bridge";
import { type GridColumn, counters } from "./engine";

/** `RenderMode` (CONTRACTS §8.1) as the query string spells it. */
export type RenderMode = "display" | "edit";

/**
 * Columns are requested in bands of 32.
 *
 * A band is the unit of "we already have these columns". Requesting exactly the
 * visible columns would refetch the whole viewport on a one-pixel horizontal
 * scroll; requesting all 32 767 of `wide.dta` would put 200 KB of column indices
 * in a URL that must stay under 4 KB. 32 is one viewport's worth of narrow
 * columns, so a horizontal scroll usually costs nothing and never costs more
 * than three bands.
 */
export const COLUMN_BAND = 32;

/** 06 §15.3: "Prefetch is 3 viewports in the scroll direction." */
export const PREFETCH_VIEWPORTS = 3;

/**
 * The ceiling this pane's requests are asserted against.
 *
 * The plan's words: "A test asserts no request payload from this pane ever
 * exceeds 4 KB, at any dataset size."
 */
export const MAX_REQUEST_BYTES = 4096;

/**
 * Longest `if` expression the filter panel will send.
 *
 * Not a dataset-size bound — a user can paste anything — but the 4 KB ceiling is
 * a property of the pane, not of the dataset, so the one field that carries
 * free text has to have a bound. 2 KB is longer than any `if` in the golden
 * logs by two orders of magnitude, and the panel refuses beyond it rather than
 * silently truncating a filter, which would show the wrong observations.
 */
export const MAX_FILTER_BYTES = 2048;

export interface PageSourceOptions {
  session: SessionId;
  frame: string;
  render?: RenderMode;
  /** Observations per page. 06 §15.1 budgets a 60×40 window at 12 ms. */
  pageRows?: number;
  /** Resident decoded pages. Bytes are cheap; a refetch on a scroll-back is not. */
  maxResidentPages?: number;
  /** Called when a page lands, so the host can repaint. Never called synchronously. */
  onPage?: (page: DataPage) => void;
  /** Called when the engine answers with a `state` the UI is not showing. */
  onStateAdvanced?: (state: DatasetStateId) => void;
  onError?: (error: unknown) => void;
  /** Test seam. Production leaves it undefined and the bridge is used. */
  fetchAsset?: (url: string, init?: { signal?: AbortSignal }) => Promise<Response>;
}

/** One cell as the wire actually carries it, for the editor's overlay input. */
export type RawCell =
  | { kind: "text"; text: string }
  | { kind: "num"; value: number; tag: number }
  | { kind: "blob"; bytes: Uint8Array; binary: boolean };

const utf8 = new TextEncoder();

/**
 * A row-and-band window of one frame, resident or in flight.
 *
 * The cache is an LRU over decoded `DataPage`s. Decoding is zero-copy
 * (`hand.ts`), so a resident page costs its bytes and twelve column descriptors.
 */
export class PageSource {
  private readonly session: SessionId;
  private frameName: string;
  private readonly render: RenderMode;
  private readonly pageRows: number;
  private readonly maxResident: number;
  private readonly options: PageSourceOptions;

  private columnList: readonly GridColumn[] = [];
  private state: DatasetStateId = 0 as DatasetStateId;
  private order: number | undefined;
  private seq = 0;

  /** Insertion-ordered, so the oldest key is the LRU victim. */
  private readonly resident = new Map<string, DataPage>();
  private readonly inflight = new Map<string, AbortController>();
  /** The newest `seq` issued for a key; an older response is stale. */
  private readonly issued = new Map<string, number>();

  /** Band range currently being paged, recomputed only when it moves. */
  private band0 = 0;
  private band1 = 0;
  private prefix = "";

  constructor(options: PageSourceOptions) {
    this.session = options.session;
    this.frameName = options.frame;
    this.render = options.render ?? "display";
    this.pageRows = Math.max(1, options.pageRows ?? 200);
    this.maxResident = Math.max(2, options.maxResidentPages ?? 24);
    this.options = options;
    this.recomputePrefix();
  }

  get frame(): string {
    return this.frameName;
  }

  get datasetState(): DatasetStateId {
    return this.state;
  }

  get orderId(): number | undefined {
    return this.order;
  }

  get residentPages(): number {
    return this.resident.size;
  }

  get inflightPages(): number {
    return this.inflight.size;
  }

  setColumns(columns: readonly GridColumn[]): void {
    this.columnList = columns;
  }

  /**
   * Points the source at a new snapshot, order or frame.
   *
   * Every one of these changes the key prefix, so nothing resident is thrown
   * away — a user who sorts, looks, and unsorts finds the dataset-order pages
   * still there. What IS thrown away is every in-flight request under the old
   * prefix: those answers can no longer be shown, and holding the connection
   * open for them is the "superseded fetch" A13 requires be cancelled.
   */
  retarget(next: {
    frame?: string;
    state?: DatasetStateId;
    order?: number | undefined;
  }): void {
    if (next.frame !== undefined) this.frameName = next.frame;
    if (next.state !== undefined) this.state = next.state;
    if ("order" in next) this.order = next.order;
    this.abortAll();
    this.recomputePrefix();
  }

  /** The dataset advanced under us: nothing resident describes it any more. */
  invalidate(state: DatasetStateId): void {
    this.state = state;
    this.resident.clear();
    this.issued.clear();
    this.abortAll();
    this.recomputePrefix();
  }

  dispose(): void {
    this.abortAll();
    this.resident.clear();
    this.issued.clear();
  }

  private abortAll(): void {
    for (const controller of this.inflight.values()) {
      controller.abort();
      counters.pageAborts += 1;
    }
    this.inflight.clear();
  }

  private recomputePrefix(): void {
    this.prefix = `${this.frameName}|${this.render}|${this.state}|${this.order ?? "-"}|${this.band0}-${this.band1}`;
  }

  private pageIndexOf(row: number): number {
    return Math.floor(row / this.pageRows);
  }

  private keyFor(pageIndex: number): string {
    return `${this.prefix}|${pageIndex}`;
  }

  // -- reading --------------------------------------------------------------

  /**
   * The formatted text of one cell, or `undefined` when its page is in flight.
   *
   * Two `Map` lookups and one arena slice. No allocation beyond the string the
   * decoder builds, and none at all for a `pending` cell.
   */
  cell(row: number, column: GridColumn): string | undefined {
    const page = this.resident.get(this.keyFor(this.pageIndexOf(row)));
    if (page === undefined) return undefined;
    const col = page.column(column.idx);
    if (col === undefined) return undefined;
    const local = row - page.row0;
    if (local < 0 || local >= page.nrows) return undefined;
    return textOf(col, local);
  }

  /** The wire value of one cell, for the edit overlay. `undefined` if not resident. */
  raw(row: number, column: GridColumn): RawCell | undefined {
    const page = this.resident.get(this.keyFor(this.pageIndexOf(row)));
    if (page === undefined) return undefined;
    const col = page.column(column.idx);
    if (col === undefined) return undefined;
    const local = row - page.row0;
    if (local < 0 || local >= page.nrows) return undefined;
    switch (col.kind) {
      case "text":
        return { kind: "text", text: col.cell(local) };
      case "num":
        return { kind: "num", value: col.values[local] ?? Number.NaN, tag: col.tags[local] ?? 255 };
      case "blob":
        return { kind: "blob", bytes: col.bytes(local), binary: col.isBinary(local) };
    }
  }

  /** True when every row of `[row0, row0+count)` is resident. */
  isResident(row0: number, count: number): boolean {
    for (let p = this.pageIndexOf(row0); p <= this.pageIndexOf(row0 + count - 1); p++) {
      if (!this.resident.has(this.keyFor(p))) return false;
    }
    return true;
  }

  // -- requesting -----------------------------------------------------------

  /**
   * Ensures the window (plus its prefetch) is resident or in flight, and aborts
   * everything that is neither.
   *
   * Called once per scroll frame. It is O(pages in the window), which is a
   * single-digit number, and it issues at most one `fetch` per page — the
   * "IPC round-trips per page" counter.
   */
  ensure(
    row0: number,
    rowCount: number,
    col0: number,
    colCount: number,
    direction: number,
    totalRows: number,
  ): void {
    if (this.columnList.length === 0 || rowCount <= 0) return;

    const band0 = Math.floor(col0 / COLUMN_BAND);
    const band1 = Math.floor(Math.max(col0, col0 + colCount - 1) / COLUMN_BAND);
    if (band0 !== this.band0 || band1 !== this.band1) {
      this.band0 = band0;
      this.band1 = band1;
      // The bands moved: in-flight answers describe columns that are no longer
      // on screen. Abort them rather than pay for bytes nobody will draw.
      this.abortAll();
      this.recomputePrefix();
    }

    const first = this.pageIndexOf(row0);
    const last = this.pageIndexOf(row0 + rowCount - 1);
    const reach = Math.max(1, Math.ceil((rowCount * PREFETCH_VIEWPORTS) / this.pageRows));
    const lastPage = Math.max(0, this.pageIndexOf(Math.max(0, totalRows - 1)));

    // Prefetch only in the direction of travel. A user scrolling down has no use
    // for the three viewports above them, and fetching both ways doubles the
    // bytes for no shortened wait.
    const lo = Math.max(0, direction < 0 ? first - reach : first);
    const hi = Math.min(lastPage, direction > 0 ? last + reach : last);

    for (const [key, controller] of this.inflight) {
      const idx = Number(key.slice(key.lastIndexOf("|") + 1));
      if (!key.startsWith(`${this.prefix}|`) || idx < lo || idx > hi) {
        controller.abort();
        counters.pageAborts += 1;
        this.inflight.delete(key);
      }
    }

    // The visible pages first, then the prefetch: on a fast scroll the visible
    // request must be the one that is already open when the others are aborted.
    for (let p = first; p <= last; p++) this.request(p, totalRows);
    for (let p = lo; p <= hi; p++) {
      if (p < first || p > last) this.request(p, totalRows);
    }
  }

  private request(pageIndex: number, totalRows: number): void {
    const key = this.keyFor(pageIndex);
    if (this.resident.has(key)) {
      counters.pageCacheHits += 1;
      this.touch(key);
      return;
    }
    if (this.inflight.has(key)) return;

    const row0 = pageIndex * this.pageRows;
    if (row0 >= totalRows) return;
    const nrows = Math.min(this.pageRows, totalRows - row0);

    const seq = ++this.seq;
    this.issued.set(key, seq);

    const query: PageQuery = {
      state: this.state,
      row0,
      nrows,
      cols: this.bandColumns(),
      render: this.render,
      seq,
      ...(this.order === undefined ? {} : { order: this.order }),
    };
    const url = framePageUrl(this.session, this.frameName, query);
    // The query string IS the request payload for a GET. This is the number the
    // 4 KB assertion is about, and recording it here means it is recorded for
    // every request rather than for the ones a test remembered to look at.
    counters.maxRequestBytes = Math.max(counters.maxRequestBytes, utf8.encode(url).byteLength);

    const controller = new AbortController();
    this.inflight.set(key, controller);
    counters.pageRequests += 1;

    const fetcher = this.options.fetchAsset ?? ((u, init) => bridge().fetchAsset(u, init));
    void fetcher(url, { signal: controller.signal })
      .then(async (response) => {
        if (!response.ok) throw new Error(`frame page ${response.status}`);
        return response.arrayBuffer();
      })
      .then((buffer) => {
        if (controller.signal.aborted) return;
        if (this.issued.get(key) !== seq) {
          counters.staleResponses += 1;
          return;
        }
        const page = decodeDataPage(buffer);
        counters.pagesDecoded += 1;
        if (page.seq !== seq) {
          counters.staleResponses += 1;
          return;
        }
        if (page.state !== this.state) {
          // Exactly CONTRACTS §8.1's rule: "If the frame has advanced, the
          // response's `state` differs and the UI invalidates."
          counters.staleResponses += 1;
          this.options.onStateAdvanced?.(page.state);
          return;
        }
        this.store(key, page);
        this.options.onPage?.(page);
      })
      .catch((error: unknown) => {
        // An abort is the design working, not a failure. Everything else is one.
        if (controller.signal.aborted) return;
        this.options.onError?.(error);
      })
      .finally(() => {
        if (this.inflight.get(key) === controller) this.inflight.delete(key);
      });
  }

  /** The `VarIdx` list for the current band range. Bounded by `3 × COLUMN_BAND`. */
  private bandColumns(): number[] {
    const from = this.band0 * COLUMN_BAND;
    const to = Math.min(this.columnList.length, (this.band1 + 1) * COLUMN_BAND);
    const out: number[] = [];
    for (let i = from; i < to; i++) {
      const column = this.columnList[i];
      if (column !== undefined) out.push(column.idx);
    }
    return out;
  }

  private store(key: string, page: DataPage): void {
    this.resident.set(key, page);
    while (this.resident.size > this.maxResident) {
      const oldest = this.resident.keys().next();
      if (oldest.done === true) break;
      this.resident.delete(oldest.value);
    }
  }

  private touch(key: string): void {
    const page = this.resident.get(key);
    if (page === undefined) return;
    this.resident.delete(key);
    this.resident.set(key, page);
  }
}

/** Display text for one already-decoded cell, whatever its column kind. */
function textOf(col: DataColumn, row: number): string {
  switch (col.kind) {
    case "text":
      return col.cell(row);
    case "blob":
      // `undefined` means the bitmap marked the row GSO type 129. A binary strL
      // has no text and must never be shown as mojibake (CONTRACTS §8.1).
      return col.cell(row) ?? "«binary»";
    case "num": {
      // A `num` column only reaches the grid in `RenderMode::Edit`. Formatting
      // belongs to the core, so the honest rendering of a raw f64 here is the
      // missing token when it is missing and the plain value when it is not.
      const tag = col.tags[row] ?? 255;
      if (tag !== 255) return missingToken(tag);
      const value = col.values[row];
      return value === undefined ? "" : String(value);
    }
  }
}

/**
 * `0` → `.`, `1..=26` → `.a`..`.z` — CONTRACTS §8.1's tag encoding, and exactly
 * how Stata prints them (`tests/golden/stata18/semantics.log`).
 */
export function missingToken(tag: number): string {
  if (tag === 0) return ".";
  if (tag >= 1 && tag <= 26) return `.${String.fromCharCode(96 + tag)}`;
  return ".";
}

/** `.` → 0, `.a` → 1 … `.z` → 26; `undefined` when the text is not a missing. */
export function missingTag(text: string): number | undefined {
  if (text === ".") return 0;
  if (/^\.[a-z]$/.test(text)) return text.charCodeAt(1) - 96;
  return undefined;
}
