/**
 * The counters — ADR-017, and the plan's "60 fps on a 10 M × 12 frame".
 *
 * ADR-017 is binding: "Assert a COUNTER — rows rendered, decorations rebuilt,
 * allocations, IPC round-trips — never a wall-clock duration." So this file
 * never times anything. It asserts the things a frame rate is MADE of, and the
 * only honest form of "10 M rows are free" is that the same numbers come out at
 * 74 rows, at 10 000 and at 10 000 000.
 *
 * The load-bearing one is the first: **`rowsMaterialized` per frame equals the
 * visible window and nothing else.** A grid that materialised more would be a
 * grid whose scroll cost grows with the dataset, which is the failure 06 §15.3
 * exists to prevent, and it is the counter the brief asks for by name.
 */

import { beforeEach, describe, expect, test } from "vitest";
import { AUTO_VARS, autoDisplay, canvasSurface } from "../panes/dataeditor/harness";
import { GridMirror } from "./a11y";
import {
  type CellSource,
  type GridColumn,
  GridEngine,
  columnsFromVariables,
  counters,
  resetGridCounters,
  snapshotGridCounters,
} from "./engine";
import { DomSurface, type GridPalette, readPalette } from "./paint";
import { SelectionModel } from "./select";

const COLUMNS = columnsFromVariables(AUTO_VARS);

/** A `CellSource` that records exactly which cells were asked for. */
interface Recorder extends CellSource {
  readonly rowsAsked: Set<number>;
  calls: number;
}

/**
 * The oracle, cycled — the same rule `frameServer` uses, so the strings a frame
 * paints in this file are the strings StataNow 18.5 MP printed.
 */
function oracleSource(): Recorder {
  const rowsAsked = new Set<number>();
  const source: Recorder = {
    rowsAsked,
    calls: 0,
    cell(row: number, column: GridColumn): string | undefined {
      source.calls += 1;
      rowsAsked.add(row);
      const col = autoDisplay.column(column.idx);
      return col?.kind === "text" ? col.cell(row % autoDisplay.nrows) : undefined;
    },
  };
  return source;
}

/** Nothing is resident. Every cell is `undefined`, which is the placeholder signal. */
function emptySource(): Recorder {
  const rowsAsked = new Set<number>();
  const source: Recorder = {
    rowsAsked,
    calls: 0,
    cell(row: number): string | undefined {
      source.calls += 1;
      rowsAsked.add(row);
      return undefined;
    },
  };
  return source;
}

function engineWith(rows: number): GridEngine {
  const engine = new GridEngine();
  engine.setColumns(COLUMNS);
  engine.setRowCount(rows);
  engine.setViewport(960, 480);
  return engine;
}

beforeEach(() => {
  resetGridCounters();
});

describe("rows materialised per frame are bounded by the viewport", () => {
  test("the same window at 74 rows, at 10 000 and at 10 000 000", () => {
    const measured = [74, 10_000, 10_000_000].map((rows) => {
      resetGridCounters();
      const engine = engineWith(rows);
      const source = oracleSource();
      const cells = engine.materialize(source);
      return {
        rows,
        counters: snapshotGridCounters(),
        window: { ...cells.window },
        asked: source.calls,
        distinctRows: source.rowsAsked.size,
      };
    });

    const first = measured[0];
    if (first === undefined) throw new Error("no measurement");
    for (const m of measured) {
      // THE assertion the brief asks for: rows materialised is the window, and
      // the window is the viewport. It does not move when the dataset grows by
      // five orders of magnitude.
      expect(m.counters.rowsMaterialized).toBe(m.window.rowCount);
      expect(m.counters.rowsMaterialized).toBe(first.counters.rowsMaterialized);
      expect(m.counters.cellsMaterialized).toBe(m.window.rowCount * m.window.colCount);
      expect(m.counters.cellsMaterialized).toBe(first.counters.cellsMaterialized);
      // And the source was never asked for a row outside it, which is the same
      // claim stated from the other side.
      expect(m.asked).toBe(m.counters.cellsMaterialized);
      expect(m.distinctRows).toBe(m.window.rowCount);
    }
    expect(first.counters.rowsMaterialized).toBeLessThanOrEqual(24);
  });

  test("a thousand frames at the far end of 10 M cost a thousand windows", () => {
    const engine = engineWith(10_000_000);
    const perFrame = engine.materialize(oracleSource()).window.rowCount;

    const source = oracleSource();
    resetGridCounters();
    for (let i = 0; i < 1000; i++) {
      engine.scrollToRow(9_000_000 + i);
      engine.materialize(source);
    }
    expect(counters.windowsComputed).toBe(1000);
    expect(counters.rowsMaterialized).toBe(1000 * perFrame);
    // Scratch capacity is reused: a settled viewport grows it zero times.
    expect(counters.scratchAllocations).toBe(0);
    // The rows touched in total are the thousand windows, not the ten million.
    expect(source.rowsAsked.size).toBe(999 + perFrame);
  });

  test("the scratch buffer grows only when the viewport does", () => {
    const engine = engineWith(10_000_000);
    const source = oracleSource();
    engine.materialize(source);
    const afterFirst = counters.scratchAllocations;
    expect(afterFirst).toBe(1);

    for (let i = 0; i < 100; i++) engine.materialize(source);
    expect(counters.scratchAllocations).toBe(afterFirst);

    engine.setViewport(1600, 1200);
    engine.materialize(source);
    expect(counters.scratchAllocations).toBe(afterFirst + 1);
    // …and then settles again at the new size.
    for (let i = 0; i < 100; i++) engine.materialize(source);
    expect(counters.scratchAllocations).toBe(afterFirst + 1);
  });
});

describe("painting one frame", () => {
  const paletteFor = (): GridPalette => readPalette(document.createElement("div"));

  test("the canvas measures no text on the cell path, at any number of frames", () => {
    const engine = engineWith(10_000_000);
    const source = oracleSource();
    const selection = new SelectionModel();
    const { surface } = canvasSurface(document);
    surface.resize(960, 480, 1);
    const palette = paletteFor();

    resetGridCounters();
    for (let i = 0; i < 1000; i++) {
      engine.scrollToRow(8_400_000 + i);
      surface.paint({ engine, cells: engine.materialize(source), selection, palette });
    }
    expect(counters.framesPainted).toBe(1000);
    // The grid font is `--font-mono`, so an advance width is `length × chPx` and
    // `measureText` is not on this path. A grid that measured 2 400 cells a frame
    // is the grid that drops to 20 fps on the machine you do not own.
    expect(counters.textMeasures).toBe(0);
    // Painted ≤ materialised: the window carries one row past the bottom edge so
    // that a fractional scroll has something to reveal, and a row entirely below
    // the canvas is skipped rather than drawn off-screen.
    expect(counters.cellsPainted).toBeLessThanOrEqual(counters.cellsMaterialized);
    expect(counters.cellsPainted).toBeGreaterThanOrEqual(
      counters.cellsMaterialized - 1000 * COLUMNS.length,
    );
    // Cells that fit allocate nothing; `auto.dta`'s do at these column widths.
    expect(counters.paintAllocations).toBe(0);
    // One `getComputedStyle` for the palette, and it happened before the loop.
    expect(counters.styleReads).toBe(0);
  });

  test("every hairline in the grid is two stroke() calls", () => {
    const engine = engineWith(10_000_000);
    const selection = new SelectionModel();
    const { surface, ctx } = canvasSurface(document);
    surface.resize(960, 480, 1);
    surface.paint({
      engine,
      cells: engine.materialize(oracleSource()),
      selection,
      palette: paletteFor(),
    });
    expect(ctx.strokes).toBe(2);
  });

  test("in-flight rows paint ⋯ and the frame still completes", () => {
    const engine = engineWith(10_000_000);
    const source = emptySource();
    const selection = new SelectionModel();
    const { surface, ctx } = canvasSurface(document);
    surface.resize(960, 480, 1);

    resetGridCounters();
    const cells = engine.materialize(source);
    surface.paint({ engine, cells, selection, palette: paletteFor() });

    // "Scrolling never waits on data": nothing was awaited, every cell drew, and
    // every one of them drew the placeholder.
    expect(counters.placeholdersPainted).toBe(counters.cellsPainted);
    expect(counters.cellsPainted).toBe((cells.window.rowCount - 1) * cells.window.colCount);
    expect(ctx.texts.some((t) => t.text === "⋯")).toBe(true);
    // The gutter still numbers the observations it is over, out of 10 M.
    expect(ctx.texts.some((t) => t.text === String(engine.visibleWindow().row0 + 1))).toBe(true);
  });

  test("the DOM fallback pools its nodes: allocations settle at zero", () => {
    const engine = engineWith(10_000_000);
    const source = oracleSource();
    const selection = new SelectionModel();
    const surface = new DomSurface(document);
    const palette = paletteFor();

    surface.paint({ engine, cells: engine.materialize(source), selection, palette });
    resetGridCounters();
    for (let i = 0; i < 200; i++) {
      engine.scrollToRow(500_000 + i);
      surface.paint({ engine, cells: engine.materialize(source), selection, palette });
    }
    expect(counters.framesPainted).toBe(200);
    expect(counters.paintAllocations).toBe(0);
  });
});

describe("the accessibility mirror is repainted, not rebuilt", () => {
  test("node creation settles, and an unchanged window writes no cells", () => {
    const engine = engineWith(10_000_000);
    const source = oracleSource();
    const selection = new SelectionModel();
    const mirror = new GridMirror({ doc: document });

    mirror.update(engine, engine.materialize(source), selection);
    const created = counters.mirrorNodesCreated;
    expect(created).toBeGreaterThan(0);

    resetGridCounters();
    for (let i = 0; i < 100; i++) mirror.update(engine, engine.materialize(source), selection);
    expect(counters.mirrorUpdates).toBe(100);
    // The window did not move and no page landed, so the mirror wrote nothing
    // and created nothing: a selection-only repaint must not touch 520 nodes.
    expect(counters.mirrorNodesCreated).toBe(0);
    expect(counters.mirrorCellsWritten).toBe(0);

    // Scrolling one row rewrites the visible cells and still creates no nodes.
    engine.scrollByRows(1);
    mirror.update(engine, engine.materialize(source), selection);
    expect(counters.mirrorNodesCreated).toBe(0);
    expect(counters.mirrorCellsWritten).toBeGreaterThan(0);
  });
});
