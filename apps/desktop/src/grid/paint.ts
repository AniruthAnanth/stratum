/**
 * Drawing the grid — and the Q8 fallback.
 *
 * 06 §15.3 rules: "a single `<canvas>` grid, not DOM cells ... 10 M rows × 12
 * columns cannot be DOM, and even a DOM-virtualised grid costs 2–4 ms per scroll
 * frame in layout alone; canvas costs ~0.6 ms for a 60×40 viewport."
 *
 * **Q8 (ARCHITECTURE's open-question table, item 4) lands here**: WebKitGTK's
 * canvas throughput is the one platform risk that could make that ruling false
 * on Linux, and the documented fallback is "DOM virtualisation with a 1 M-row
 * soft cap, not a stuttering grid". So this file ships BOTH surfaces behind one
 * interface, plus the probe that chooses between them; `probeCanvas` documents
 * what it measures and what it refuses to claim.
 *
 * Two properties keep a frame cheap, and both are counters rather than timings
 * (ADR-017):
 *
 * **Zero `measureText` on the cell path.** The grid font is `--font-mono`, so a
 * string's advance width is `length × chPx` exactly. `chPx` is measured once per
 * font change. `counters.textMeasures` is asserted to stay flat across a
 * thousand painted frames — a grid that measures 2 400 cells a frame is the
 * grid that drops to 20 fps on the machine you do not own.
 *
 * **Two draw calls for every rule in the grid.** All hairlines go into one path
 * and one `stroke()`.
 *
 * `paintAllocations` counts the only two things a frame can allocate: a
 * truncated cell string, and (on the DOM surface) a pooled node. Both are zero
 * once the viewport has settled over data that fits its columns.
 */

import { type CellWindow, type GridColumn, type GridEngine, INK, counters } from "./engine";
import type { SelectionModel } from "./select";

/**
 * The 2D context, narrowed to what the painter touches.
 *
 * `Pick<CanvasRenderingContext2D, …>` rather than a hand-written interface, so a
 * real context satisfies it by construction and a test double is checked
 * against the real signatures instead of against a convenient copy of them.
 */
export type GridPaintContext = Pick<
  CanvasRenderingContext2D,
  | "save"
  | "restore"
  | "setTransform"
  | "clearRect"
  | "fillRect"
  | "strokeRect"
  | "fillText"
  | "measureText"
  | "beginPath"
  | "moveTo"
  | "lineTo"
  | "stroke"
  | "fillStyle"
  | "strokeStyle"
  | "font"
  | "textAlign"
  | "textBaseline"
  | "lineWidth"
>;

/** The placeholder for a cell whose page is still in flight (06 §15.3). */
export const PLACEHOLDER = "⋯";

// ---------------------------------------------------------------------------
// Palette
// ---------------------------------------------------------------------------

/**
 * Every colour the grid draws with, resolved from `tokens.generated.css`.
 *
 * A canvas cannot inherit a custom property, so the tokens have to be READ once
 * and passed in. They are still the only source: nothing here holds a hex
 * literal, and a theme change re-reads rather than recomputes.
 *
 * Two of the roles have no token of their own, which is a gap in
 * `design/tokens.json` rather than a licence to invent colour. 06 §9.7 names
 * them by value — "strings `#B3261E`-family vermillion, value-labeled `#116A6A`
 * accent" — and those two values ARE `--state-failed` and `--accent` in every
 * theme the generated file defines. So the grid aliases them, which keeps the
 * contrast pairs CI recomputes authoritative and keeps the dark theme correct
 * for free. Flagged in this unit's return.
 */
export interface GridPalette {
  canvas: string;
  surface: string;
  border: string;
  borderStrong: string;
  headerText: string;
  gutterText: string;
  inkText: string;
  inkLabelled: string;
  inkNumeric: string;
  inkMissing: string;
  inkPending: string;
  selectionFill: string;
  selectionEdge: string;
  cellFont: string;
  headerFont: string;
  /** Advance width of one character in `cellFont`. Measured, then cached. */
  chPx: number;
}

const FALLBACK: GridPalette = {
  canvas: "#FAFAFB",
  surface: "#F4F5F6",
  border: "#E3E5E8",
  borderStrong: "#D3D6DA",
  headerText: "#5C636D",
  gutterText: "#696F79",
  inkText: "#B3261E",
  inkLabelled: "#116A6A",
  inkNumeric: "#232830",
  inkMissing: "#696F79",
  inkPending: "#696F79",
  selectionFill: "#E2F0EF",
  selectionEdge: "#116A6A",
  cellFont: '12px "IBM Plex Mono", ui-monospace, monospace',
  headerFont: '500 12px "IBM Plex Sans", system-ui, sans-serif',
  chPx: 7.2,
};

/**
 * Reads the palette off a mounted element.
 *
 * One `getComputedStyle` for the whole palette, counted, and called only when
 * the theme changes — never inside a frame. Where a token is missing (jsdom
 * resolves no custom properties at all) the fallback above is used, which is the
 * light theme's own values and therefore not a second palette.
 */
export function readPalette(host: Element, ctx?: GridPaintContext): GridPalette {
  counters.styleReads += 1;
  const style = typeof getComputedStyle === "function" ? getComputedStyle(host) : undefined;
  const token = (name: string, fallback: string): string => {
    const value = style?.getPropertyValue(name).trim();
    return value === undefined || value === "" ? fallback : value;
  };

  const mono = token("--font-mono", '"IBM Plex Mono", ui-monospace, monospace');
  const sans = token("--font-sans", '"IBM Plex Sans", system-ui, sans-serif');
  const size = token("--fs-small", "12px");
  const cellFont = `${size} ${mono}`;

  const palette: GridPalette = {
    canvas: token("--canvas", FALLBACK.canvas),
    surface: token("--surface", FALLBACK.surface),
    border: token("--border", FALLBACK.border),
    borderStrong: token("--border-strong", FALLBACK.borderStrong),
    headerText: token("--text-label", FALLBACK.headerText),
    gutterText: token("--text-meta", FALLBACK.gutterText),
    // 06 §9.7's four inks. See the doc comment above for why two are aliases.
    inkText: token("--state-failed", FALLBACK.inkText),
    inkLabelled: token("--accent", FALLBACK.inkLabelled),
    inkNumeric: token("--text-body", FALLBACK.inkNumeric),
    inkMissing: token("--text-meta", FALLBACK.inkMissing),
    inkPending: token("--text-disabled", FALLBACK.inkPending),
    selectionFill: token("--accent-subtle", FALLBACK.selectionFill),
    selectionEdge: token("--accent", FALLBACK.selectionEdge),
    cellFont,
    headerFont: `${token("--fw-medium", "500")} ${size} ${sans}`,
    chPx: FALLBACK.chPx,
  };

  if (ctx !== undefined) {
    ctx.font = cellFont;
    counters.textMeasures += 1;
    const measured = ctx.measureText("0").width;
    // jsdom's stub context returns 0 for everything; a zero advance would make
    // every cell "too wide" and truncate the whole grid to one ellipsis.
    if (measured > 0) palette.chPx = measured;
  }
  return palette;
}

const INK_FOR_CODE: readonly (keyof GridPalette)[] = [
  "inkText",
  "inkLabelled",
  "inkNumeric",
  "inkMissing",
  "inkPending",
];

// ---------------------------------------------------------------------------
// The surface interface
// ---------------------------------------------------------------------------

export interface GridFrame {
  engine: GridEngine;
  cells: CellWindow;
  selection: SelectionModel;
  palette: GridPalette;
  /** 1-based observation numbers in the gutter, as Stata numbers them. */
  showGutter?: boolean;
}

export interface GridSurface {
  readonly kind: "canvas" | "dom";
  readonly element: HTMLElement;
  resize(width: number, height: number, dpr: number): void;
  paint(frame: GridFrame): void;
  dispose(): void;
}

// ---------------------------------------------------------------------------
// Shared cell logic
// ---------------------------------------------------------------------------

/**
 * The characters of `text` that fit in `width`, with an ellipsis when they do
 * not all fit.
 *
 * Returns `text` itself in the common case, so the common case allocates
 * nothing. `counters.paintAllocations` counts the other one.
 */
export function fitText(text: string, width: number, chPx: number, padX: number): string {
  if (text.length === 0) return text;
  const room = Math.floor((width - padX * 2) / chPx);
  if (room >= text.length) return text;
  if (room <= 1) return "…";
  counters.paintAllocations += 1;
  return `${text.slice(0, room - 1)}…`;
}

/** Stata numbers observations from 1. The gutter shows the true index out of 10 M. */
export const obsLabel = (row: number): string => String(row + 1);

// ---------------------------------------------------------------------------
// The canvas surface
// ---------------------------------------------------------------------------

export class CanvasSurface implements GridSurface {
  readonly kind = "canvas";
  readonly element: HTMLCanvasElement;
  private readonly ctx: GridPaintContext;
  private dpr = 1;
  private w = 0;
  private h = 0;
  private font = "";

  constructor(canvas: HTMLCanvasElement, ctx: GridPaintContext) {
    this.element = canvas;
    this.ctx = ctx;
  }

  /** `undefined` when the platform will not give us a 2D context (Q8's trigger). */
  static create(document: Document): CanvasSurface | undefined {
    const canvas = document.createElement("canvas");
    canvas.className = "grid__canvas";
    const ctx = canvas.getContext("2d", { alpha: false });
    if (ctx === null) return undefined;
    return new CanvasSurface(canvas, ctx);
  }

  resize(width: number, height: number, dpr: number): void {
    this.w = width;
    this.h = height;
    this.dpr = dpr;
    this.element.width = Math.max(1, Math.round(width * dpr));
    this.element.height = Math.max(1, Math.round(height * dpr));
    this.element.style.width = `${width}px`;
    this.element.style.height = `${height}px`;
  }

  paint(frame: GridFrame): void {
    const { engine, cells, palette } = frame;
    const m = engine.metrics;
    const ctx = this.ctx;
    const w = cells.window;

    counters.framesPainted += 1;
    ctx.setTransform(this.dpr, 0, 0, this.dpr, 0, 0);
    ctx.fillStyle = palette.canvas;
    ctx.fillRect(0, 0, this.w, this.h);

    // -- selection, under the text so the ink stays at full contrast ---------
    const sel = frame.selection.normalized();
    if (sel !== undefined) {
      ctx.fillStyle = palette.selectionFill;
      for (let r = 0; r < w.rowCount; r++) {
        const row = w.row0 + r;
        if (row < sel.top || row > sel.bottom) continue;
        const y = m.headerHeight + r * m.rowHeight + w.yOffset;
        let x = m.gutterWidth + w.xOffset;
        for (let c = 0; c < w.colCount; c++) {
          const column = engine.columns[w.col0 + c];
          if (column === undefined) continue;
          const col = w.col0 + c;
          if (col >= sel.left && col <= sel.right) ctx.fillRect(x, y, column.width, m.rowHeight);
          x += column.width;
        }
      }
    }

    // -- cells ---------------------------------------------------------------
    if (this.font !== palette.cellFont) {
      ctx.font = palette.cellFont;
      this.font = palette.cellFont;
    }
    ctx.textBaseline = "middle";
    const baseline = m.rowHeight / 2;

    for (let r = 0; r < w.rowCount; r++) {
      const y = m.headerHeight + r * m.rowHeight + w.yOffset;
      if (y + m.rowHeight < m.headerHeight || y > this.h) continue;
      const base = r * cells.cols;
      let x = m.gutterWidth + w.xOffset;
      for (let c = 0; c < w.colCount; c++) {
        const column = engine.columns[w.col0 + c];
        if (column === undefined) continue;
        const code = cells.ink[base + c] ?? INK.pending;
        const pending = code === INK.pending;
        const text = pending ? PLACEHOLDER : (cells.text[base + c] ?? "");
        if (text.length > 0) {
          ctx.fillStyle = palette[INK_FOR_CODE[code] ?? "inkNumeric"] as string;
          drawCell(ctx, text, x, y + baseline, column, m.cellPadX, palette.chPx);
          if (pending) counters.placeholdersPainted += 1;
        }
        counters.cellsPainted += 1;
        x += column.width;
      }
    }

    // -- chrome: gutter, header, and every hairline in ONE path --------------
    ctx.fillStyle = palette.surface;
    ctx.fillRect(0, 0, this.w, m.headerHeight);
    if (frame.showGutter !== false) ctx.fillRect(0, m.headerHeight, m.gutterWidth, this.h);

    ctx.textAlign = "right";
    ctx.fillStyle = palette.gutterText;
    if (frame.showGutter !== false) {
      for (let r = 0; r < w.rowCount; r++) {
        const y = m.headerHeight + r * m.rowHeight + w.yOffset;
        if (y + m.rowHeight < m.headerHeight) continue;
        ctx.fillText(obsLabel(w.row0 + r), m.gutterWidth - m.cellPadX, y + baseline);
        counters.fillTextCalls += 1;
      }
    }

    ctx.font = palette.headerFont;
    this.font = palette.headerFont;
    ctx.fillStyle = palette.headerText;
    ctx.textAlign = "left";
    {
      let x = m.gutterWidth + w.xOffset;
      for (let c = 0; c < w.colCount; c++) {
        const column = engine.columns[w.col0 + c];
        if (column === undefined) continue;
        ctx.fillText(
          fitText(column.name, column.width, palette.chPx, m.cellPadX),
          x + m.cellPadX,
          m.headerHeight / 2,
        );
        counters.fillTextCalls += 1;
        x += column.width;
      }
    }

    ctx.beginPath();
    ctx.lineWidth = 1;
    ctx.strokeStyle = palette.border;
    for (let r = 0; r <= w.rowCount; r++) {
      const y = Math.round(m.headerHeight + r * m.rowHeight + w.yOffset) + 0.5;
      if (y < m.headerHeight) continue;
      ctx.moveTo(0, y);
      ctx.lineTo(this.w, y);
    }
    {
      let x = m.gutterWidth + w.xOffset;
      for (let c = 0; c < w.colCount; c++) {
        const column = engine.columns[w.col0 + c];
        if (column === undefined) continue;
        x += column.width;
        const px = Math.round(x) + 0.5;
        ctx.moveTo(px, 0);
        ctx.lineTo(px, this.h);
      }
      const gx = Math.round(m.gutterWidth) + 0.5;
      ctx.moveTo(gx, 0);
      ctx.lineTo(gx, this.h);
    }
    ctx.stroke();

    ctx.beginPath();
    ctx.strokeStyle = palette.borderStrong;
    const hy = Math.round(m.headerHeight) + 0.5;
    ctx.moveTo(0, hy);
    ctx.lineTo(this.w, hy);
    ctx.stroke();

    // -- the focused cell's edge, last so nothing paints over it -------------
    if (sel !== undefined) {
      const rect = engine.cellRect(frame.selection.head.row, frame.selection.head.col);
      if (rect.y >= m.headerHeight - m.rowHeight && rect.y <= this.h) {
        ctx.beginPath();
        ctx.lineWidth = 2;
        ctx.strokeStyle = palette.selectionEdge;
        ctx.strokeRect(rect.x + 1, rect.y + 1, rect.w - 2, rect.h - 2);
        ctx.stroke();
      }
    }
  }

  dispose(): void {
    this.element.remove();
  }
}

function drawCell(
  ctx: GridPaintContext,
  text: string,
  x: number,
  baselineY: number,
  column: GridColumn,
  padX: number,
  chPx: number,
): void {
  const fitted = fitText(text, column.width, chPx, padX);
  if (column.align === "right") {
    ctx.textAlign = "right";
    ctx.fillText(fitted, x + column.width - padX, baselineY);
  } else {
    ctx.textAlign = "left";
    ctx.fillText(fitted, x + padX, baselineY);
  }
  counters.fillTextCalls += 1;
}

// ---------------------------------------------------------------------------
// The DOM surface — the Q8 fallback
// ---------------------------------------------------------------------------

/**
 * DOM virtualisation, for a platform whose canvas cannot keep up.
 *
 * Same window, same materialised cells, same inks; a pooled `<div>` per row and
 * a pooled `<span>` per cell, positioned with one `transform` per row. Nodes are
 * created once and then only ever have `textContent` and `className` written, so
 * `paintAllocations` settles at zero here too — what it does NOT avoid is style
 * and layout, which is the 2–4 ms per frame 06 §15.3 measured and the reason
 * this is the fallback rather than the design.
 *
 * The 1 M-row soft cap is not enforced here but in `GridEngine.setSoftCap`, so
 * that a capped grid cannot be scrolled to a row it would not draw.
 */
export class DomSurface implements GridSurface {
  readonly kind = "dom";
  readonly element: HTMLElement;
  private readonly body: HTMLElement;
  private readonly header: HTMLElement;
  private readonly gutter: HTMLElement;
  private readonly rowPool: HTMLElement[] = [];
  private readonly cellPool: HTMLElement[][] = [];
  private readonly headerPool: HTMLElement[] = [];
  private readonly gutterPool: HTMLElement[] = [];
  private readonly doc: Document;

  constructor(doc: Document) {
    this.doc = doc;
    this.element = doc.createElement("div");
    this.element.className = "grid__dom";
    this.header = doc.createElement("div");
    this.header.className = "grid__dom-header";
    this.gutter = doc.createElement("div");
    this.gutter.className = "grid__dom-gutter";
    this.body = doc.createElement("div");
    this.body.className = "grid__dom-body";
    this.element.append(this.header, this.gutter, this.body);
  }

  resize(width: number, height: number): void {
    this.element.style.width = `${width}px`;
    this.element.style.height = `${height}px`;
  }

  paint(frame: GridFrame): void {
    const { engine, cells } = frame;
    const m = engine.metrics;
    const w = cells.window;
    counters.framesPainted += 1;

    const sel = frame.selection.normalized();

    for (let r = 0; r < w.rowCount; r++) {
      const row = this.rowAt(r, w.colCount);
      row.style.transform = `translateY(${m.headerHeight + r * m.rowHeight + w.yOffset}px)`;
      row.style.left = `${m.gutterWidth + w.xOffset}px`;
      const base = r * cells.cols;
      const cellRow = this.cellPool[r] ?? [];
      for (let c = 0; c < w.colCount; c++) {
        const column = engine.columns[w.col0 + c];
        const cell = cellRow[c];
        if (column === undefined || cell === undefined) continue;
        const code = cells.ink[base + c] ?? INK.pending;
        const pending = code === INK.pending;
        const text = pending ? PLACEHOLDER : (cells.text[base + c] ?? "");
        if (cell.textContent !== text) cell.textContent = text;
        const absRow = w.row0 + r;
        const absCol = w.col0 + c;
        const selected =
          sel !== undefined &&
          absRow >= sel.top &&
          absRow <= sel.bottom &&
          absCol >= sel.left &&
          absCol <= sel.right;
        const next = `grid__cell grid__cell--${INK_NAME[code] ?? "numeric"} grid__cell--${column.align}${selected ? " is-selected" : ""}`;
        if (cell.className !== next) cell.className = next;
        cell.style.width = `${column.width}px`;
        if (pending) counters.placeholdersPainted += 1;
        counters.cellsPainted += 1;
      }
      for (let c = w.colCount; c < cellRow.length; c++) {
        const spare = cellRow[c];
        if (spare !== undefined) spare.textContent = "";
      }
    }
    for (let r = w.rowCount; r < this.rowPool.length; r++) {
      const spare = this.rowPool[r];
      if (spare !== undefined) spare.style.display = "none";
    }

    // Header and gutter, same pooling.
    for (let c = 0; c < w.colCount; c++) {
      const column = engine.columns[w.col0 + c];
      const cell = this.headerCell(c);
      if (column === undefined) continue;
      if (cell.textContent !== column.name) cell.textContent = column.name;
      cell.style.width = `${column.width}px`;
    }
    for (let c = w.colCount; c < this.headerPool.length; c++) {
      const spare = this.headerPool[c];
      if (spare !== undefined) spare.textContent = "";
    }
    this.header.style.transform = `translateX(${m.gutterWidth + w.xOffset}px)`;

    if (frame.showGutter !== false) {
      for (let r = 0; r < w.rowCount; r++) {
        const cell = this.gutterCell(r);
        const label = obsLabel(w.row0 + r);
        if (cell.textContent !== label) cell.textContent = label;
        cell.style.transform = `translateY(${m.headerHeight + r * m.rowHeight + w.yOffset}px)`;
        cell.style.display = "";
      }
      for (let r = w.rowCount; r < this.gutterPool.length; r++) {
        const spare = this.gutterPool[r];
        if (spare !== undefined) spare.style.display = "none";
      }
    }
  }

  private rowAt(index: number, cols: number): HTMLElement {
    let row = this.rowPool[index];
    if (row === undefined) {
      counters.paintAllocations += 1;
      row = this.doc.createElement("div");
      row.className = "grid__row";
      this.rowPool[index] = row;
      this.cellPool[index] = [];
      this.body.appendChild(row);
    }
    row.style.display = "";
    const cellRow = this.cellPool[index] ?? [];
    while (cellRow.length < cols) {
      counters.paintAllocations += 1;
      const cell = this.doc.createElement("span");
      cell.className = "grid__cell";
      cellRow.push(cell);
      row.appendChild(cell);
    }
    return row;
  }

  private headerCell(index: number): HTMLElement {
    let cell = this.headerPool[index];
    if (cell === undefined) {
      counters.paintAllocations += 1;
      cell = this.doc.createElement("span");
      cell.className = "grid__headcell";
      this.headerPool[index] = cell;
      this.header.appendChild(cell);
    }
    return cell;
  }

  private gutterCell(index: number): HTMLElement {
    let cell = this.gutterPool[index];
    if (cell === undefined) {
      counters.paintAllocations += 1;
      cell = this.doc.createElement("span");
      cell.className = "grid__obs";
      this.gutterPool[index] = cell;
      this.gutter.appendChild(cell);
    }
    return cell;
  }

  dispose(): void {
    this.element.remove();
  }
}

const INK_NAME: readonly string[] = ["text", "labelled", "numeric", "missing", "pending"];

// ---------------------------------------------------------------------------
// Q8: choosing a surface
// ---------------------------------------------------------------------------

export type SurfacePreference = "auto" | "canvas" | "dom";

export interface CanvasProbe {
  /** `false` when there is no 2D context at all, which settles it immediately. */
  available: boolean;
  /** Cells per second the probe managed. RECORDED, never asserted (ADR-017). */
  cellsPerSecond: number;
  /** What the probe concluded, given `MIN_CELLS_PER_SECOND`. */
  usable: boolean;
}

/**
 * A 60×40 viewport at 60 fps is 144 000 cells per second. The probe asks for
 * four times that before it trusts canvas, because the probe runs on an idle
 * main thread and a real frame shares it with everything else.
 */
export const MIN_CELLS_PER_SECOND = 576_000;

/**
 * The Q8 spike, as a runtime probe rather than a claim.
 *
 * **What this is not.** It is not a benchmark assertion — ADR-017 forbids those
 * and is right to. It is a capability decision made once at startup, on the
 * machine the user actually has, and the number it produces is RECORDED for the
 * status bar and the log, never compared in a test.
 *
 * **What is still owed.** The failure mode Q8 names is WebKitGTK specifically,
 * and neither this machine nor CI runs WebKitGTK. So the honest position is:
 * the fallback exists, is complete, is reachable, and is selected by a
 * measurement taken on the user's own webview; the Linux number itself has to be
 * taken on Linux before the GA claim, which is what ARCHITECTURE's Q-table
 * already says ("Before the Linux GA claim").
 */
export function probeCanvas(doc: Document, cells = 2400): CanvasProbe {
  const canvas = doc.createElement("canvas");
  canvas.width = 640;
  canvas.height = 480;
  const ctx = canvas.getContext("2d", { alpha: false });
  if (ctx === null) return { available: false, cellsPerSecond: 0, usable: false };

  ctx.font = '12px "IBM Plex Mono", ui-monospace, monospace';
  ctx.textBaseline = "middle";
  const started = performance.now();
  for (let i = 0; i < cells; i++) {
    ctx.fillText("123,456", (i % 12) * 52, ((i / 12) | 0) % 480);
  }
  const elapsed = performance.now() - started;
  // A zero elapsed time means the platform's clock is too coarse to tell us
  // anything (or the context is a no-op stub). Treat "unknown" as "usable":
  // canvas is the design, and the fallback is for a platform that has DEMONSTRATED
  // it cannot keep up, not for one we could not measure.
  const rate = elapsed <= 0 ? Number.POSITIVE_INFINITY : (cells / elapsed) * 1000;
  return { available: true, cellsPerSecond: rate, usable: rate >= MIN_CELLS_PER_SECOND };
}

export interface SurfaceChoice {
  surface: GridSurface;
  probe: CanvasProbe;
  /** True when the 1 M-row soft cap applies (06 §15.3 / Q8). */
  softCapped: boolean;
}

/** Builds the surface this platform can actually sustain. */
export function createSurface(
  doc: Document,
  preference: SurfacePreference = "auto",
): SurfaceChoice {
  const probe =
    preference === "dom"
      ? { available: false, cellsPerSecond: 0, usable: false }
      : probeCanvas(doc);
  const wantCanvas = preference === "canvas" || (preference === "auto" && probe.usable);
  if (wantCanvas) {
    const surface = CanvasSurface.create(doc);
    if (surface !== undefined) return { surface, probe, softCapped: false };
  }
  return { surface: new DomSurface(doc), probe, softCapped: true };
}
