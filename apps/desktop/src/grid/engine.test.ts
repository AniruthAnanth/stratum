/**
 * The engine's geometry and its classification of a cell — W18, 06 §15.3.
 *
 * Everything asserted here is checked against something that is not our own
 * formatter: the column table comes from `tests/fixtures/sdp1/README.md`'s own
 * transcription of `describe`, and every cell string comes from
 * `auto_40x12.bin`, captured from StataNow 18.5 MP. `inkFor` in particular is a
 * claim about Stata's output — `4,099` is a number, `Domestic` is a label, `.`
 * is a missing — and checking it against strings we generated would be checking
 * our formatter against itself.
 *
 * The counter assertions live in `grid.perf.test.ts`; this file is about being
 * right rather than about being cheap.
 */

import { beforeEach, describe, expect, test } from "vitest";
import { AUTO_VARS, autoDisplay } from "../panes/dataeditor/harness";
import {
  DEFAULT_METRICS,
  DOM_SOFT_CAP,
  GridEngine,
  columnsFromVariables,
  counters,
  formatWidth,
  inkFor,
  isMissingText,
  isStringStorage,
  resetGridCounters,
  storageWidthOf,
} from "./engine";

const columns = (): ReturnType<typeof columnsFromVariables> => columnsFromVariables(AUTO_VARS);

/** The oracle's display text for one cell of `auto_40x12.bin`. */
function oracle(idx: number, row: number): string {
  const col = autoDisplay.column(idx);
  if (col === undefined || col.kind !== "text") throw new Error(`column ${idx} is not text`);
  return col.cell(row);
}

beforeEach(() => {
  resetGridCounters();
});

describe("columns are laid out once, from the format", () => {
  test("width comes from the display format and the name, never from a cell", () => {
    const laid = columns();
    expect(laid).toHaveLength(12);

    const make = laid[0];
    const price = laid[1];
    const displacement = laid[9];
    // `%-18s` asks for 18 characters and `make` is 4, so the format wins; for
    // `displacement` (`%8.0g`, 12-character name) the name wins. Neither is the
    // widest VALUE, which is unknowable without reading 10 M rows.
    expect(make?.width).toBeGreaterThan(price?.width ?? 0);
    expect(displacement?.width).toBeGreaterThan(price?.width ?? 0);
    for (const column of laid) {
      expect(column.width).toBeGreaterThanOrEqual(DEFAULT_METRICS.minColumnWidth);
      expect(column.width).toBeLessThanOrEqual(DEFAULT_METRICS.maxColumnWidth);
    }
  });

  test("one layout pass per column set, not per column", () => {
    resetGridCounters();
    columns();
    expect(counters.columnLayouts).toBe(1);
    columns();
    expect(counters.columnLayouts).toBe(2);
  });

  test("alignment and storage width follow Stata", () => {
    const laid = columns();
    expect(laid[0]?.align).toBe("left"); // str18, and `%-18s` says so twice
    expect(laid[1]?.align).toBe("right"); // int
    expect(laid.map((c) => c.storageWidth)).toEqual([18, 2, 2, 2, 4, 2, 2, 2, 2, 2, 4, 1]);
    expect(laid[11]?.valueLabel).toBe("origin");
  });

  test("format widths, including the datetime ones Stata prints wide", () => {
    expect(formatWidth("%8.0gc")).toBe(8);
    expect(formatWidth("%-18s")).toBe(18);
    expect(formatWidth("%6.1f")).toBe(6);
    expect(formatWidth("%tc")).toBe(18);
    expect(formatWidth("%td")).toBe(9);
    expect(formatWidth("nonsense")).toBe(9);
  });

  test("storage widths, with strL refusing to invent one", () => {
    expect(storageWidthOf("str18")).toBe(18);
    expect(storageWidthOf("double")).toBe(8);
    expect(storageWidthOf("byte")).toBe(1);
    // A strL observation is an 8-byte pointer plus an unbounded payload; Stata's
    // own `describe` reports the type and not a width.
    expect(storageWidthOf("strL")).toBe(0);
    expect(isStringStorage("strL")).toBe(true);
    expect(isStringStorage("str1")).toBe(true);
    expect(isStringStorage("float")).toBe(false);
  });
});

describe("cell ink, against the oracle's own strings", () => {
  test("the four kinds 06 §9.7 names, plus pending", () => {
    const laid = columns();
    const make = laid[0];
    const price = laid[1];
    const rep78 = laid[3];
    const foreign = laid[11];
    if (make === undefined || price === undefined || rep78 === undefined || foreign === undefined) {
      throw new Error("auto.dta's columns did not load");
    }

    expect(oracle(0, 0)).toBe("AMC Concord");
    expect(inkFor(make, oracle(0, 0))).toBe("text");

    // `%8.0gc` puts a comma in it and the classifier must still read it as a number.
    expect(oracle(1, 0)).toBe("4,099");
    expect(inkFor(price, oracle(1, 0))).toBe("numeric");

    // README §3: `rep78` is missing at observations 3 and 7.
    expect(oracle(3, 2)).toBe(".");
    expect(inkFor(rep78, oracle(3, 2))).toBe("missing");
    expect(isMissingText(oracle(3, 2))).toBe(true);

    // A labelled value prints as its label, and the label is not a number.
    expect(oracle(11, 0)).toBe("Domestic");
    expect(inkFor(foreign, oracle(11, 0))).toBe("labelled");

    // A page still in flight is not a value and must not be inked like one.
    expect(inkFor(price, undefined)).toBe("pending");
  });

  test("a labelled column with no matching entry falls back to the number (README §2.4)", () => {
    const foreign = columns()[11];
    if (foreign === undefined) throw new Error("foreign did not load");
    expect(inkFor(foreign, "3")).toBe("numeric");
    expect(inkFor(foreign, ".")).toBe("missing");
  });
});

describe("the visible window over 10 M rows", () => {
  const engine = (): GridEngine => {
    const e = new GridEngine();
    e.setColumns(columns());
    e.setRowCount(10_000_000);
    e.setViewport(960, 480);
    return e;
  };

  test("the window is a function of the viewport, not of the row count", () => {
    const big = engine();
    const small = engine();
    small.setRowCount(10_000);

    const a = big.visibleWindow();
    const b = small.visibleWindow();
    expect(a.rowCount).toBe(b.rowCount);
    expect(a.colCount).toBe(b.colCount);
    // One partial row at each edge, and no more.
    expect(a.rowCount).toBe(big.visibleRowCount + 2);
  });

  test("scrolling to the last observation of 10 M keeps the row exact", () => {
    const e = engine();
    e.scrollToRow(Number.MAX_SAFE_INTEGER);
    // 53 bits of mantissa: 10 M is exact, and so is 10 M minus the viewport.
    expect(e.scrollRow).toBe(e.maxScrollRow);
    expect(e.maxScrollRow).toBe(10_000_000 - e.visibleRowCount);
    const w = e.visibleWindow();
    expect(w.row0).toBe(e.maxScrollRow);
    expect(w.row0 + w.rowCount).toBe(10_000_000);
  });

  test("a fractional position produces a sub-row pixel offset, not a jump", () => {
    const e = engine();
    e.scrollToRow(1_234_567.5);
    const w = e.visibleWindow();
    expect(w.row0).toBe(1_234_567);
    expect(w.yOffset).toBeCloseTo(-DEFAULT_METRICS.rowHeight / 2, 10);
  });

  test("columnAt is a search over prefix sums, and it is right at 32 767 columns", () => {
    const wide = Array.from({ length: 32_767 }, (_, i) => ({
      idx: i,
      name: `v${i}`,
      storage: "int",
      format: "%8.0g",
    }));
    const e = new GridEngine();
    e.setColumns(columnsFromVariables(wide));
    e.setViewport(960, 480);
    for (const i of [0, 1, 500, 16_383, 32_766]) {
      const left = e.columnLeft(i);
      const width = e.columns[i]?.width ?? 0;
      expect(e.columnAt(left)).toBe(i);
      expect(e.columnAt(left + width - 1)).toBe(i);
    }
  });

  test("hit-testing rejects the chrome and the rows past the end", () => {
    const e = engine();
    expect(e.hitTest(10, 4)).toBeUndefined(); // the header
    expect(e.hitTest(10, 100)).toBeUndefined(); // the gutter
    const hit = e.hitTest(DEFAULT_METRICS.gutterWidth + 4, DEFAULT_METRICS.headerHeight + 4);
    expect(hit).toEqual({ row: 0, col: 0 });
    e.setRowCount(3);
    expect(
      e.hitTest(DEFAULT_METRICS.gutterWidth + 4, DEFAULT_METRICS.headerHeight + 22 * 5),
    ).toBeUndefined();
  });
});

describe("Q8's soft cap is enforced in the scroll clamp", () => {
  test("a capped grid cannot be scrolled to a row it would not draw", () => {
    const e = new GridEngine();
    e.setColumns(columns());
    e.setRowCount(10_000_000);
    e.setViewport(960, 480);
    e.setSoftCap(DOM_SOFT_CAP);

    expect(e.capped).toBe(true);
    expect(e.reachableRows).toBe(DOM_SOFT_CAP);
    e.scrollToRow(5_000_000);
    expect(e.scrollRow).toBe(DOM_SOFT_CAP - e.visibleRowCount);
    const w = e.visibleWindow();
    expect(w.row0 + w.rowCount).toBe(DOM_SOFT_CAP);
    // `rowCount` is still the truth about the dataset; only the reach is capped.
    expect(e.rowCount).toBe(10_000_000);
  });

  test("an uncapped grid reports itself uncapped", () => {
    const e = new GridEngine();
    e.setColumns(columns());
    e.setRowCount(74);
    e.setViewport(960, 480);
    expect(e.capped).toBe(false);
    expect(e.reachableRows).toBe(74);
  });
});

describe("revealCell moves the minimum", () => {
  test("a cell below the fold scrolls by exactly the overshoot", () => {
    const e = new GridEngine();
    e.setColumns(columns());
    e.setRowCount(10_000_000);
    e.setViewport(960, 480);

    expect(e.revealCell(5, 0)).toBe(false); // already visible
    const target = e.visibleRowCount + 3;
    expect(e.revealCell(target, 0)).toBe(true);
    expect(e.scrollRow).toBe(target - e.visibleRowCount + 1);

    // Horizontally, the same rule against the prefix sums.
    const last = e.columns.length - 1;
    expect(e.revealCell(target, last)).toBe(true);
    expect(e.scrollX).toBe(e.columnLeft(last) + (e.columns[last]?.width ?? 0) - e.bodyWidth);
  });
});
