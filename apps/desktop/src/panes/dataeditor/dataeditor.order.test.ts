/**
 * Sorting and filtering — A13, from the client side.
 *
 * The plan: "**Sorting and filtering issue `data_order_set` and then scroll
 * against an `OrderId`** (A13). A test asserts no request payload from this pane
 * ever exceeds 4 KB, at any dataset size."
 *
 * The pre-audit `PageRequest.order: Option<Vec<u64>>` was "a permutation of
 * observation indices" — 80 MB of JSON per 40-row fetch on a sorted 10 M-row
 * view, from the one sender that cannot compute a permutation, while 06 §15.3
 * simultaneously required sorting to happen in Rust. This file is the counter
 * form of that amendment: the declaration is bounded, the handle is a `u32`, and
 * `counters.maxRequestBytes` covers both halves.
 *
 * `grid/fetch.test.ts` asserts the same ceiling for the page URLs.
 */

import { afterEach, beforeEach, describe, expect, test } from "vitest";
import { counters, resetGridCounters } from "../../grid/engine";
import { MAX_FILTER_BYTES, MAX_REQUEST_BYTES } from "../../grid/fetch";
import { asDatasetStateId, asSessionId } from "../../ipc/hand";
import { detachedBridge, setBridge } from "../../platform/bridge";
import {
  MAX_SORT_KEYS,
  type SortKey,
  cycleSortKey,
  dropOrder,
  filterLabel,
  orderLabel,
  setOrder,
} from "./order";

const SESSION = asSessionId(1);
const STATE = asDatasetStateId(17);

interface Call {
  command: string;
  args: Record<string, unknown>;
}

let calls: Call[] = [];

beforeEach(() => {
  resetGridCounters();
  calls = [];
  setBridge(
    detachedBridge({
      invoke: <T>(command: string, args?: Record<string, unknown>): Promise<T> => {
        calls.push({ command, args: args ?? {} });
        return Promise.resolve({ order: 7, nRows: 4_212_998, state: 17 } as T);
      },
    }),
  );
});

afterEach(() => {
  setBridge(undefined);
});

const keys = (n: number): SortKey[] =>
  Array.from({ length: n }, (_, i) => ({
    idx: 30_000 + i,
    name: `variable_with_a_long_name_${i}`,
    dir: i % 2 === 0 ? ("asc" as const) : ("desc" as const),
  }));

describe("one declaration, then a handle", () => {
  test("a sort is `data_order_set` with an OrderSpec, and nothing else", async () => {
    const outcome = await setOrder(
      SESSION,
      "default",
      [{ idx: 11, name: "foreign", dir: "asc" }],
      undefined,
      STATE,
    );

    expect(outcome).toEqual({ ok: true, result: { order: 7, nRows: 4_212_998, state: 17 } });
    expect(calls).toHaveLength(1);
    expect(calls[0]?.command).toBe("data_order_set");
    expect(calls[0]?.args).toEqual({
      session: SESSION,
      frame: "default",
      // CONTRACTS §7's `OrderSpec`: keys as `[VarIdx, dir]` pairs, and `null`
      // rather than `""` for "no filter" — an empty string is a filter that
      // matches nothing.
      spec: { keys: [[11, "asc"]], filter: null, state: 17 },
    });
    expect(counters.orderRequests).toBe(1);
    expect(counters.ipcInvocations).toBe(1);
  });

  test("a filter travels as text, once, and not per page", async () => {
    await setOrder(SESSION, "default", [], "foreign == 1 & price > 5000", STATE);
    expect(calls[0]?.args.spec).toEqual({
      keys: [],
      filter: "foreign == 1 & price > 5000",
      state: 17,
    });
  });

  test("dropping a handle is scoped to the session", async () => {
    await dropOrder(SESSION, 7);
    expect(calls).toEqual([{ command: "data_order_drop", args: { session: SESSION, order: 7 } }]);
    expect(counters.orderRequests).toBe(1);
  });
});

describe("no request payload exceeds 4 KB, at any dataset size", () => {
  test("eight sort keys over 32 767 variables plus a 2 KB filter, on 10 M rows", async () => {
    // The largest declaration the pane can express: the key cap, the longest
    // variable indices in the file, and a filter at exactly its own ceiling.
    const filter = `price > ${"9".repeat(MAX_FILTER_BYTES - 9)}`;
    expect(new TextEncoder().encode(filter).byteLength).toBeLessThanOrEqual(MAX_FILTER_BYTES);

    const outcome = await setOrder(SESSION, "default", keys(MAX_SORT_KEYS), filter, STATE);
    expect(outcome.ok).toBe(true);
    expect(counters.maxRequestBytes).toBeGreaterThan(MAX_FILTER_BYTES);
    expect(counters.maxRequestBytes).toBeLessThanOrEqual(MAX_REQUEST_BYTES);

    // The row count the engine answers with is the POST-FILTER one, which is
    // what the grid scrolls against; the permutation stays in Rust.
    expect(outcome.ok === true ? outcome.result.nRows : 0).toBe(4_212_998);
  });

  test("a filter longer than its ceiling is refused, not truncated", async () => {
    const outcome = await setOrder(SESSION, "default", [], "x".repeat(MAX_FILTER_BYTES + 1), STATE);
    expect(outcome.ok).toBe(false);
    // Truncating would show the wrong observations, which is worse than refusing.
    expect(outcome.ok === false ? outcome.reason : "").toContain("keep if");
    expect(calls).toEqual([]);
    expect(counters.ipcInvocations).toBe(0);
  });

  test("more sort keys than the header affordance can express are refused", async () => {
    const outcome = await setOrder(SESSION, "default", keys(MAX_SORT_KEYS + 1), undefined, STATE);
    expect(outcome.ok).toBe(false);
    expect(calls).toEqual([]);
  });

  test("a multi-byte filter is measured in bytes, not in characters", async () => {
    // 1 200 three-byte characters is 3 600 bytes: under the character count and
    // over the byte ceiling. UTF-16 `.length` would let it through.
    const outcome = await setOrder(SESSION, "default", [], "名".repeat(1200), STATE);
    expect(outcome.ok).toBe(false);
    expect(calls).toEqual([]);
  });
});

describe("Stata's own Order: and Filter: fields", () => {
  test("orderLabel is `Dataset`, a varlist, or gsort's minus notation", () => {
    expect(orderLabel([])).toBe("Dataset");
    expect(orderLabel([{ idx: 0, name: "make", dir: "asc" }])).toBe("make");
    expect(orderLabel([{ idx: 1, name: "price", dir: "desc" }])).toBe("-price");
    expect(
      orderLabel([
        { idx: 11, name: "foreign", dir: "asc" },
        { idx: 1, name: "price", dir: "desc" },
      ]),
    ).toBe("foreign -price");
  });

  test("filterLabel is Stata's On/Off and not the expression", () => {
    expect(filterLabel(undefined)).toBe("Off");
    expect(filterLabel("")).toBe("Off");
    expect(filterLabel("price > 5000")).toBe("On");
  });

  test("a header click cycles ascending, descending, unsorted", () => {
    const price = { idx: 1, name: "price" };
    const first = cycleSortKey([], price);
    expect(first).toEqual([{ idx: 1, name: "price", dir: "asc" }]);
    const second = cycleSortKey(first, price);
    expect(second).toEqual([{ idx: 1, name: "price", dir: "desc" }]);
    expect(cycleSortKey(second, price)).toEqual([]);
    // Clicking a different header starts that column over, as Stata's does.
    expect(cycleSortKey(second, { idx: 11, name: "foreign" })).toEqual([
      { idx: 11, name: "foreign", dir: "asc" },
    ]);
  });
});
