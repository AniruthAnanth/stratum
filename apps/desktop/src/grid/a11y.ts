/**
 * The off-screen accessibility mirror.
 *
 * 06 §15.3 lists this as the second cost of drawing the grid on a canvas, and
 * names the payment exactly: "**No native accessibility tree** → we maintain an
 * off-screen DOM mirror of the visible window (`role="grid"`,
 * `aria-rowindex`/`aria-colindex` with the true indices) so screen readers get
 * correct row numbers out of 10 M."
 *
 * Two things make that sentence true rather than decorative.
 *
 * **The indices are absolute, not window-relative.** `aria-rowindex` on the
 * fortieth rendered row of a window starting at observation 8 399 960 is
 * 8 399 962, not 41. That is the whole point: a screen-reader user scrolling a
 * 10 M-row dataset is told where they are in the dataset, not where they are in
 * our buffer. `aria-rowcount` is the true total.
 *
 * **The mirror is the focusable element.** The canvas is `aria-hidden`, and
 * `role="grid"` lives here with `tabindex="0"` and `aria-activedescendant`
 * pointing at the focused cell. Keyboard events therefore arrive at the element
 * that assistive technology is actually reading, instead of at a canvas that
 * announces nothing.
 *
 * It is clipped rather than `display: none` or `visibility: hidden` on purpose:
 * both of those remove the subtree from the accessibility tree, which would make
 * this file a no-op that looked like a feature.
 *
 * Node churn is a counter, not a hope: `mirrorNodesCreated` settles once the
 * viewport has been seen at its size, and `mirrorCellsWritten` counts only cells
 * whose text actually changed.
 */

import type { CellWindow, GridEngine } from "./engine";
import { counters } from "./engine";
import { obsLabel } from "./paint";
import type { SelectionModel } from "./select";

export interface GridMirrorOptions {
  doc?: Document;
  /** Prefix for the generated cell ids `aria-activedescendant` points at. */
  idPrefix?: string;
  label?: string;
}

/** ARIA counts the header as row 1, so observation `r` (0-based) is `r + 2`. */
export const ariaRowIndex = (row: number): number => row + 2;

/** Column 1 is the observation-number gutter, so grid column `c` is `c + 2`. */
export const ariaColIndex = (col: number): number => col + 2;

export class GridMirror {
  readonly element: HTMLElement;
  private readonly doc: Document;
  private readonly idPrefix: string;
  private readonly headerRow: HTMLElement;
  private readonly headerCells: HTMLElement[] = [];
  private readonly rows: HTMLElement[] = [];
  private readonly rowHeaders: HTMLElement[] = [];
  private readonly cells: HTMLElement[][] = [];
  /** Bumped by the host when a page lands, so an unchanged window is skipped. */
  revision = 0;
  private sort: { columnIndex: number; dir: "asc" | "desc" } | undefined;
  private painted = { row0: -1, col0: -1, rowCount: -1, colCount: -1, revision: -1 };

  constructor(options: GridMirrorOptions = {}) {
    this.doc = options.doc ?? document;
    this.idPrefix = options.idPrefix ?? "grid";
    this.element = this.doc.createElement("div");
    this.element.className = "grid__mirror";
    this.element.setAttribute("role", "grid");
    this.element.setAttribute("aria-label", options.label ?? "Data editor");
    this.element.setAttribute("aria-readonly", "true");
    this.element.tabIndex = 0;

    this.headerRow = this.doc.createElement("div");
    this.headerRow.setAttribute("role", "row");
    this.headerRow.setAttribute("aria-rowindex", "1");
    this.element.appendChild(this.headerRow);

    const corner = this.doc.createElement("div");
    corner.setAttribute("role", "columnheader");
    corner.setAttribute("aria-colindex", "1");
    corner.textContent = "Observation";
    this.headerRow.appendChild(corner);
  }

  /**
   * The live sort, for `aria-sort`.
   *
   * A screen-reader user has no header arrow to look at, and "sorted by make,
   * ascending" is the difference between observation 1 meaning the first car in
   * the file and the first car alphabetically.
   */
  setSort(sort: { columnIndex: number; dir: "asc" | "desc" } | undefined): void {
    this.sort = sort;
    this.painted.revision = -1;
  }

  /** `false` once the grid is editable, which flips `aria-readonly`. */
  setReadonly(readonly: boolean): void {
    this.element.setAttribute("aria-readonly", String(readonly));
  }

  focus(): void {
    this.element.focus();
  }

  /**
   * Rebuilds the mirror for the current window.
   *
   * Skipped entirely when neither the window nor the data has moved: a repaint
   * caused by a selection change alone must not touch 520 DOM nodes.
   */
  update(engine: GridEngine, cells: CellWindow, selection: SelectionModel): void {
    const w = cells.window;
    const unchanged =
      this.painted.row0 === w.row0 &&
      this.painted.col0 === w.col0 &&
      this.painted.rowCount === w.rowCount &&
      this.painted.colCount === w.colCount &&
      this.painted.revision === this.revision;

    counters.mirrorUpdates += 1;
    this.element.setAttribute("aria-rowcount", String(engine.rowCount + 1));
    this.element.setAttribute("aria-colcount", String(engine.columns.length + 1));

    if (!unchanged) {
      this.syncHeader(engine, w.col0, w.colCount);
      this.syncBody(engine, cells);
      this.painted = {
        row0: w.row0,
        col0: w.col0,
        rowCount: w.rowCount,
        colCount: w.colCount,
        revision: this.revision,
      };
    }
    this.syncSelection(selection, w.row0, w.rowCount, w.col0, w.colCount);
  }

  private syncHeader(engine: GridEngine, col0: number, colCount: number): void {
    for (let c = 0; c < colCount; c++) {
      const column = engine.columns[col0 + c];
      if (column === undefined) continue;
      const cell = this.headerCell(c);
      cell.setAttribute("aria-colindex", String(ariaColIndex(col0 + c)));
      const sorted = this.sort !== undefined && this.sort.columnIndex === col0 + c;
      cell.setAttribute(
        "aria-sort",
        sorted ? (this.sort?.dir === "desc" ? "descending" : "ascending") : "none",
      );
      // The header carries the storage type and the display format, because a
      // screen-reader user cannot see the ink that tells a sighted user a column
      // is a string, and `str18 %-18s` is the same fact stated in words.
      const text = `${column.name}, ${column.storage}, ${column.format}${
        column.valueLabel === undefined ? "" : `, value label ${column.valueLabel}`
      }${column.label === undefined ? "" : `, ${column.label}`}`;
      if (cell.textContent !== text) {
        cell.textContent = text;
        counters.mirrorCellsWritten += 1;
      }
    }
    for (let c = colCount; c < this.headerCells.length; c++) {
      const spare = this.headerCells[c];
      if (spare !== undefined) spare.remove();
    }
    this.headerCells.length = Math.min(this.headerCells.length, colCount);
  }

  private syncBody(engine: GridEngine, cells: CellWindow): void {
    const w = cells.window;
    for (let r = 0; r < w.rowCount; r++) {
      const absRow = w.row0 + r;
      const row = this.rowAt(r);
      this.ensureCells(r, w.colCount);
      row.setAttribute("aria-rowindex", String(ariaRowIndex(absRow)));
      row.hidden = false;

      const header = this.rowHeaders[r];
      if (header !== undefined) {
        const label = obsLabel(absRow);
        if (header.textContent !== label) {
          header.textContent = label;
          counters.mirrorCellsWritten += 1;
        }
      }

      const base = r * cells.cols;
      const rowCells = this.cells[r] ?? [];
      for (let c = 0; c < w.colCount; c++) {
        const column = engine.columns[w.col0 + c];
        const cell = rowCells[c];
        if (column === undefined || cell === undefined) continue;
        cell.id = `${this.idPrefix}-c-${absRow}-${w.col0 + c}`;
        cell.setAttribute("aria-colindex", String(ariaColIndex(w.col0 + c)));
        // A cell whose page is in flight says so. "Loading" is a fact; silence
        // would be announced as an empty cell, which is a different fact.
        const text = cells.ink[base + c] === 4 ? "loading" : (cells.text[base + c] ?? "");
        if (cell.textContent !== text) {
          cell.textContent = text;
          counters.mirrorCellsWritten += 1;
        }
      }
      for (let c = w.colCount; c < rowCells.length; c++) {
        const spare = rowCells[c];
        if (spare !== undefined) spare.remove();
      }
      rowCells.length = Math.min(rowCells.length, w.colCount);
    }
    for (let r = w.rowCount; r < this.rows.length; r++) {
      const spare = this.rows[r];
      if (spare !== undefined) spare.hidden = true;
    }
  }

  private syncSelection(
    selection: SelectionModel,
    row0: number,
    rowCount: number,
    col0: number,
    colCount: number,
  ): void {
    const rect = selection.normalized();
    for (let r = 0; r < rowCount; r++) {
      const rowCells = this.cells[r] ?? [];
      for (let c = 0; c < colCount; c++) {
        const cell = rowCells[c];
        if (cell === undefined) continue;
        const selected =
          rect !== undefined &&
          row0 + r >= rect.top &&
          row0 + r <= rect.bottom &&
          col0 + c >= rect.left &&
          col0 + c <= rect.right;
        const value = selected ? "true" : "false";
        if (cell.getAttribute("aria-selected") !== value) cell.setAttribute("aria-selected", value);
      }
    }
    if (selection.isEmpty) {
      this.element.removeAttribute("aria-activedescendant");
      return;
    }
    this.element.setAttribute(
      "aria-activedescendant",
      `${this.idPrefix}-c-${selection.head.row}-${selection.head.col}`,
    );
  }

  private headerCell(index: number): HTMLElement {
    let cell = this.headerCells[index];
    if (cell === undefined) {
      counters.mirrorNodesCreated += 1;
      cell = this.doc.createElement("div");
      cell.setAttribute("role", "columnheader");
      this.headerCells[index] = cell;
      this.headerRow.appendChild(cell);
    }
    return cell;
  }

  private rowAt(index: number): HTMLElement {
    let row = this.rows[index];
    if (row === undefined) {
      counters.mirrorNodesCreated += 1;
      row = this.doc.createElement("div");
      row.setAttribute("role", "row");
      this.rows[index] = row;
      const header = this.doc.createElement("div");
      header.setAttribute("role", "rowheader");
      header.setAttribute("aria-colindex", "1");
      this.rowHeaders[index] = header;
      row.appendChild(header);
      this.cells[index] = [];
      this.element.appendChild(row);
    }
    return row;
  }

  /** Grows the per-row cell pool. `rowAt` has already created the row's array. */
  private ensureCells(rowIndex: number, count: number): void {
    const row = this.rowAt(rowIndex);
    const rowCells = this.cells[rowIndex];
    if (rowCells === undefined) return;
    while (rowCells.length < count) {
      counters.mirrorNodesCreated += 1;
      const cell = this.doc.createElement("div");
      cell.setAttribute("role", "gridcell");
      rowCells.push(cell);
      row.appendChild(cell);
    }
  }

  dispose(): void {
    this.element.remove();
  }
}
