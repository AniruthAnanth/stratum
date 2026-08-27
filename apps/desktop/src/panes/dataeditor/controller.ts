/**
 * The live grid: engine + pages + surface + selection + mirror + editor.
 *
 * One `requestAnimationFrame` loop that "draws from whatever chunks are
 * resident, painting `⋯` placeholders in the `--text-meta` ink for rows in
 * flight" (06 §15.3). Nothing on the scroll path awaits anything, which is the
 * literal reading of **"Scrolling never waits on data."**
 *
 * The order of operations in a frame is the whole design:
 *
 *   1. `engine.materialize(pages)` — reads the visible window out of whatever is
 *      resident. Missing cells come back `undefined`; they are not an error.
 *   2. `surface.paint(...)` — draws, placeholders included.
 *   3. `pages.ensure(...)` — asks for what the NEXT frames will need.
 *
 * Fetching last is deliberate. A frame that requested first would still not have
 * the bytes (the request is a promise), and it would put the request's cost
 * before the paint's on the same main thread.
 *
 * **Two page sources in Edit mode.** `RenderMode::Display` is what the grid
 * paints — formatted, value-labelled, missing values as Stata prints them,
 * because "Formatting happens in the CORE, so `list`, the Data Editor and the
 * inline cards cannot disagree" (CONTRACTS §8.1). But `replace price = 4,099` is
 * a syntax error, so an edit needs the RAW value, which is `RenderMode::Edit`.
 * Browse mode therefore costs one round-trip per page and Edit mode costs two,
 * and the second one exists only while the user is in a mode whose whole purpose
 * is changing values.
 */

import { GridMirror } from "../../grid/a11y";
import {
  DEFAULT_METRICS,
  DOM_SOFT_CAP,
  type GridColumn,
  GridEngine,
  type GridMetrics,
} from "../../grid/engine";
import { PageSource } from "../../grid/fetch";
import { CellEditor } from "../../grid/ime";
import {
  type GridPalette,
  type GridSurface,
  type SurfacePreference,
  createSurface,
  readPalette,
} from "../../grid/paint";
import { SyntheticScrollbar, wheelToScroll } from "../../grid/scrollbar";
import { SelectionModel, copySelection, navigate } from "../../grid/select";
import type { DatasetStateId, SessionId } from "../../ipc/hand";
import { replaceCommand, seedValue } from "./edit";

export type GridMode = "browse" | "edit";

export interface GridStatus {
  /** Stata's five fields, in Stata's order (06 §9.7). */
  vars: number;
  order: string;
  obs: number;
  length: number;
  filter: string;
  mode: GridMode;
  /** Which surface Q8's probe selected, and what it measured. Recorded only. */
  surface: "canvas" | "dom";
  cellsPerSecond: number;
  /** True when the DOM fallback's 1 M-row soft cap is hiding observations. */
  capped: boolean;
  /** The cell the keyboard is on, for the Properties readout. */
  cursor?: { row: number; column: GridColumn };
}

export interface DataGridOptions {
  session: SessionId;
  frame: string;
  state: DatasetStateId;
  doc?: Document;
  metrics?: GridMetrics;
  surface?: SurfacePreference;
  /** Test seam; production leaves it undefined and the bridge is used. */
  fetchAsset?: (url: string, init?: { signal?: AbortSignal }) => Promise<Response>;
  /** Test seam for the render loop. Defaults to `requestAnimationFrame`. */
  schedule?: (callback: () => void) => void;
  /** Receives `replace <var> = <val> in <n>`. The pane submits it. */
  onEdit?: (command: string) => void;
  onStatus?: (status: GridStatus) => void;
  /** Transient user-facing text: a rejected edit, a truncated copy, the soft cap. */
  onNotice?: (text: string | undefined) => void;
  onStateAdvanced?: (state: DatasetStateId) => void;
  /** A click on a column header. The pane turns it into `data_order_set`. */
  onHeaderActivate?: (columnIndex: number) => void;
}

export class DataGridController {
  readonly element: HTMLElement;
  readonly engine: GridEngine;
  readonly selection = new SelectionModel();
  readonly mirror: GridMirror;
  readonly editor: CellEditor;

  private readonly doc: Document;
  private readonly options: DataGridOptions;
  private readonly surface: GridSurface;
  private readonly probeRate: number;
  private readonly vbar: SyntheticScrollbar;
  private readonly hbar: SyntheticScrollbar;
  private readonly viewport: HTMLElement;
  private display: PageSource;
  private raw: PageSource | undefined;
  private palette: GridPalette;
  private mode: GridMode = "browse";
  private orderId: number | undefined;
  private orderText = "Dataset";
  private filterText = "Off";
  private direction = 1;
  private pending = false;
  private width = 0;
  private height = 0;
  private readonly teardown: (() => void)[] = [];

  constructor(options: DataGridOptions) {
    this.options = options;
    this.doc = options.doc ?? document;
    this.engine = new GridEngine(options.metrics ?? DEFAULT_METRICS);

    this.element = this.doc.createElement("div");
    this.element.className = "grid";
    this.viewport = this.doc.createElement("div");
    this.viewport.className = "grid__viewport";
    this.element.appendChild(this.viewport);

    const choice = createSurface(this.doc, options.surface);
    this.surface = choice.surface;
    this.probeRate = choice.probe.cellsPerSecond;
    // Q8's documented fallback, enforced where it cannot be scrolled past.
    if (choice.softCapped) this.engine.setSoftCap(DOM_SOFT_CAP);
    this.surface.element.setAttribute("aria-hidden", "true");
    this.viewport.appendChild(this.surface.element);

    this.mirror = new GridMirror({ doc: this.doc, idPrefix: `grid-${options.frame}` });
    this.viewport.appendChild(this.mirror.element);

    this.editor = new CellEditor({
      doc: this.doc,
      onCommit: (value, move) => this.commitEdit(value, move),
      onCancel: () => {
        this.mirror.focus();
        this.schedule();
      },
    });
    this.viewport.appendChild(this.editor.element);

    this.vbar = new SyntheticScrollbar({
      orientation: "vertical",
      doc: this.doc,
      label: "Observations",
      onScroll: (row) => {
        this.direction = row >= this.engine.scrollRow ? 1 : -1;
        this.engine.scrollToRow(row);
        this.schedule();
      },
    });
    this.hbar = new SyntheticScrollbar({
      orientation: "horizontal",
      doc: this.doc,
      label: "Variables",
      onScroll: (x) => {
        this.engine.scrollToX(x);
        this.schedule();
      },
    });
    this.element.append(this.vbar.element, this.hbar.element);

    this.palette = readPalette(this.element);
    this.display = this.makeSource("display");
    this.installEvents();
  }

  // -- lifecycle ------------------------------------------------------------

  private makeSource(render: "display" | "edit"): PageSource {
    const source = new PageSource({
      session: this.options.session,
      frame: this.options.frame,
      render,
      ...(this.options.fetchAsset === undefined ? {} : { fetchAsset: this.options.fetchAsset }),
      onPage: () => {
        this.mirror.revision += 1;
        this.schedule();
      },
      onStateAdvanced: (state) => this.options.onStateAdvanced?.(state),
    });
    source.retarget({ state: this.options.state, order: this.orderId });
    source.setColumns(this.engine.columns);
    return source;
  }

  setColumns(columns: readonly GridColumn[]): void {
    this.engine.setColumns(columns);
    this.display.setColumns(columns);
    this.raw?.setColumns(columns);
    this.schedule();
  }

  setRowCount(rows: number): void {
    this.engine.setRowCount(rows);
    this.schedule();
  }

  /** The frame advanced: nothing resident describes it any more (CONTRACTS §8.1). */
  invalidate(state: DatasetStateId): void {
    this.display.invalidate(state);
    this.raw?.invalidate(state);
    this.mirror.revision += 1;
    this.schedule();
  }

  /** After `data_order_set`. `undefined` restores dataset order. */
  setOrder(
    order: number | undefined,
    nRows: number,
    label: string,
    filter: string,
    sort?: { columnIndex: number; dir: "asc" | "desc" },
  ): void {
    this.mirror.setSort(sort);
    this.orderId = order;
    this.orderText = label;
    this.filterText = filter;
    this.display.retarget({ order });
    this.raw?.retarget({ order });
    this.engine.setRowCount(nRows);
    this.engine.scrollToRow(0);
    if (order !== undefined && this.mode === "edit") {
      // See `order.ts`: `replace … in n` counts observations and a view row is
      // not one. Dropping to Browse is the honest response to a contract gap.
      this.setMode("browse");
      this.options.onNotice?.(
        "Sorted and filtered views are read-only: `replace … in n` counts observations, and the wire carries no way to map a view row back to one.",
      );
    }
    this.schedule();
  }

  setMode(mode: GridMode): void {
    if (mode === "edit" && this.orderId !== undefined) return;
    if (this.mode === mode) return;
    this.mode = mode;
    this.mirror.setReadonly(mode === "browse");
    if (mode === "edit") {
      this.raw ??= this.makeSource("edit");
    } else {
      this.editor.close();
      this.raw?.dispose();
      this.raw = undefined;
    }
    this.schedule();
  }

  get currentMode(): GridMode {
    return this.mode;
  }

  /** Re-reads the tokens. Called on a theme change, never inside a frame. */
  refreshPalette(): void {
    this.palette = readPalette(this.element);
    this.schedule();
  }

  layout(width: number, height: number): void {
    this.width = width;
    this.height = height;
    const dpr = this.doc.defaultView?.devicePixelRatio ?? 1;
    this.engine.setViewport(width, height);
    this.surface.resize(width, height, dpr);
    this.paint();
  }

  dispose(): void {
    for (const off of this.teardown) off();
    this.display.dispose();
    this.raw?.dispose();
    this.surface.dispose();
    this.mirror.dispose();
    this.editor.dispose();
    this.vbar.dispose();
    this.hbar.dispose();
    this.element.remove();
  }

  // -- the frame ------------------------------------------------------------

  /** Coalesces every reason to repaint into at most one frame. */
  schedule(): void {
    if (this.pending) return;
    this.pending = true;
    const run = (): void => {
      this.pending = false;
      this.paint();
    };
    if (this.options.schedule !== undefined) this.options.schedule(run);
    else if (typeof requestAnimationFrame === "function") requestAnimationFrame(run);
    else run();
  }

  /** Draws one frame. Called by the loop; safe to call directly in a test. */
  paint(): void {
    // Always the DISPLAY source: Edit mode changes what a commit reads, not what
    // the grid shows. Stata's own Data Editor shows formatted, labelled values in
    // both modes.
    const cells = this.engine.materialize(this.display);
    const w = cells.window;

    this.surface.paint({
      engine: this.engine,
      cells,
      selection: this.selection,
      palette: this.palette,
    });
    this.mirror.update(this.engine, cells, this.selection);

    // Position, viewport, total, track. No maximum is passed: the bar derives it
    // with W16's `maxPosition`, which is the same function `engine.maxScrollRow`
    // is written from, so the thumb and the clamp cannot drift apart.
    this.vbar.update(
      this.engine.scrollRow,
      this.engine.visibleRowCount,
      this.engine.reachableRows,
      Math.max(0, this.height - this.engine.metrics.headerHeight),
    );
    this.hbar.update(
      this.engine.scrollX,
      this.engine.bodyWidth,
      this.engine.totalColumnWidth,
      Math.max(0, this.width - this.engine.metrics.gutterWidth),
    );

    // Requested AFTER the paint: see the header comment.
    this.display.ensure(
      w.row0,
      w.rowCount,
      w.col0,
      w.colCount,
      this.direction,
      this.engine.reachableRows,
    );
    this.raw?.ensure(
      w.row0,
      w.rowCount,
      w.col0,
      w.colCount,
      this.direction,
      this.engine.reachableRows,
    );

    if (this.editor.isOpen) {
      const at = this.editor.editing;
      this.editor.reposition(at === undefined ? undefined : this.visibleRect(at.row, at.col));
    }
    this.options.onStatus?.(this.status());
  }

  private visibleRect(
    row: number,
    col: number,
  ): { x: number; y: number; w: number; h: number } | undefined {
    const rect = this.engine.cellRect(row, col);
    if (rect.y < this.engine.metrics.headerHeight || rect.y > this.height) return undefined;
    return rect;
  }

  status(): GridStatus {
    const columns = this.engine.columns;
    const cursorCol = this.selection.isEmpty ? undefined : columns[this.selection.head.col];
    const lengthOf = cursorCol ?? columns[0];
    return {
      vars: columns.length,
      order: this.orderText,
      obs: this.engine.rowCount,
      length: lengthOf?.storageWidth ?? 0,
      filter: this.filterText,
      mode: this.mode,
      surface: this.surface.kind,
      cellsPerSecond: this.probeRate,
      capped: this.engine.capped,
      ...(cursorCol === undefined
        ? {}
        : { cursor: { row: this.selection.head.row, column: cursorCol } }),
    };
  }

  // -- input ----------------------------------------------------------------

  private installEvents(): void {
    const on = <K extends keyof HTMLElementEventMap>(
      target: HTMLElement,
      type: K,
      handler: (event: HTMLElementEventMap[K]) => void,
      options?: AddEventListenerOptions,
    ): void => {
      target.addEventListener(type, handler as EventListener, options);
      this.teardown.push(() => target.removeEventListener(type, handler as EventListener, options));
    };

    on(this.viewport, "wheel", (event) => {
      const delta = wheelToScroll(
        event,
        this.engine.metrics.rowHeight,
        this.engine.visibleRowCount,
      );
      if (delta.rows !== 0) this.direction = delta.rows > 0 ? 1 : -1;
      const moved = this.engine.scrollByRows(delta.rows) || this.engine.scrollByX(delta.x);
      if (moved) {
        event.preventDefault();
        this.schedule();
      }
    });

    on(this.viewport, "pointerdown", (event) => {
      if (event.target === this.editor.element) return;
      const m = this.engine.metrics;
      if (event.offsetY < m.headerHeight) {
        // The header is the sort affordance. It issues `data_order_set`, never a
        // client-side comparison — `06` §15.3: sorting happens in Rust.
        if (event.offsetX < m.gutterWidth) return;
        const column = this.engine.columnAt(event.offsetX - m.gutterWidth + this.engine.scrollX);
        if (column < this.engine.columns.length) this.options.onHeaderActivate?.(column);
        return;
      }
      const hit = this.engine.hitTest(event.offsetX, event.offsetY);
      if (hit === undefined) return;
      this.mirror.focus();
      if (event.shiftKey) this.selection.extendTo(hit.row, hit.col);
      else this.selection.moveTo(hit.row, hit.col);
      this.schedule();
    });

    on(this.viewport, "pointermove", (event) => {
      if (event.buttons !== 1 || this.editor.isOpen) return;
      const hit = this.engine.hitTest(event.offsetX, event.offsetY);
      if (hit === undefined) return;
      this.selection.extendTo(hit.row, hit.col);
      this.schedule();
    });

    on(this.viewport, "dblclick", (event) => {
      const hit = this.engine.hitTest(event.offsetX, event.offsetY);
      if (hit !== undefined) this.beginEdit(hit.row, hit.col);
    });

    on(this.mirror.element, "keydown", (event) => {
      if (this.editor.isOpen) return;
      const mod = event.ctrlKey || event.metaKey;
      if (mod && (event.key === "c" || event.key === "C")) {
        event.preventDefault();
        this.copy();
        return;
      }
      if (!mod && (event.key === "Enter" || event.key === "F2") && !this.selection.isEmpty) {
        event.preventDefault();
        this.beginEdit(this.selection.head.row, this.selection.head.col);
        return;
      }
      if (navigate(this.engine, this.selection, event)) {
        event.preventDefault();
        this.schedule();
      }
    });
  }

  // -- editing --------------------------------------------------------------

  beginEdit(row: number, col: number): void {
    if (this.mode !== "edit") return;
    const column = this.engine.columns[col];
    if (column === undefined) return;
    this.selection.moveTo(row, col);
    this.engine.revealCell(row, col);
    const rect = this.engine.cellRect(row, col);
    // The raw page is what `replace` has to be built from. In Edit mode it is
    // already resident for the visible window; if a fast scroll got ahead of it,
    // the display text is the honest seed and the user sees what they clicked.
    const cell = this.raw?.raw(row, column);
    const seed = seedValue(
      cell?.kind === "blob" ? undefined : cell,
      this.display.cell(row, column) ?? "",
    );
    this.editor.openAt(rect, seed, column, { row, col });
    this.schedule();
  }

  private commitEdit(value: string, move: "none" | "down" | "up" | "right" | "left"): void {
    const at = this.editor.editing ?? this.selection.head;
    const column = this.engine.columns[at.col];
    if (column === undefined) return;

    const outcome = replaceCommand(column, at.row + 1, value);
    if (!outcome.ok) {
      this.options.onNotice?.(outcome.reason);
      this.mirror.focus();
      this.schedule();
      return;
    }
    this.options.onNotice?.(undefined);
    this.options.onEdit?.(outcome.command);

    const rows = this.engine.reachableRows;
    const cols = this.engine.columns.length;
    switch (move) {
      case "down":
        this.selection.moveTo(Math.min(rows - 1, at.row + 1), at.col);
        break;
      case "up":
        this.selection.moveTo(Math.max(0, at.row - 1), at.col);
        break;
      case "right":
        this.selection.moveTo(at.row, Math.min(cols - 1, at.col + 1));
        break;
      case "left":
        this.selection.moveTo(at.row, Math.max(0, at.col - 1));
        break;
      default:
        this.selection.moveTo(at.row, at.col);
    }
    this.engine.revealCell(this.selection.head.row, this.selection.head.col);
    this.mirror.focus();
    this.schedule();
  }

  // -- copy -----------------------------------------------------------------

  copy(format: "tsv" | "csv" | "stata-list" = "tsv"): void {
    const result = copySelection(this.display, this.engine.columns, this.selection, format);
    if (result.text !== "") void this.doc.defaultView?.navigator?.clipboard?.writeText(result.text);
    if (!result.complete) {
      this.options.onNotice?.(
        `Copied ${result.rows.toLocaleString()} of ${result.requestedRows.toLocaleString()} observations — the rest are not resident, and CONTRACTS §11 declares no \`data_copy\` command to ask the engine for them.`,
      );
    }
  }
}
