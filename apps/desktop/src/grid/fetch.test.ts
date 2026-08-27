/**
 * Paging — the A13 amendment, asserted rather than described.
 *
 * The plan's bullets for this file:
 *
 *  * "Pages are fetched from `stratum-asset://localhost/frame/…` with an
 *    `AbortController`, and a superseded fetch is cancelled; there is **no**
 *    `data_page` command (A13)."
 *  * "A test asserts no request payload from this pane ever exceeds 4 KB, at any
 *    dataset size."
 *
 * The second one is the reason `PageRequest.order` is a `u32` and not a
 * permutation: the pre-audit shape put 80 MB of JSON in front of a 12 ms budget
 * on every 40-row scroll of a sorted 10 M-row view. `maxRequestBytes` is
 * recorded inside `request()` for every request rather than for the ones a test
 * remembered to look at, so the assertion below covers the paths this file does
 * not walk as well as the ones it does.
 *
 * The server is `harness.frameServer`, which is checked against
 * `auto_40x12.bin` byte for byte in `harness.test.ts` before anything here runs.
 */

import { readFileSync, readdirSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { beforeEach, describe, expect, test } from "vitest";
import { type DatasetStateId, asDatasetStateId, asSessionId } from "../ipc/hand";
import {
  AUTO_VARS,
  autoDisplay,
  encodeDisplayPage,
  frameServer,
  settle,
} from "../panes/dataeditor/harness";
import { type GridColumn, columnsFromVariables, counters, resetGridCounters } from "./engine";
import { COLUMN_BAND, MAX_REQUEST_BYTES, PageSource } from "./fetch";

const SESSION = asSessionId(1);
const STATE = asDatasetStateId(17);
const COLUMNS = columnsFromVariables(AUTO_VARS);

function sourceOver(
  server: ReturnType<typeof frameServer>,
  columns: readonly GridColumn[] = COLUMNS,
  extra: { pageRows?: number; onStateAdvanced?: (s: DatasetStateId) => void } = {},
): PageSource {
  const source = new PageSource({
    session: SESSION,
    frame: "default",
    fetchAsset: server.fetchAsset,
    ...extra,
  });
  source.retarget({ state: STATE });
  source.setColumns(columns);
  return source;
}

/** `auto_40x12.bin`'s own text for the cell the synthetic frame cycles onto `row`. */
function oracle(idx: number, row: number): string {
  const col = autoDisplay.column(idx);
  if (col === undefined || col.kind !== "text") throw new Error(`column ${idx} is not text`);
  return col.cell(row % autoDisplay.nrows);
}

beforeEach(() => {
  resetGridCounters();
});

describe("one transport, and it is the asset scheme (A13)", () => {
  test("every page is a GET of stratum-asset://localhost/frame/… and no command at all", async () => {
    const server = frameServer({ rows: 10_000_000 });
    const source = sourceOver(server);
    source.ensure(9_000_000, 22, 0, 11, 1, 10_000_000);
    await server.flush();

    expect(server.requests.length).toBeGreaterThan(0);
    for (const request of server.requests) {
      expect(request.url.startsWith("stratum-asset://localhost/frame/1/default/page?")).toBe(true);
      expect(request.render).toBe("display");
      expect(request.state).toBe(17);
    }
    // The scroll path issues no Tauri command. `data_page` was deleted from
    // CONTRACTS §11 precisely so that this counter could be zero.
    expect(counters.ipcInvocations).toBe(0);
    expect(counters.pageRequests).toBe(server.requests.length);
    expect(counters.pagesDecoded).toBeGreaterThan(0);
  });

  test("this unit invokes exactly three commands, and `data_page` is not one", () => {
    const here = dirname(fileURLToPath(import.meta.url));
    const files = [
      ...readdirSync(here)
        .filter((f) => f.endsWith(".ts") && !f.endsWith(".test.ts"))
        .map((f) => join(here, f)),
      ...readdirSync(resolve(here, "../panes/dataeditor"))
        .filter((f) => (f.endsWith(".ts") || f.endsWith(".tsx")) && !f.includes(".test."))
        .map((f) => join(here, "../panes/dataeditor", f)),
    ];
    expect(files.length).toBeGreaterThan(10);

    const invoked = new Set<string>();
    for (const file of files) {
      const text = readFileSync(file, "utf8");
      for (const match of text.matchAll(/\.invoke(?:<[^>]*>)?\(\s*"([a-z_]+)"/g)) {
        const name = match[1];
        if (name !== undefined) invoked.add(name);
      }
    }
    // Sorting and filtering are a declaration, an edit is a command submission,
    // and pages are not a command at all. `data_page` was deleted from
    // CONTRACTS §11 and nothing here resurrected it.
    expect([...invoked].sort()).toEqual(["data_order_drop", "data_order_set", "exec_submit"]);
  });

  test("a landed page serves the oracle's own strings", async () => {
    const server = frameServer({ rows: 10_000_000 });
    const source = sourceOver(server);
    source.ensure(9_999_960, 22, 0, 11, 1, 10_000_000);
    await server.flush();

    const make = COLUMNS[0];
    const price = COLUMNS[1];
    const rep78 = COLUMNS[3];
    if (make === undefined || price === undefined || rep78 === undefined) {
      throw new Error("auto.dta's columns did not load");
    }
    expect(source.cell(9_999_960, make)).toBe(oracle(0, 9_999_960));
    expect(source.cell(9_999_960, make)).toBe("AMC Concord");
    // `%8.0gc`'s comma survives the wire; the grid never re-formats it.
    expect(source.cell(9_999_961, price)).toBe(oracle(1, 9_999_961));
    // README §3: `rep78` is missing at observations 3 and 7 of every cycle.
    expect(source.cell(9_999_962, rep78)).toBe(".");
  });
});

describe("no request payload exceeds 4 KB, at any dataset size", () => {
  test("10 M rows × 32 767 variables, sorted, at the far end of both axes", async () => {
    const wide = columnsFromVariables(
      Array.from({ length: 32_767 }, (_, i) => ({
        idx: i,
        name: `variable_with_a_long_name_${i}`,
        storage: "int",
        format: "%8.0g",
      })),
    );
    const server = frameServer({ rows: 10_000_000 });
    const source = sourceOver(server, wide);
    // An `OrderId` is seven characters of query string. The permutation it
    // replaced would have been 80 MB.
    source.retarget({ order: 4_294_967_295 });

    for (const col0 of [0, 16_000, 32_700]) {
      for (const row0 of [0, 5_000_000, 9_999_900]) {
        source.ensure(row0, 22, col0, 60, 1, 10_000_000);
      }
    }
    await server.flush();

    expect(server.requests.length).toBeGreaterThan(0);
    for (const request of server.requests) {
      expect(new TextEncoder().encode(request.url).byteLength).toBeLessThanOrEqual(
        MAX_REQUEST_BYTES,
      );
      expect(request.order).toBe(4_294_967_295);
      // Columns travel as a bounded band list, never as "all of them".
      expect(request.cols.length).toBeLessThanOrEqual(3 * COLUMN_BAND);
    }
    expect(counters.maxRequestBytes).toBeGreaterThan(0);
    expect(counters.maxRequestBytes).toBeLessThanOrEqual(MAX_REQUEST_BYTES);
  });

  test("the recorded maximum is the largest URL actually sent", async () => {
    const server = frameServer({ rows: 10_000_000 });
    const source = sourceOver(server);
    source.ensure(0, 22, 0, 11, 1, 10_000_000);
    await server.flush();
    const longest = Math.max(
      ...server.requests.map((r) => new TextEncoder().encode(r.url).byteLength),
    );
    expect(counters.maxRequestBytes).toBe(longest);
  });
});

describe("a superseded fetch is cancelled", () => {
  test("scrolling away aborts the pages that are no longer wanted", async () => {
    const server = frameServer({ rows: 10_000_000, mode: "manual" });
    const source = sourceOver(server);

    source.ensure(0, 22, 0, 11, 1, 10_000_000);
    const openedFirst = source.inflightPages;
    expect(openedFirst).toBeGreaterThan(0);

    // The user throws the scrollbar to the far end. Those answers can no longer
    // be shown, and holding the connections open for them is what A13 calls the
    // superseded fetch.
    source.ensure(9_000_000, 22, 0, 11, 1, 10_000_000);
    expect(counters.pageAborts).toBeGreaterThanOrEqual(openedFirst);
    await server.flush();
    expect(server.aborts()).toBeGreaterThanOrEqual(openedFirst);

    // Nothing from the abandoned window landed.
    const make = COLUMNS[0];
    if (make === undefined) throw new Error("make did not load");
    expect(source.cell(0, make)).toBeUndefined();
    expect(source.cell(9_000_000, make)).toBe(oracle(0, 9_000_000));
  });

  test("moving the column band aborts too: those bytes are for columns nobody will draw", () => {
    const wide = columnsFromVariables(
      Array.from({ length: 1000 }, (_, i) => ({
        idx: i,
        name: `v${i}`,
        storage: "int",
        format: "%8.0g",
      })),
    );
    const server = frameServer({ rows: 100_000, mode: "manual" });
    const source = sourceOver(server, wide);
    source.ensure(0, 22, 0, 11, 1, 100_000);
    const before = counters.pageAborts;
    source.ensure(0, 22, 900, 11, 1, 100_000);
    expect(counters.pageAborts).toBeGreaterThan(before);
  });

  test("a retarget aborts everything under the old prefix", () => {
    const server = frameServer({ rows: 100_000, mode: "manual" });
    const source = sourceOver(server);
    source.ensure(0, 22, 0, 11, 1, 100_000);
    const open = source.inflightPages;
    source.retarget({ order: 7 });
    expect(counters.pageAborts).toBe(open);
    expect(source.inflightPages).toBe(0);
  });
});

describe("a response the UI cannot use is dropped, not shown", () => {
  test("a page whose state has advanced invalidates instead of painting", async () => {
    const seen: DatasetStateId[] = [];
    // A server that answers with a state the UI is not showing — CONTRACTS §8.1:
    // "If the frame has advanced, the response's `state` differs and the UI
    // invalidates."
    const advanced = (url: string): Promise<Response> => {
      const query = new URLSearchParams(url.slice(url.indexOf("?") + 1));
      const bytes = encodeDisplayPage({
        state: 18,
        row0: Number(query.get("row0")),
        seq: Number(query.get("seq")),
        nrows: Number(query.get("nrows")),
        columns: [{ idx: 0, cells: Array.from({ length: Number(query.get("nrows")) }, () => "x") }],
      });
      return Promise.resolve(new Response(bytes, { status: 200 }));
    };
    const source = new PageSource({
      session: SESSION,
      frame: "default",
      fetchAsset: advanced,
      onStateAdvanced: (state) => seen.push(state),
    });
    source.retarget({ state: STATE });
    source.setColumns(COLUMNS);

    source.ensure(0, 22, 0, 11, 1, 1000);
    await settle();

    expect(seen).toContain(18);
    expect(counters.staleResponses).toBeGreaterThan(0);
    const make = COLUMNS[0];
    if (make === undefined) throw new Error("make did not load");
    // The stale bytes were not stored: the grid would rather show `⋯` than a
    // value from a dataset that no longer exists.
    expect(source.cell(0, make)).toBeUndefined();
  });
});

describe("residency", () => {
  test("a page already resident is a cache hit, not a second request", async () => {
    const server = frameServer({ rows: 10_000_000 });
    const source = sourceOver(server);
    source.ensure(0, 22, 0, 11, 1, 10_000_000);
    await server.flush();
    const issued = counters.pageRequests;
    expect(source.residentPages).toBeGreaterThan(0);

    source.ensure(0, 22, 0, 11, 1, 10_000_000);
    await server.flush();
    expect(counters.pageRequests).toBe(issued);
    expect(counters.pageCacheHits).toBeGreaterThan(0);
    expect(source.isResident(0, 22)).toBe(true);
  });

  test("prefetch runs in the direction of travel only", async () => {
    const server = frameServer({ rows: 10_000_000 });
    const source = sourceOver(server, COLUMNS, { pageRows: 200 });
    source.ensure(1_000_000, 22, 0, 11, 1, 10_000_000);
    await server.flush();
    const rows = server.requests.map((r) => r.row0).sort((a, b) => a - b);
    // A user scrolling down has no use for the three viewports above them.
    expect(Math.min(...rows)).toBe(1_000_000);
    expect(Math.max(...rows)).toBeGreaterThan(1_000_000);
  });

  test("the resident set is bounded: an LRU, not a leak", async () => {
    const server = frameServer({ rows: 10_000_000 });
    const source = new PageSource({
      session: SESSION,
      frame: "default",
      fetchAsset: server.fetchAsset,
      maxResidentPages: 4,
    });
    source.retarget({ state: STATE });
    source.setColumns(COLUMNS);
    for (let i = 0; i < 40; i++) {
      source.ensure(i * 5000, 22, 0, 11, 1, 10_000_000);
      await server.flush();
    }
    expect(source.residentPages).toBeLessThanOrEqual(4);
  });
});
