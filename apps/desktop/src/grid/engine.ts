/**
 * The grid engine — geometry, the scroll model and the visible window.
 *
 * 06 §15.3 sets the problem: 10 M rows × 12 columns, 60 fps sustained, and
 * "scrolling never waits on data". Everything in this file exists to make the
 * cost of one scroll frame a function of the VIEWPORT and of nothing else. The
 * row count appears in exactly three places — the scrollbar's thumb ratio, the
 * clamp on `scrollRow`, and `aria-rowcount` — and in none of them is it
 * iterated.
 *
 * Three decisions carry that.
 *
 * **No tall spacer element.** 06 §15.2's "33 M-pixel problem": 10 M rows × 22 px
 * is 220 M px and every engine we ship on clamps element height near 33.5 M.
 * The scroll position is an `f64` ROW INDEX (`scrollRow`), not a pixel offset,
 * so it is exact at 10 M rows and at 10 B. `scrollbar.ts` drives it.
 *
 * **Column x-positions are a prefix sum, resolved by binary search.** Horizontal
 * scrolling over 32 767 variables must not walk the column list, and column
 * widths are computed ONCE per column from the Stata display format — never by
 * measuring cells, which `design/tokens.json` §14.3 rules out explicitly ("a
 * per-column ch padding computed from the display format ... never per-cell
 * measurement").
 *
 * **The materialised window is a reused scratch buffer.** `materialize()` fills
 * two flat arrays sized `rows × cols` and reuses them across frames, so the
 * per-frame allocation count is zero once the viewport has stopped changing
 * size. `counters.scratchAllocations` is the assertion, per ADR-017: a counter,
 * never a duration.
 *
 * This module is framework-free and DOM-free on purpose. It is the half of the
 * Data Editor that can be tested without a canvas, a webview or a host.
 */

// ---------------------------------------------------------------------------
// Counters (ADR-017)
// ---------------------------------------------------------------------------

/**
 * Every counter the Data Editor asserts on, in one object.
 *
 * One object rather than one per module because the interesting assertions are
 * cross-module — "one scroll frame materialises `rows × cols` cells, issues zero
 * IPC round-trips and allocates nothing" reads across `engine`, `fetch` and
 * `paint`. Mutated in place so recording never allocates.
 */
export interface GridCounters {
  /** `visibleWindow()` evaluations. */
  windowsComputed: number;
  /** Rows whose cells were pulled out of a page. Bounded by the viewport. */
  rowsMaterialized: number;
  cellsMaterialized: number;
  /** Scratch-buffer growths. Zero per frame after the viewport settles. */
  scratchAllocations: number;
  /** Column-width layout passes. Once per column-set change, never per frame. */
  columnLayouts: number;

  /** `fetch()` calls against `stratum-asset://…/frame/…`. */
  pageRequests: number;
  /** Superseded page fetches that were aborted. */
  pageAborts: number;
  pageCacheHits: number;
  pagesDecoded: number;
  /** Responses dropped because `seq` or `state` had moved on. */
  staleResponses: number;
  /** `data_order_set` / `data_order_drop` round-trips. */
  orderRequests: number;
  /** Largest request payload this pane has ever produced, in bytes. */
  maxRequestBytes: number;
  /** Tauri command round-trips. Must be 0 on any scroll path. */
  ipcInvocations: number;

  framesPainted: number;
  cellsPainted: number;
  /** Cells drawn as `⋯` because their page was still in flight. */
  placeholdersPainted: number;
  textMeasures: number;
  measureCacheHits: number;
  fillTextCalls: number;
  /** Objects/arrays allocated inside a paint. Zero after warmup. */
  paintAllocations: number;
  /** `getComputedStyle` reads. Once per theme change, never per frame. */
  styleReads: number;

  mirrorUpdates: number;
  mirrorNodesCreated: number;
  mirrorCellsWritten: number;

  editsBegun: number;
  editsCommitted: number;
  compositions: number;
}

const ZERO: GridCounters = {
  windowsComputed: 0,
  rowsMaterialized: 0,
  cellsMaterialized: 0,
  scratchAllocations: 0,
  columnLayouts: 0,
  pageRequests: 0,
  pageAborts: 0,
  pageCacheHits: 0,
  pagesDecoded: 0,
  staleResponses: 0,
  orderRequests: 0,
  maxRequestBytes: 0,
  ipcInvocations: 0,
  framesPainted: 0,
  cellsPainted: 0,
  placeholdersPainted: 0,
  textMeasures: 0,
  measureCacheHits: 0,
  fillTextCalls: 0,
  paintAllocations: 0,
  styleReads: 0,
  mirrorUpdates: 0,
  mirrorNodesCreated: 0,
  mirrorCellsWritten: 0,
  editsBegun: 0,
  editsCommitted: 0,
  compositions: 0,
};

/** Live counters. Mutated in place so a hot path never allocates to record. */
export const counters: GridCounters = { ...ZERO };

export function resetGridCounters(): void {
  Object.assign(counters, ZERO);
}

export function snapshotGridCounters(): GridCounters {
  return { ...counters };
}

// ---------------------------------------------------------------------------
// Columns
// ---------------------------------------------------------------------------

/**
 * How a cell's ink is chosen — 06 §9.7, "Cell ink by kind, mirroring Stata's
 * convention so the eye reads type instantly".
 *
 * `pending` is ours rather than Stata's: a cell whose page has not arrived is
 * not a value and must not be inked like one.
 */
export type CellInk = "text" | "labelled" | "numeric" | "missing" | "pending";

/** Numeric codes for `CellInk`, so the materialised window is a `Uint8Array`. */
export const INK = {
  text: 0,
  labelled: 1,
  numeric: 2,
  missing: 3,
  pending: 4,
} as const satisfies Record<CellInk, number>;

export const INK_NAMES = ["text", "labelled", "numeric", "missing", "pending"] as const;

/**
 * A variable as the grid needs it. Built from `VariableInfo` (CONTRACTS §8) by
 * `columnsFromVariables`; declared structurally so this module does not become
 * a hand-written mirror of a generated type (CONTRACTS §12).
 */
export interface GridColumn {
  /** `VarIdx` — the storage index, which is what `PageRequest.cols` names. */
  readonly idx: number;
  readonly name: string;
  /** `"str18"`, `"int"`, `"float"`, `"byte"`, `"long"`, `"double"`, `"strL"`. */
  readonly storage: string;
  /** The Stata display format, `"%8.0gc"`. */
  readonly format: string;
  readonly label?: string;
  readonly valueLabel?: string;
  readonly isString: boolean;
  /** Bytes one observation of this variable occupies. `Length:` in §9.7's bar. */
  readonly storageWidth: number;
  /** Laid out once from `format` and `name`. Never from a cell. */
  readonly width: number;
  readonly align: "left" | "right";
}

/** The structural minimum `columnsFromVariables` reads off a `VariableInfo`. */
export interface VariableLike {
  readonly idx?: number;
  readonly name: string;
  /** `StorageType` serialises as `"byte"` … or `{ str: { width } }`. */
  readonly ty?: unknown;
  /** The desktop's own `VariableRow` spells the same thing `storage: "str18"`. */
  readonly storage?: string;
  readonly format?: string;
  readonly label?: string;
  readonly valueLabel?: string;
  readonly value_label?: string;
}

/**
 * `str18` → 18, `int` → 2, `strL` → 0.
 *
 * `strL` is 0 because a `strL` observation stores an 8-byte GSO pointer and an
 * unbounded payload; Stata's own `describe` reports the type and not a width,
 * and inventing one for the status bar would be a number with no referent.
 */
export function storageWidthOf(storage: string): number {
  const str = /^str(\d+)$/.exec(storage);
  if (str !== null) return Number(str[1]);
  switch (storage) {
    case "byte":
      return 1;
    case "int":
      return 2;
    case "long":
    case "float":
      return 4;
    case "double":
      return 8;
    default:
      return 0;
  }
}

export function isStringStorage(storage: string): boolean {
  return storage === "strL" || /^str\d+$/.test(storage);
}

/** Normalises the wire's `StorageType` (either spelling) to Stata's own name. */
function storageName(v: VariableLike): string {
  if (typeof v.storage === "string") return v.storage;
  const ty = v.ty;
  if (typeof ty === "string") return ty === "strL" ? "strL" : ty;
  if (ty !== null && typeof ty === "object" && "str" in ty) {
    const inner = (ty as { str: { width?: number } }).str;
    return `str${inner?.width ?? 1}`;
  }
  return "float";
}

/**
 * The display width a Stata format asks for, in characters.
 *
 * `%8.0gc` → 8, `%-18s` → 18, `%6.1f` → 6, `%tc` → 18 (a `%tc` datetime prints
 * `01jan1960 00:00:00`, and Stata's own default width for it is what the eye
 * needs). Anything unparseable falls back to 9, Stata's `%9.0g` default.
 */
export function formatWidth(format: string): number {
  const m = /^%-?(\d+)/.exec(format);
  if (m !== null) return Number(m[1]);
  if (/^%-?t[cC]/.test(format)) return 18;
  if (/^%-?t/.test(format)) return 9;
  return 9;
}

/** Layout constants. `chPx` is measured once by the surface, never per cell. */
export interface GridMetrics {
  rowHeight: number;
  headerHeight: number;
  /** The observation-number gutter on the left. */
  gutterWidth: number;
  cellPadX: number;
  /** Advance width of one monospace character at the grid's font size. */
  chPx: number;
  minColumnWidth: number;
  maxColumnWidth: number;
}

export const DEFAULT_METRICS: GridMetrics = {
  rowHeight: 22,
  headerHeight: 26,
  gutterWidth: 56,
  cellPadX: 8,
  chPx: 7.8,
  minColumnWidth: 48,
  maxColumnWidth: 360,
};

/**
 * Builds the grid's columns from the variable list.
 *
 * Width is `max(name, format) ch + padding`, clamped. That is deliberately not
 * "the widest cell": the widest cell of a 10 M-row column is unknowable without
 * reading 10 M rows, and a column that resizes as you scroll past a long value
 * is worse than one that is occasionally too narrow.
 */
export function columnsFromVariables(
  vars: readonly VariableLike[],
  metrics: GridMetrics = DEFAULT_METRICS,
): GridColumn[] {
  counters.columnLayouts += 1;
  return vars.map((v, i) => {
    const storage = storageName(v);
    const format = v.format ?? "%9.0g";
    const isString = isStringStorage(storage);
    const chars = Math.max(v.name.length, formatWidth(format));
    const width = Math.min(
      metrics.maxColumnWidth,
      Math.max(metrics.minColumnWidth, Math.ceil(chars * metrics.chPx) + metrics.cellPadX * 2),
    );
    const valueLabel = v.valueLabel ?? v.value_label;
    const column: GridColumn = {
      idx: v.idx ?? i,
      name: v.name,
      storage,
      format,
      isString,
      storageWidth: storageWidthOf(storage),
      width,
      // Stata right-aligns numerics and left-aligns strings, and `%-18s`'s minus
      // sign says the same thing about this variable in particular.
      align: isString || format.startsWith("%-") ? "left" : "right",
      ...(v.label === undefined || v.label === "" ? {} : { label: v.label }),
      ...(valueLabel === undefined || valueLabel === "" ? {} : { valueLabel }),
    };
    return column;
  });
}

// ---------------------------------------------------------------------------
// Cell classification
// ---------------------------------------------------------------------------

/**
 * Stata's missing values as `RenderMode::Display` prints them: `.` and `.a`–`.z`
 * (`tests/golden/stata18/semantics.log`, the `list x` block).
 */
const MISSING_RE = /^\.[a-z]?$/;

/**
 * A formatted number, including `%8.0gc`'s comma grouping and `%10.0e`'s
 * exponent. Used only to tell a substituted value LABEL from the numeric
 * fallback in a labelled column — fixture README §2.4: "A labelled value with no
 * matching label entry falls back to the formatted number."
 */
const NUMERIC_RE = /^[-+]?[\d,]*\.?\d+(?:[eE][-+]?\d+)?$/;

/**
 * The ink for one already-formatted cell.
 *
 * Everything here is decided from the DISPLAY text plus the column's metadata,
 * because that is all a `RenderMode::Display` page carries and formatting
 * belongs to the core (CONTRACTS §8.1: "Formatting happens in the CORE, so
 * `list`, the Data Editor and the inline cards cannot disagree"). We do not
 * re-derive the value; we classify what the core already decided to show.
 */
export function inkFor(column: GridColumn, text: string | undefined): CellInk {
  if (text === undefined) return "pending";
  if (column.isString) return "text";
  if (MISSING_RE.test(text)) return "missing";
  if (column.valueLabel !== undefined && !NUMERIC_RE.test(text)) return "labelled";
  return "numeric";
}

/** True when a `RenderMode::Display` cell of a numeric column is a missing value. */
export function isMissingText(text: string): boolean {
  return MISSING_RE.test(text);
}

// ---------------------------------------------------------------------------
// The visible window
// ---------------------------------------------------------------------------

export interface VisibleWindow {
  /** First visible observation, 0-based and absolute. */
  row0: number;
  /** Rows to draw, including the partial one at each edge. */
  rowCount: number;
  /** Index into `columns` of the first visible column. */
  col0: number;
  colCount: number;
  /** Fractional-row offset, in px, of `row0`'s top edge above the body origin. */
  yOffset: number;
  /** Fractional-column offset, in px, of `col0`'s left edge. */
  xOffset: number;
}

/** What the engine reads to materialise a window. `fetch.ts`'s `PageSource` is one. */
export interface CellSource {
  /**
   * The formatted text of `(row, column)`, or `undefined` when the page holding
   * it has not arrived. `undefined` is the placeholder signal and is normal:
   * scrolling never waits on data.
   */
  cell(row: number, column: GridColumn): string | undefined;
}

/** A materialised window. Both arrays are the engine's scratch and are reused. */
export interface CellWindow {
  readonly window: VisibleWindow;
  /** `rowCount × colCount`, row-major. `""` where `ink[i] === INK.pending`. */
  readonly text: string[];
  readonly ink: Uint8Array;
  readonly rows: number;
  readonly cols: number;
}

/**
 * The soft cap the DOM fallback runs under (Q8, ARCHITECTURE Q-table).
 *
 * "Documented fallback is DOM virtualisation with a 1 M-row soft cap, not a
 * stuttering grid." The cap is enforced here, in the scroll clamp, rather than
 * in the surface: a surface that could not draw row 2 000 000 but let you scroll
 * to it would be the stuttering grid the fallback exists to avoid.
 */
export const DOM_SOFT_CAP = 1_000_000;

export class GridEngine {
  metrics: GridMetrics;
  private columnList: readonly GridColumn[] = [];
  /** `x[i]` is the left edge of column `i`; `x[n]` is the total width. */
  private xs: Float64Array = new Float64Array(1);
  private rows = 0;
  private cap = Number.POSITIVE_INFINITY;
  private viewW = 0;
  private viewH = 0;
  /** The scroll position, as an f64 ROW INDEX. See the header comment. */
  private row = 0;
  private x = 0;

  // The reused scratch. Capacity grows monotonically; `scratchAllocations`
  // counts every growth, and a settled viewport grows it zero times per frame.
  private scratchText: string[] = [];
  private scratchInk = new Uint8Array(0);
  private readonly win: VisibleWindow = {
    row0: 0,
    rowCount: 0,
    col0: 0,
    colCount: 0,
    yOffset: 0,
    xOffset: 0,
  };
  private readonly cellWindow: CellWindow;

  constructor(metrics: GridMetrics = DEFAULT_METRICS) {
    this.metrics = { ...metrics };
    this.cellWindow = {
      window: this.win,
      text: this.scratchText,
      ink: this.scratchInk,
      rows: 0,
      cols: 0,
    } as CellWindow;
  }

  // -- configuration --------------------------------------------------------

  setColumns(columns: readonly GridColumn[]): void {
    this.columnList = columns;
    const xs = new Float64Array(columns.length + 1);
    let acc = 0;
    for (let i = 0; i < columns.length; i++) {
      xs[i] = acc;
      acc += columns[i]?.width ?? 0;
    }
    xs[columns.length] = acc;
    this.xs = xs;
    this.clamp();
  }

  get columns(): readonly GridColumn[] {
    return this.columnList;
  }

  /** Total observations in the view. May be 10 M; is never iterated. */
  setRowCount(n: number): void {
    this.rows = Math.max(0, Math.floor(n));
    this.clamp();
  }

  get rowCount(): number {
    return this.rows;
  }

  /** The number of rows actually reachable — `rowCount` unless the cap bites. */
  get reachableRows(): number {
    return Math.min(this.rows, this.cap);
  }

  get capped(): boolean {
    return this.rows > this.cap;
  }

  setSoftCap(cap: number): void {
    this.cap = cap;
    this.clamp();
  }

  setViewport(width: number, height: number): void {
    this.viewW = Math.max(0, width);
    this.viewH = Math.max(0, height);
    this.clamp();
  }

  get viewportWidth(): number {
    return this.viewW;
  }

  get viewportHeight(): number {
    return this.viewH;
  }

  /** Height of the region rows are drawn into, below the header. */
  get bodyHeight(): number {
    return Math.max(0, this.viewH - this.metrics.headerHeight);
  }

  /** Width of the region cells are drawn into, right of the row-number gutter. */
  get bodyWidth(): number {
    return Math.max(0, this.viewW - this.metrics.gutterWidth);
  }

  /** Whole rows that fit. The window draws one more to cover the partial edge. */
  get visibleRowCount(): number {
    return Math.floor(this.bodyHeight / this.metrics.rowHeight);
  }

  get totalColumnWidth(): number {
    return this.xs[this.xs.length - 1] ?? 0;
  }

  // -- scrolling ------------------------------------------------------------

  get scrollRow(): number {
    return this.row;
  }

  get scrollX(): number {
    return this.x;
  }

  /** The largest legal `scrollRow`. Keeps the last row reachable, not centred. */
  get maxScrollRow(): number {
    return Math.max(0, this.reachableRows - this.visibleRowCount);
  }

  get maxScrollX(): number {
    return Math.max(0, this.totalColumnWidth - this.bodyWidth);
  }

  scrollToRow(row: number): boolean {
    const next = Math.min(this.maxScrollRow, Math.max(0, row));
    if (next === this.row) return false;
    this.row = next;
    return true;
  }

  scrollByRows(delta: number): boolean {
    return this.scrollToRow(this.row + delta);
  }

  scrollToX(px: number): boolean {
    const next = Math.min(this.maxScrollX, Math.max(0, px));
    if (next === this.x) return false;
    this.x = next;
    return true;
  }

  scrollByX(delta: number): boolean {
    return this.scrollToX(this.x + delta);
  }

  /** Brings `(row, col)` into view with the minimum movement. Used by selection. */
  revealCell(row: number, col: number): boolean {
    let moved = false;
    if (row < Math.ceil(this.row)) moved = this.scrollToRow(row) || moved;
    else if (row >= this.row + this.visibleRowCount) {
      moved = this.scrollToRow(row - this.visibleRowCount + 1) || moved;
    }
    const left = this.xs[col];
    const width = this.columnList[col]?.width;
    if (left === undefined || width === undefined) return moved;
    if (left < this.x) moved = this.scrollToX(left) || moved;
    else if (left + width > this.x + this.bodyWidth) {
      moved = this.scrollToX(left + width - this.bodyWidth) || moved;
    }
    return moved;
  }

  private clamp(): void {
    this.row = Math.min(this.maxScrollRow, Math.max(0, this.row));
    this.x = Math.min(this.maxScrollX, Math.max(0, this.x));
  }

  // -- geometry -------------------------------------------------------------

  /** Left edge of column `i` in content space. O(1). */
  columnLeft(i: number): number {
    return this.xs[i] ?? 0;
  }

  /**
   * The column at content-space x, by binary search over the prefix sums.
   *
   * O(log k) rather than O(k) because a horizontal scroll is an interaction path
   * and `wide.dta` is in the fixtures for a reason.
   */
  columnAt(contentX: number): number {
    const xs = this.xs;
    let lo = 0;
    let hi = xs.length - 1;
    if (contentX < 0) return 0;
    if (contentX >= (xs[hi] ?? 0)) return Math.max(0, hi - 1);
    while (lo + 1 < hi) {
      const mid = (lo + hi) >> 1;
      if ((xs[mid] ?? 0) <= contentX) lo = mid;
      else hi = mid;
    }
    return lo;
  }

  /** Viewport-space rect of a cell, for the IME overlay and the selection paint. */
  cellRect(row: number, col: number): { x: number; y: number; w: number; h: number } {
    const m = this.metrics;
    return {
      x: m.gutterWidth + (this.xs[col] ?? 0) - this.x,
      y: m.headerHeight + (row - this.row) * m.rowHeight,
      w: this.columnList[col]?.width ?? 0,
      h: m.rowHeight,
    };
  }

  /** The cell under a viewport-space point, or `undefined` over chrome. */
  hitTest(px: number, py: number): { row: number; col: number } | undefined {
    const m = this.metrics;
    if (py < m.headerHeight || px < m.gutterWidth) return undefined;
    const row = Math.floor(this.row + (py - m.headerHeight) / m.rowHeight);
    if (row < 0 || row >= this.reachableRows) return undefined;
    const col = this.columnAt(px - m.gutterWidth + this.x);
    if (col >= this.columnList.length) return undefined;
    return { row, col };
  }

  /**
   * The window to draw. Mutated in place and returned by reference: one object
   * for the life of the engine, so reading the window costs no allocation.
   */
  visibleWindow(): VisibleWindow {
    counters.windowsComputed += 1;
    const m = this.metrics;
    const w = this.win;
    w.row0 = Math.floor(this.row);
    w.yOffset = -(this.row - w.row0) * m.rowHeight;
    // +1 for the fractional row at the top, +1 for the partial row at the bottom.
    w.rowCount = Math.max(0, Math.min(this.reachableRows - w.row0, this.visibleRowCount + 2));

    w.col0 = this.columnAt(this.x);
    w.xOffset = (this.xs[w.col0] ?? 0) - this.x;
    let count = 0;
    let span = w.xOffset;
    while (w.col0 + count < this.columnList.length && span < this.bodyWidth) {
      span += this.columnList[w.col0 + count]?.width ?? 0;
      count += 1;
    }
    w.colCount = count;
    return w;
  }

  /**
   * Pulls the visible window's cells out of `source` into the scratch buffers.
   *
   * This is the "rows materialized per scroll" counter's home. It touches
   * `rowCount × colCount` cells and no others; `counters.rowsMaterialized` is
   * asserted to be independent of `rowCount()` in `grid.perf.test.ts`.
   */
  materialize(source: CellSource): CellWindow {
    const w = this.visibleWindow();
    const need = w.rowCount * w.colCount;
    if (this.scratchInk.length < need) {
      counters.scratchAllocations += 1;
      // Grow with slack so a one-pixel resize does not reallocate every frame.
      const cap = Math.max(need * 2, 256);
      this.scratchInk = new Uint8Array(cap);
      this.scratchText = new Array<string>(cap).fill("");
    }
    const text = this.scratchText;
    const ink = this.scratchInk;

    for (let r = 0; r < w.rowCount; r++) {
      const row = w.row0 + r;
      const base = r * w.colCount;
      for (let c = 0; c < w.colCount; c++) {
        const column = this.columnList[w.col0 + c];
        if (column === undefined) continue;
        const value = source.cell(row, column);
        text[base + c] = value ?? "";
        ink[base + c] = INK[inkFor(column, value)];
      }
    }
    counters.rowsMaterialized += w.rowCount;
    counters.cellsMaterialized += need;

    const out = this.cellWindow as {
      -readonly [K in keyof CellWindow]: CellWindow[K];
    };
    out.text = text;
    out.ink = ink;
    out.rows = w.rowCount;
    out.cols = w.colCount;
    return this.cellWindow;
  }
}
