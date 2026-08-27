/**
 * The off-screen mirror — the plan's "Off-screen `role="grid"` DOM mirror with
 * true `aria-rowindex` out of 10 M".
 *
 * 06 §15.3 names this as the price of drawing on a canvas, and the whole value
 * of the payment is in the word "true": a screen-reader user scrolling a 10 M-row
 * dataset must be told where they are in the DATASET, not where they are in our
 * buffer. A mirror that numbered its forty rendered rows 1–40 would pass a
 * shallow accessibility audit and tell every one of those users a lie.
 */

import { beforeEach, describe, expect, test } from "vitest";
import { AUTO_VARS, autoDisplay } from "../panes/dataeditor/harness";
import { GridMirror, ariaColIndex, ariaRowIndex } from "./a11y";
import {
  type CellSource,
  type GridColumn,
  GridEngine,
  columnsFromVariables,
  counters,
  resetGridCounters,
} from "./engine";
import { SelectionModel } from "./select";

const COLUMNS = columnsFromVariables(AUTO_VARS);

const oracleSource: CellSource = {
  cell(row: number, column: GridColumn): string | undefined {
    const col = autoDisplay.column(column.idx);
    return col?.kind === "text" ? col.cell(row % autoDisplay.nrows) : undefined;
  },
};

const pending: CellSource = { cell: () => undefined };

function engineAt(row: number): GridEngine {
  const engine = new GridEngine();
  engine.setColumns(COLUMNS);
  engine.setRowCount(10_000_000);
  engine.setViewport(960, 480);
  engine.scrollToRow(row);
  return engine;
}

/** The header cells, corner first — `aria-colindex` 1 is the observation number. */
const headerCells = (mirror: GridMirror): Element[] => [
  ...(rows(mirror)[0]?.querySelectorAll('[role="columnheader"]') ?? []),
];

const rows = (mirror: GridMirror): HTMLElement[] =>
  [...mirror.element.querySelectorAll('[role="row"]')].filter(
    (el): el is HTMLElement => el instanceof HTMLElement,
  );

beforeEach(() => {
  resetGridCounters();
});

describe("true indices out of 10 M", () => {
  test("row 8 399 960 is announced as 8 399 962, not as 1", () => {
    const engine = engineAt(8_399_960);
    const mirror = new GridMirror({ doc: document, idPrefix: "grid" });
    const selection = new SelectionModel();
    mirror.update(engine, engine.materialize(oracleSource), selection);

    // ARIA counts the header as row 1, so observation `r` (0-based) is `r + 2`.
    expect(ariaRowIndex(8_399_960)).toBe(8_399_962);
    expect(mirror.element.getAttribute("aria-rowcount")).toBe("10000001");
    expect(mirror.element.getAttribute("aria-colcount")).toBe("13");
    expect(mirror.element.getAttribute("role")).toBe("grid");
    expect(mirror.element.tabIndex).toBe(0);

    const [header, first] = rows(mirror);
    expect(header?.getAttribute("aria-rowindex")).toBe("1");
    expect(first?.getAttribute("aria-rowindex")).toBe("8399962");
    // The row header carries Stata's own 1-based observation number.
    expect(first?.firstElementChild?.getAttribute("role")).toBe("rowheader");
    expect(first?.firstElementChild?.textContent).toBe("8399961");

    // The gutter is column 1, so the first variable is column 2.
    const cell = first?.querySelector('[role="gridcell"]');
    expect(cell?.getAttribute("aria-colindex")).toBe(String(ariaColIndex(0)));
    expect(cell?.textContent).toBe("AMC Concord");
    expect(cell?.id).toBe("grid-c-8399960-0");
  });

  test("the header states the type and the format, which the ink cannot", () => {
    const engine = engineAt(0);
    // Wide enough that all twelve variables are in the window; the mirror only
    // ever describes the window, which is the point of it.
    engine.setViewport(2000, 480);
    const mirror = new GridMirror({ doc: document });
    mirror.update(engine, engine.materialize(oracleSource), new SelectionModel());
    const cells = headerCells(mirror);
    // The first `columnheader` is the observation-number corner, which is why
    // `ariaColIndex` starts the variables at 2.
    expect(cells[0]?.textContent).toBe("Observation");
    expect(cells[1]?.textContent).toBe("make, str18, %-18s, Make and model");
    expect(cells[12]?.textContent).toBe("foreign, byte, %8.0g, value label origin, Car origin");
    expect(cells[1]?.getAttribute("aria-sort")).toBe("none");
  });

  test("a sorted column says so, because there is no arrow to look at", () => {
    const engine = engineAt(0);
    const mirror = new GridMirror({ doc: document });
    mirror.setSort({ columnIndex: 1, dir: "desc" });
    mirror.update(engine, engine.materialize(oracleSource), new SelectionModel());
    const cells = headerCells(mirror);
    expect(cells[2]?.getAttribute("aria-sort")).toBe("descending");
    expect(cells[1]?.getAttribute("aria-sort")).toBe("none");
  });

  test("a cell whose page is in flight is announced as loading, not as empty", () => {
    const engine = engineAt(9_000_000);
    const mirror = new GridMirror({ doc: document });
    mirror.update(engine, engine.materialize(pending), new SelectionModel());
    const cell = rows(mirror)[1]?.querySelector('[role="gridcell"]');
    // Silence would be announced as an empty cell, which is a different fact.
    expect(cell?.textContent).toBe("loading");
  });
});

describe("the focused cell and the selection", () => {
  test("aria-activedescendant follows the head, in absolute coordinates", () => {
    const engine = engineAt(4_000_000);
    const mirror = new GridMirror({ doc: document, idPrefix: "grid-default" });
    const selection = new SelectionModel();
    selection.moveTo(4_000_003, 2);
    mirror.update(engine, engine.materialize(oracleSource), selection);

    expect(mirror.element.getAttribute("aria-activedescendant")).toBe("grid-default-c-4000003-2");
    const target = document.createElement("div");
    target.append(mirror.element);
    expect(mirror.element.querySelector("#grid-default-c-4000003-2")).not.toBeNull();
  });

  test("a range marks exactly its own cells selected", () => {
    const engine = engineAt(0);
    const mirror = new GridMirror({ doc: document });
    const selection = new SelectionModel();
    selection.moveTo(1, 1);
    selection.extendTo(3, 2);
    mirror.update(engine, engine.materialize(oracleSource), selection);

    const body = rows(mirror).slice(1);
    const selected = body.flatMap((row, r) =>
      [...row.querySelectorAll('[role="gridcell"]')].map((cell, c) => ({
        r,
        c,
        on: cell.getAttribute("aria-selected") === "true",
      })),
    );
    for (const { r, c, on } of selected) {
      expect(on).toBe(r >= 1 && r <= 3 && c >= 1 && c <= 2);
    }
  });

  test("no selection means no active descendant at all", () => {
    const engine = engineAt(0);
    const mirror = new GridMirror({ doc: document });
    mirror.update(engine, engine.materialize(oracleSource), new SelectionModel());
    expect(mirror.element.hasAttribute("aria-activedescendant")).toBe(false);
  });
});

describe("readonly reflects the mode", () => {
  test("Browse is aria-readonly, Edit is not", () => {
    const mirror = new GridMirror({ doc: document });
    expect(mirror.element.getAttribute("aria-readonly")).toBe("true");
    mirror.setReadonly(false);
    expect(mirror.element.getAttribute("aria-readonly")).toBe("false");
  });
});

describe("the mirror is clipped, never hidden", () => {
  test("its class is the clipped one, and it carries no display:none", () => {
    const mirror = new GridMirror({ doc: document });
    // `display: none` and `visibility: hidden` both remove a subtree from the
    // accessibility tree, which would make this whole file a no-op that looked
    // like a feature. The CSS clips it instead; the element must not opt out.
    expect(mirror.element.className).toBe("grid__mirror");
    expect(mirror.element.style.display).toBe("");
    expect(mirror.element.style.visibility).toBe("");
    expect(mirror.element.hasAttribute("aria-hidden")).toBe(false);
  });
});

describe("node churn is a counter", () => {
  test("scrolling a million rows creates no new nodes", () => {
    const engine = engineAt(0);
    const mirror = new GridMirror({ doc: document });
    const selection = new SelectionModel();
    mirror.update(engine, engine.materialize(oracleSource), selection);

    resetGridCounters();
    for (let i = 0; i < 200; i++) {
      engine.scrollToRow(1_000_000 + i * 5000);
      mirror.update(engine, engine.materialize(oracleSource), selection);
    }
    expect(counters.mirrorUpdates).toBe(200);
    expect(counters.mirrorNodesCreated).toBe(0);
    expect(counters.mirrorCellsWritten).toBeGreaterThan(0);
  });
});
