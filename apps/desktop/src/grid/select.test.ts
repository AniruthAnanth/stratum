/**
 * Selection, keyboard navigation and copy — 06 §15.3's first cost of the canvas.
 *
 * "A canvas has no native text selection" is paid for here, and the two things
 * worth asserting are that the model never iterates (Mod+A over 10 M rows is
 * four number assignments) and that copy is HONEST: CONTRACTS §11 declares
 * `log_copy` for the log and no `data_copy` for a frame, so a selection larger
 * than what is resident cannot be fully copied and must say so rather than
 * silently hand the user a short clipboard.
 */

import { describe, expect, test } from "vitest";
import { AUTO_VARS, autoDisplay } from "../panes/dataeditor/harness";
import { type CellSource, type GridColumn, GridEngine, columnsFromVariables } from "./engine";
import { MAX_LOCAL_COPY_ROWS, SelectionModel, copySelection, navigate } from "./select";

const COLUMNS = columnsFromVariables(AUTO_VARS);

const oracleSource: CellSource = {
  cell(row: number, column: GridColumn): string | undefined {
    const col = autoDisplay.column(column.idx);
    return col?.kind === "text" ? col.cell(row % autoDisplay.nrows) : undefined;
  },
};

function engine(rows = 10_000_000): GridEngine {
  const e = new GridEngine();
  e.setColumns(COLUMNS);
  e.setRowCount(rows);
  e.setViewport(960, 480);
  return e;
}

describe("the selection is two cells and a derivation", () => {
  test("Mod+A over 10 M rows costs no iteration and reports the true area", () => {
    const selection = new SelectionModel();
    expect(selection.isEmpty).toBe(true);
    selection.selectAll(10_000_000, 12);
    expect(selection.area).toBe(120_000_000);
    expect(selection.normalized()).toEqual({ top: 0, bottom: 9_999_999, left: 0, right: 11 });
    expect(selection.contains(5_000_000, 6)).toBe(true);
    expect(selection.contains(10_000_000, 0)).toBe(false);
  });

  test("an extend backwards normalises, it does not swap the head", () => {
    const selection = new SelectionModel();
    selection.moveTo(500, 5);
    selection.extendTo(100, 2);
    expect(selection.head).toEqual({ row: 100, col: 2 });
    expect(selection.normalized()).toEqual({ top: 100, bottom: 500, left: 2, right: 5 });
  });

  test("a column selection is the whole column, at any row count", () => {
    const selection = new SelectionModel();
    selection.selectColumn(3, 10_000_000);
    expect(selection.normalized()).toEqual({ top: 0, bottom: 9_999_999, left: 3, right: 3 });
    selection.selectRow(9_999_999, 12);
    expect(selection.normalized()).toEqual({
      top: 9_999_999,
      bottom: 9_999_999,
      left: 0,
      right: 11,
    });
  });
});

describe("keyboard navigation", () => {
  test("Mod+ArrowDown goes to observation 10 000 000 and reveals it", () => {
    const e = engine();
    const selection = new SelectionModel();
    expect(navigate(e, selection, { key: "ArrowDown", ctrlKey: true })).toBe(true);
    expect(selection.head).toEqual({ row: 9_999_999, col: 0 });
    expect(e.scrollRow).toBe(e.maxScrollRow);
  });

  test("PageDown steps a viewport minus one, keeping a row of overlap", () => {
    const e = engine();
    const selection = new SelectionModel();
    selection.moveTo(0, 0);
    navigate(e, selection, { key: "PageDown" });
    expect(selection.head.row).toBe(e.visibleRowCount - 1);
  });

  test("Shift extends instead of moving", () => {
    const e = engine();
    const selection = new SelectionModel();
    selection.moveTo(10, 3);
    navigate(e, selection, { key: "ArrowDown", shiftKey: true });
    expect(selection.normalized()).toEqual({ top: 10, bottom: 11, left: 3, right: 3 });
  });

  test("keys the grid does not own are left alone", () => {
    const e = engine();
    const selection = new SelectionModel();
    expect(navigate(e, selection, { key: "Backspace" })).toBe(false);
    expect(navigate(e, selection, { key: "a" })).toBe(false);
    expect(navigate(e, selection, { key: "a", metaKey: true })).toBe(true);
  });

  test("navigation on an empty grid does nothing rather than clamping to -1", () => {
    const e = new GridEngine();
    const selection = new SelectionModel();
    expect(navigate(e, selection, { key: "ArrowDown" })).toBe(false);
  });
});

describe("copy is what was on the screen, and says when it is short", () => {
  test("TSV carries the oracle's own formatted values, header first", () => {
    const selection = new SelectionModel();
    selection.moveTo(0, 0);
    selection.extendTo(1, 1);
    const result = copySelection(oracleSource, COLUMNS, selection, "tsv");
    expect(result.complete).toBe(true);
    expect(result.text.split("\n")).toEqual([
      "make\tprice",
      "AMC Concord\t4,099",
      "AMC Pacer\t4,749",
    ]);
  });

  test("CSV quotes only what it must", () => {
    const selection = new SelectionModel();
    selection.moveTo(0, 0);
    selection.extendTo(0, 1);
    const result = copySelection(oracleSource, COLUMNS, selection, "csv");
    // `4,099` has a comma in it and would otherwise become two fields.
    expect(result.text.split("\n")[1]).toBe('AMC Concord,"4,099"');
  });

  test("stata-list uses Stata's own gutter and no header", () => {
    const selection = new SelectionModel();
    selection.moveTo(2, 0);
    selection.extendTo(2, 1);
    const result = copySelection(oracleSource, COLUMNS, selection, "stata-list");
    expect(result.text).toBe("  3. AMC Spirit 3,799");
  });

  test("a 10 M-row selection copies what is resident and reports the truncation", () => {
    const selection = new SelectionModel();
    selection.selectAll(10_000_000, 12);
    const result = copySelection(oracleSource, COLUMNS, selection, "tsv");
    // No `data_copy` command exists to ask the engine for the rest, and
    // inventing one would violate R1. So: copy what we have, say what we did.
    expect(result.complete).toBe(false);
    expect(result.rows).toBe(MAX_LOCAL_COPY_ROWS);
    expect(result.requestedRows).toBe(10_000_000);
  });

  test("a cell whose page is not resident marks the copy incomplete", () => {
    const half: CellSource = {
      cell: (row, column) => (row < 2 ? oracleSource.cell(row, column) : undefined),
    };
    const selection = new SelectionModel();
    selection.moveTo(0, 0);
    selection.extendTo(3, 0);
    const result = copySelection(half, COLUMNS, selection, "tsv");
    expect(result.complete).toBe(false);
    expect(result.rows).toBe(4);
    expect(result.text.split("\n")).toEqual(["make", "AMC Concord", "AMC Pacer", "", ""]);
  });

  test("an empty selection copies nothing and is complete about it", () => {
    const result = copySelection(oracleSource, COLUMNS, new SelectionModel());
    expect(result).toEqual({ text: "", complete: true, rows: 0, requestedRows: 0 });
  });
});
