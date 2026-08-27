/**
 * The Data Editor, whole — the plan's acceptance bullets driven through the real
 * controller over the real transport.
 *
 *  * "scrolling **never waits on data** (in-flight rows paint `⋯` placeholders)"
 *  * "Pages are fetched from `stratum-asset://localhost/frame/…`"
 *  * "**Every edit issues `replace <var> = <val> in <n>`.**"
 *  * "Status bar matches Stata's fields and order"
 *  * "**Q8 spike lands here**… Documented fallback is DOM virtualisation with a
 *    1 M-row soft cap, not a stuttering grid."
 *
 * **What jsdom cannot do.** `HTMLCanvasElement.getContext("2d")` returns `null`
 * here, so `createSurface` correctly declines canvas and this file exercises the
 * Q8 FALLBACK end to end — including the soft cap, which is why the row counts
 * below are 1 000 000 and not 10 000 000. The canvas painter is driven directly
 * in `grid/grid.perf.test.ts` against a recording context, and the engine is
 * driven at 10 M rows in `grid/engine.test.ts`. Between the three, both surfaces
 * and both row scales are covered by something that actually ran.
 */

import { afterEach, beforeEach, describe, expect, test } from "vitest";
import { DOM_SOFT_CAP, columnsFromVariables, counters, resetGridCounters } from "../../grid/engine";
import { PLACEHOLDER } from "../../grid/paint";
import { asDatasetStateId, asSessionId } from "../../ipc/hand";
import { DataGridController, type GridStatus } from "./controller";
import { AUTO_VARS, frameServer, settle } from "./harness";
import { statusFields, statusLine } from "./status";

const SESSION = asSessionId(1);
const STATE = asDatasetStateId(17);
const COLUMNS = columnsFromVariables(AUTO_VARS);

interface Fixture {
  grid: DataGridController;
  server: ReturnType<typeof frameServer>;
  edits: string[];
  notices: (string | undefined)[];
  headers: number[];
  status: () => GridStatus;
}

let live: DataGridController | undefined;

function mount(options: { rows?: number; mode?: "immediate" | "manual" } = {}): Fixture {
  const rows = options.rows ?? 10_000_000;
  const server = frameServer({
    rows,
    ...(options.mode === undefined ? {} : { mode: options.mode }),
  });
  const edits: string[] = [];
  const notices: (string | undefined)[] = [];
  const headers: number[] = [];
  let last: GridStatus | undefined;

  const grid = new DataGridController({
    session: SESSION,
    frame: "default",
    state: STATE,
    doc: document,
    fetchAsset: server.fetchAsset,
    // The frame loop, run synchronously: `requestAnimationFrame` in jsdom would
    // make every assertion below a race.
    schedule: (callback) => callback(),
    onEdit: (command) => edits.push(command),
    onNotice: (text) => notices.push(text),
    onStatus: (status) => {
      last = status;
    },
    onHeaderActivate: (index) => headers.push(index),
  });
  live = grid;
  document.body.appendChild(grid.element);
  grid.setColumns(COLUMNS);
  grid.setRowCount(rows);
  grid.layout(960, 480);

  return {
    grid,
    server,
    edits,
    notices,
    headers,
    status: () => {
      if (last === undefined) throw new Error("no status was ever reported");
      return last;
    },
  };
}

/** jsdom's `MouseEvent` has no offsets; the controller hit-tests with them. */
function pointer(target: HTMLElement, type: string, x: number, y: number): void {
  const event = new MouseEvent(type, { bubbles: true, cancelable: true });
  Object.defineProperty(event, "offsetX", { value: x });
  Object.defineProperty(event, "offsetY", { value: y });
  target.dispatchEvent(event);
}

const cellTexts = (grid: DataGridController): string[] =>
  [...grid.element.querySelectorAll(".grid__cell")].map((el) => el.textContent ?? "");

beforeEach(() => {
  resetGridCounters();
  document.body.replaceChildren();
});

afterEach(() => {
  live?.dispose();
  live = undefined;
});

describe("Q8: the fallback is a documented cap, not a stuttering grid", () => {
  test("with no 2D context the grid takes the DOM surface and caps the reach", () => {
    const f = mount();
    expect(f.status().surface).toBe("dom");
    expect(f.status().capped).toBe(true);
    expect(f.grid.engine.reachableRows).toBe(DOM_SOFT_CAP);
    // The cap is in the scroll clamp, so a capped grid cannot be scrolled to a
    // row it would not draw.
    f.grid.engine.scrollToRow(5_000_000);
    expect(f.grid.engine.scrollRow).toBe(DOM_SOFT_CAP - f.grid.engine.visibleRowCount);
    // `Obs:` still tells the truth about the dataset.
    expect(f.status().obs).toBe(10_000_000);
  });
});

describe("scrolling never waits on data", () => {
  test("the first frame paints ⋯ and the second paints Stata's own strings", async () => {
    const f = mount({ mode: "manual" });

    // Nothing is resident yet; the frame completed anyway.
    expect(counters.placeholdersPainted).toBeGreaterThan(0);
    expect(cellTexts(f.grid).filter((t) => t === PLACEHOLDER).length).toBeGreaterThan(0);
    expect(counters.framesPainted).toBeGreaterThan(0);

    await f.server.flush();
    f.grid.paint();
    expect(cellTexts(f.grid)).toContain("AMC Concord");
    expect(cellTexts(f.grid)).toContain("4,099");
    // The `.` for a missing `rep78` is Stata's, and it survives to the pixel.
    expect(cellTexts(f.grid)).toContain(".");
  });

  test("a hundred scroll frames issue no IPC and materialise only their windows", async () => {
    const f = mount();
    await settle();
    const perFrame = f.grid.engine.visibleWindow().rowCount;

    resetGridCounters();
    for (let i = 0; i < 100; i++) {
      f.grid.engine.scrollToRow(500_000 + i);
      f.grid.paint();
    }
    expect(counters.framesPainted).toBe(100);
    expect(counters.rowsMaterialized).toBe(100 * perFrame);
    // Not one Tauri command on the scroll path: the pages are an asset fetch and
    // `data_page` does not exist (A13).
    expect(counters.ipcInvocations).toBe(0);
    for (const request of f.server.requests) {
      expect(request.url.startsWith("stratum-asset://localhost/frame/")).toBe(true);
    }
    // A hundred one-row steps stay inside a handful of 200-row pages.
    expect(counters.pageRequests).toBeLessThan(20);
  });

  test("throwing the scrollbar across the dataset aborts what it left behind", async () => {
    const f = mount({ mode: "manual" });
    expect(f.grid.engine.scrollRow).toBe(0);
    f.grid.engine.scrollToRow(900_000);
    f.grid.paint();
    expect(counters.pageAborts).toBeGreaterThan(0);
    await f.server.flush();
    expect(f.server.aborts()).toBeGreaterThan(0);
  });
});

describe("the status bar is Stata's, field for field", () => {
  test("Vars, Order, Obs, Length, Filter — in that order and no other", async () => {
    const f = mount({ rows: 74 });
    await settle();
    f.grid.paint();

    expect(statusFields(f.status()).map((field) => field.label)).toEqual([
      "Vars",
      "Order",
      "Obs",
      "Length",
      "Filter",
    ]);
    // 06 §9.7 gives the line verbatim, four spaces between fields.
    expect(statusLine(f.status())).toBe(
      "Vars: 12    Order: Dataset    Obs: 74    Length: 18    Filter: Off",
    );
  });

  test("Length follows the cursor's variable, as Stata's does", async () => {
    const f = mount({ rows: 74 });
    await settle();
    f.grid.selection.moveTo(0, 1); // price, an int
    f.grid.paint();
    expect(f.status().length).toBe(2);
    f.grid.selection.moveTo(0, 4); // headroom, a float
    f.grid.paint();
    expect(f.status().length).toBe(4);
  });

  test("Obs on a 10 M frame is grouped, or it is unreadable", () => {
    const f = mount();
    expect(statusLine({ ...f.status(), obs: 10_000_000 })).toContain("Obs: 10,000,000");
  });
});

describe("every edit issues `replace <var> = <val> in <n>`", () => {
  test("editing price observation 1 submits the RAW value, not the displayed one", async () => {
    const f = mount({ rows: 74 });
    f.grid.setMode("edit");
    await settle();
    f.grid.paint();
    await settle();

    f.grid.beginEdit(0, 1);
    // Seeded from `RenderMode::Edit`: the value is 4099 and the cell says 4,099.
    expect(f.grid.editor.element.value).toBe("4099");
    f.grid.editor.element.value = "4200";
    f.grid.editor.element.dispatchEvent(
      new KeyboardEvent("keydown", { key: "Enter", bubbles: true, cancelable: true }),
    );

    expect(f.edits).toEqual(["replace price = 4200 in 1"]);
    expect(counters.editsCommitted).toBe(1);
    // The command went out; nothing was mutated behind the log's back.
    expect(counters.ipcInvocations).toBe(0);
  });

  test("a value Stata could not parse is refused with a reason, not sent", async () => {
    const f = mount({ rows: 74 });
    f.grid.setMode("edit");
    await settle();
    f.grid.paint();
    await settle();

    f.grid.beginEdit(0, 1);
    f.grid.editor.element.value = "4,200";
    f.grid.editor.element.dispatchEvent(
      new KeyboardEvent("keydown", { key: "Enter", bubbles: true, cancelable: true }),
    );
    expect(f.edits).toEqual([]);
    expect(f.notices.at(-1)).toContain("not a number");
  });

  test("Browse mode does not open an editor at all", async () => {
    const f = mount({ rows: 74 });
    await settle();
    f.grid.beginEdit(0, 1);
    expect(f.grid.editor.isOpen).toBe(false);
    expect(counters.editsBegun).toBe(0);
  });

  test("a sorted view drops to Browse and says why", async () => {
    const f = mount({ rows: 74 });
    f.grid.setMode("edit");
    await settle();
    expect(f.grid.currentMode).toBe("edit");

    f.grid.setOrder(7, 30, "-price", "Off", { columnIndex: 1, dir: "desc" });
    // `replace … in n` counts OBSERVATIONS and a view row is not one; nothing on
    // the wire maps one to the other. Read-only is the honest response.
    expect(f.grid.currentMode).toBe("browse");
    expect(f.notices.at(-1)).toContain("read-only");
    expect(f.status().order).toBe("-price");
    expect(f.status().obs).toBe(30);
    // And Edit cannot be re-entered while the order is live.
    f.grid.setMode("edit");
    expect(f.grid.currentMode).toBe("browse");
  });
});

describe("the pane's own input", () => {
  test("a click selects the cell under it and focuses the mirror", async () => {
    const f = mount({ rows: 74 });
    await settle();
    const viewport = f.grid.element.querySelector(".grid__viewport") as HTMLElement;
    pointer(viewport, "pointerdown", 60 + 4, 26 + 22 * 2 + 4);
    expect(f.grid.selection.head).toEqual({ row: 2, col: 0 });
    expect(document.activeElement).toBe(f.grid.mirror.element);
  });

  test("a click on the header asks for a sort rather than sorting here", async () => {
    const f = mount({ rows: 74 });
    await settle();
    const viewport = f.grid.element.querySelector(".grid__viewport") as HTMLElement;
    pointer(viewport, "pointerdown", 60 + 4, 4);
    // 06 §15.3 requires sorting to happen in Rust; the pane turns this into
    // `data_order_set`, and the grid itself never compares two cells.
    expect(f.headers).toEqual([0]);
    expect(counters.ipcInvocations).toBe(0);
  });

  test("the arrow keys arrive at the mirror, which is what a screen reader reads", async () => {
    const f = mount({ rows: 74 });
    await settle();
    f.grid.selection.moveTo(0, 0);
    f.grid.mirror.element.dispatchEvent(
      new KeyboardEvent("keydown", { key: "ArrowDown", bubbles: true, cancelable: true }),
    );
    expect(f.grid.selection.head).toEqual({ row: 1, col: 0 });
    expect(f.grid.mirror.element.getAttribute("aria-activedescendant")).toContain("-c-1-0");
  });

  test("the drawn surface is hidden from assistive technology, the mirror is not", () => {
    const f = mount({ rows: 74 });
    const surface = f.grid.element.querySelector(".grid__dom");
    expect(surface?.getAttribute("aria-hidden")).toBe("true");
    expect(f.grid.mirror.element.getAttribute("role")).toBe("grid");
    expect(f.grid.mirror.element.hasAttribute("aria-hidden")).toBe(false);
  });
});

describe("the frame advancing under us", () => {
  test("an invalidate drops every resident page rather than showing a stale one", async () => {
    const f = mount({ rows: 74 });
    await settle();
    f.grid.paint();
    expect(cellTexts(f.grid)).toContain("AMC Concord");

    f.grid.invalidate(asDatasetStateId(18));
    f.grid.paint();
    // CONTRACTS §8.1: the dataset advanced, so nothing resident describes it.
    expect(cellTexts(f.grid).filter((t) => t === PLACEHOLDER).length).toBeGreaterThan(0);
    await settle();
    f.grid.paint();
    expect(f.server.requests.some((r) => r.state === 18)).toBe(true);
  });
});
