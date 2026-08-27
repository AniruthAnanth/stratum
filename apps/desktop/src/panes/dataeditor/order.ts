/**
 * Sorting and filtering — the A13 amendment, from the client side.
 *
 * The rule, from CONTRACTS §8.1's own comment on `PageRequest.order`: "The
 * frontend now declares intent (`OrderSpec`) once and scrolls against a `u32`."
 * Before the audit this field was `Option<Vec<u64>>`, "a permutation of
 * observation indices" — which the Data Editor is the only sender of, cannot
 * compute (`06` §15.3 requires sorting to happen in Rust), and which would have
 * put 80 MB of JSON in front of a 12 ms budget on every 40-row scroll of a
 * sorted 10 M-row view.
 *
 * So: one `data_order_set` per sort or filter change, answered with an
 * `OrderId(u32)` and the post-filter row count. Every page fetch afterwards
 * carries seven characters of query string. `counters.maxRequestBytes` records
 * both halves and `dataeditor.order.test.ts` asserts the 4 KB ceiling holds at
 * 10 M rows with a full sort key list and a filter.
 *
 * **Editing is refused while an order is live, and that is a reported contract
 * gap, not a design choice.** `replace … in n` counts OBSERVATIONS; a view row
 * under an `OrderId` is a position in a permutation the client is — correctly —
 * never shown. Nothing on the wire maps one to the other: `PageRequest` carries
 * `row0` in view space, the SDP1 header echoes it back in view space, and
 * `EngineResponse::DataOrder` returns only `{ order, n_rows, state }`. The
 * gutter has the same problem: Stata's Data Editor shows the observation number
 * under a sort, and we can only show the view position. Escalated in this
 * unit's return; until it is answered, a sorted or filtered view is Browse.
 */

import { counters } from "../../grid/engine";
import { MAX_FILTER_BYTES, MAX_REQUEST_BYTES } from "../../grid/fetch";
import type { DatasetStateId, SessionId } from "../../ipc/hand";
import { bridge } from "../../platform/bridge";

export type SortDir = "asc" | "desc";

export interface SortKey {
  /** `VarIdx`, the storage index — what `OrderSpec.keys` is written in. */
  idx: number;
  name: string;
  dir: SortDir;
}

/**
 * More keys than this and the sort is not something a person is reading; Stata's
 * own `sort` accepts more, but the Data Editor's header-click affordance cannot
 * express them and a bounded list is what keeps the request bounded.
 */
export const MAX_SORT_KEYS = 8;

/** `OrderSpec` exactly as CONTRACTS §7 declares it. */
export interface OrderSpecWire {
  keys: [number, SortDir][];
  filter: string | null;
  state: number;
}

export interface OrderResult {
  order: number;
  nRows: number;
  state: DatasetStateId;
}

export type OrderOutcome = { ok: true; result: OrderResult } | { ok: false; reason: string };

/** Stata's own `Order:` field: `Dataset`, or the sort varlist. */
export function orderLabel(keys: readonly SortKey[]): string {
  if (keys.length === 0) return "Dataset";
  // `-name` for descending is `gsort`'s notation, which is the one a Stata user
  // already reads as "descending".
  return keys.map((k) => (k.dir === "desc" ? `-${k.name}` : k.name)).join(" ");
}

/** Stata's `Filter:` field. */
export function filterLabel(filter: string | undefined): string {
  return filter === undefined || filter === "" ? "Off" : "On";
}

/** Toggles a column between ascending, descending and unsorted, Stata-style. */
export function cycleSortKey(
  keys: readonly SortKey[],
  column: { idx: number; name: string },
): SortKey[] {
  const existing = keys.find((k) => k.idx === column.idx);
  if (existing === undefined) return [{ idx: column.idx, name: column.name, dir: "asc" }];
  if (existing.dir === "asc") return [{ idx: column.idx, name: column.name, dir: "desc" }];
  return [];
}

const utf8 = new TextEncoder();

function record(args: unknown): number {
  const bytes = utf8.encode(JSON.stringify(args)).byteLength;
  counters.maxRequestBytes = Math.max(counters.maxRequestBytes, bytes);
  return bytes;
}

/**
 * Establishes an engine-side view order.
 *
 * Returns `{ ok: false }` rather than throwing for a rejected filter, because
 * "your `if` expression is too long" and "the engine is gone" are different
 * things to a user and the pane shows them differently.
 */
export async function setOrder(
  session: SessionId,
  frame: string,
  keys: readonly SortKey[],
  filter: string | undefined,
  state: DatasetStateId,
): Promise<OrderOutcome> {
  if (keys.length > MAX_SORT_KEYS) {
    return { ok: false, reason: `At most ${MAX_SORT_KEYS} sort keys.` };
  }
  if (filter !== undefined && utf8.encode(filter).byteLength > MAX_FILTER_BYTES) {
    return {
      ok: false,
      reason: `The filter expression is longer than ${MAX_FILTER_BYTES} bytes. Put it in a \`keep if\` in your do-file instead — that is reproducible, and this is not.`,
    };
  }

  const spec: OrderSpecWire = {
    keys: keys.map((k) => [k.idx, k.dir]),
    filter: filter === undefined || filter === "" ? null : filter,
    state,
  };
  const args = { session, frame, spec };
  const bytes = record(args);
  if (bytes > MAX_REQUEST_BYTES) {
    // Unreachable given the two bounds above; asserted anyway, because the whole
    // point of A13 is that this pane cannot emit a large request by accident.
    return {
      ok: false,
      reason: `order request is ${bytes} bytes, over the ${MAX_REQUEST_BYTES} limit`,
    };
  }

  counters.orderRequests += 1;
  counters.ipcInvocations += 1;
  const result = await bridge().invoke<OrderResult>("data_order_set", args);
  return { ok: true, result };
}

/** Frees an order handle. Scoped to one session (CONTRACTS §1, `OrderId`). */
export async function dropOrder(session: SessionId, order: number): Promise<void> {
  const args = { session, order };
  record(args);
  counters.orderRequests += 1;
  counters.ipcInvocations += 1;
  await bridge().invoke<void>("data_order_drop", args);
}
