/**
 * Cell and range selection, keyboard navigation, and copy.
 *
 * A canvas has no native text selection — 06 §15.3 lists that as the first cost
 * of the canvas ruling and names the payment: "cell/range selection model,
 * `Mod+C` copies through Rust in TSV/CSV/Stata-list format".
 *
 * The model is two cells, an anchor and a head, exactly as a spreadsheet and as
 * CodeMirror do it. Everything else — the rectangle, whether a cell is in it,
 * what to copy — is derived. That matters at 10 M rows: `Mod+A` is four number
 * assignments and no iteration, and a rectangle that covers ten million
 * observations costs the same as one that covers three.
 *
 * **Copy is honest about its limit.** 06 §15.3 says copy goes through Rust so
 * that text outside the rendered window is included, and CONTRACTS §11 declares
 * `log_copy` for the log — but no `data_copy` for a frame. There is therefore no
 * sanctioned way to copy a selection larger than what is resident, and
 * inventing a command would violate R1. `copySelection` copies what it can and
 * reports `complete: false` for the rest; the pane says so out loud. Escalated
 * in this unit's return.
 */

import type { CellSource, GridColumn, GridEngine } from "./engine";

export interface CellRef {
  row: number;
  col: number;
}

export interface SelectionRect {
  top: number;
  bottom: number;
  left: number;
  right: number;
}

export class SelectionModel {
  anchor: CellRef = { row: 0, col: 0 };
  head: CellRef = { row: 0, col: 0 };
  private active = false;

  get isEmpty(): boolean {
    return !this.active;
  }

  clear(): void {
    this.active = false;
  }

  /** The rectangle, or `undefined` when nothing is selected. Allocates one object. */
  normalized(): SelectionRect | undefined {
    if (!this.active) return undefined;
    return {
      top: Math.min(this.anchor.row, this.head.row),
      bottom: Math.max(this.anchor.row, this.head.row),
      left: Math.min(this.anchor.col, this.head.col),
      right: Math.max(this.anchor.col, this.head.col),
    };
  }

  contains(row: number, col: number): boolean {
    const r = this.normalized();
    return r !== undefined && row >= r.top && row <= r.bottom && col >= r.left && col <= r.right;
  }

  /** Rows × columns the selection covers. May be 10 M; never iterated to get it. */
  get area(): number {
    const r = this.normalized();
    if (r === undefined) return 0;
    return (r.bottom - r.top + 1) * (r.right - r.left + 1);
  }

  moveTo(row: number, col: number): void {
    this.anchor = { row, col };
    this.head = { row, col };
    this.active = true;
  }

  extendTo(row: number, col: number): void {
    if (!this.active) {
      this.moveTo(row, col);
      return;
    }
    this.head = { row, col };
  }

  selectAll(rows: number, cols: number): void {
    if (rows === 0 || cols === 0) return;
    this.anchor = { row: 0, col: 0 };
    this.head = { row: rows - 1, col: cols - 1 };
    this.active = true;
  }

  selectColumn(col: number, rows: number): void {
    if (rows === 0) return;
    this.anchor = { row: 0, col };
    this.head = { row: rows - 1, col };
    this.active = true;
  }

  selectRow(row: number, cols: number): void {
    if (cols === 0) return;
    this.anchor = { row, col: 0 };
    this.head = { row, col: cols - 1 };
    this.active = true;
  }
}

// ---------------------------------------------------------------------------
// Keyboard navigation
// ---------------------------------------------------------------------------

/** The subset of a `KeyboardEvent` navigation reads. */
export interface NavKey {
  key: string;
  shiftKey?: boolean;
  ctrlKey?: boolean;
  metaKey?: boolean;
  altKey?: boolean;
}

/**
 * Moves or extends the selection, and reveals the result.
 *
 * `Mod` is Ctrl on Windows/Linux and Cmd on macOS, matching `keys/accelerator.ts`;
 * both are accepted here because this handler is below the keymap layer and
 * receives whatever the platform sent.
 *
 * Returns `true` when the key was consumed, so the caller can `preventDefault`
 * for exactly the keys the grid owns and no others.
 */
export function navigate(engine: GridEngine, selection: SelectionModel, event: NavKey): boolean {
  const rows = engine.reachableRows;
  const cols = engine.columns.length;
  if (rows === 0 || cols === 0) return false;

  const mod = event.ctrlKey === true || event.metaKey === true;
  const extend = event.shiftKey === true;
  const page = Math.max(1, engine.visibleRowCount - 1);
  const from = selection.isEmpty ? { row: 0, col: 0 } : selection.head;
  let { row, col } = from;

  switch (event.key) {
    case "ArrowDown":
      row = mod ? rows - 1 : row + 1;
      break;
    case "ArrowUp":
      row = mod ? 0 : row - 1;
      break;
    case "ArrowRight":
      col = mod ? cols - 1 : col + 1;
      break;
    case "ArrowLeft":
      col = mod ? 0 : col - 1;
      break;
    case "PageDown":
      row += page;
      break;
    case "PageUp":
      row -= page;
      break;
    case "Home":
      col = 0;
      if (mod) row = 0;
      break;
    case "End":
      col = cols - 1;
      if (mod) row = rows - 1;
      break;
    case "a":
    case "A":
      if (!mod) return false;
      selection.selectAll(rows, cols);
      return true;
    default:
      return false;
  }

  row = Math.min(rows - 1, Math.max(0, row));
  col = Math.min(cols - 1, Math.max(0, col));
  if (extend) selection.extendTo(row, col);
  else selection.moveTo(row, col);
  engine.revealCell(row, col);
  return true;
}

// ---------------------------------------------------------------------------
// Copy
// ---------------------------------------------------------------------------

export type CopyFormat = "tsv" | "csv" | "stata-list";

export interface CopyResult {
  text: string;
  /** False when part of the selection was not resident and could not be read. */
  complete: boolean;
  rows: number;
  /** Rows the selection asked for. `rows < requestedRows` means truncation. */
  requestedRows: number;
}

/**
 * The largest selection this copies without the engine's help.
 *
 * Not a taste number: it is roughly one screen of a 60-column grid times the
 * resident page budget, which is the honest limit of what the pane can read
 * without a `data_copy` command to ask for the rest.
 */
export const MAX_LOCAL_COPY_ROWS = 5000;

/** A CSV field, quoted only when it has to be. */
function csvField(value: string): string {
  return /[",\n\r]/.test(value) ? `"${value.replaceAll('"', '""')}"` : value;
}

/**
 * Copies the selection out of whatever is resident.
 *
 * Reads through the same `CellSource` the painter reads, so what lands on the
 * clipboard is what was on the screen — the formatted, value-labelled,
 * missing-value-honest text, not a re-derivation of it.
 */
export function copySelection(
  source: CellSource,
  columns: readonly GridColumn[],
  selection: SelectionModel,
  format: CopyFormat = "tsv",
): CopyResult {
  const rect = selection.normalized();
  if (rect === undefined) {
    return { text: "", complete: true, rows: 0, requestedRows: 0 };
  }

  const requestedRows = rect.bottom - rect.top + 1;
  const limit = Math.min(rect.bottom, rect.top + MAX_LOCAL_COPY_ROWS - 1);
  let complete = limit === rect.bottom;
  const lines: string[] = [];

  const names: string[] = [];
  for (let c = rect.left; c <= rect.right; c++) {
    names.push(columns[c]?.name ?? "");
  }
  if (format !== "stata-list") {
    lines.push(format === "csv" ? names.map(csvField).join(",") : names.join("\t"));
  }

  const cells: string[] = [];
  for (let row = rect.top; row <= limit; row++) {
    cells.length = 0;
    for (let c = rect.left; c <= rect.right; c++) {
      const column = columns[c];
      if (column === undefined) continue;
      const value = source.cell(row, column);
      if (value === undefined) complete = false;
      cells.push(value ?? "");
    }
    switch (format) {
      case "csv":
        lines.push(cells.map(csvField).join(","));
        break;
      case "stata-list":
        // Stata's own `list` gutter: `  1. ` then the values, one space apart.
        lines.push(`${String(row + 1).padStart(3, " ")}. ${cells.join(" ")}`);
        break;
      default:
        lines.push(cells.join("\t"));
    }
  }

  return {
    text: lines.join("\n"),
    complete,
    rows: Math.max(0, limit - rect.top + 1),
    requestedRows,
  };
}
