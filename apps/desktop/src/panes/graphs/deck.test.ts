/**
 * The Graph Deck's rules — spec §18, 06 §6.7.
 *
 * The bullet under test: "Graphs appear inline **and** in the Graph Deck".
 * Everything here is the deck half's retention and selection logic, asserted as
 * a **counter** wherever the rule is about size (ADR-017): "holds the last 50"
 * is checked against 500 pushes, because a rule stated as a bound has to be
 * tested past the bound.
 */

import { describe, expect, test } from "vitest";
import type { ResultId } from "../../ipc/hand";
import { envelopeOf, payloadOfEveryKind } from "../../renderers/fixtures";
import {
  COMPARE_MAX,
  DECK_CAPACITY,
  type DeckItem,
  clear,
  clockTime,
  comparison,
  dropAll,
  emptyDeck,
  itemsOf,
  matchedScale,
  push,
  setCollapsed,
  setPinned,
  toggleSelected,
} from "./deck";

function item(n: number, over: Partial<DeckItem> = {}): DeckItem {
  return {
    key: `${String(n)}:0`,
    result: n as unknown as ResultId,
    name: `Graph${String(n)}`,
    cmd: `histogram v${String(n)}`,
    at: 1_700_000_000_000 + n,
    asset: { path: `graph/1/${String(n)}.svg`, mime: "image/svg+xml", bytes: 1024 },
    intrinsic_pt: [396, 288],
    scheme: "stratum",
    pinned: false,
    collapsed: false,
    ...over,
  };
}

function deckOf(n: number) {
  let state = emptyDeck();
  for (let i = 0; i < n; i += 1) state = push(state, item(i));
  return state;
}

describe("extraction", () => {
  test("a graph payload becomes a deck item; other payloads do not", () => {
    const kinds = payloadOfEveryKind();
    expect(itemsOf(envelopeOf(kinds.graph), 1_700_000_000_000)).toHaveLength(1);
    for (const kind of ["log", "summarize", "estimation", "table", "error"] as const) {
      expect(itemsOf(envelopeOf(kinds[kind]), 0)).toHaveLength(0);
    }
  });

  test("the item carries what the filmstrip shows", () => {
    const [only] = itemsOf(envelopeOf(payloadOfEveryKind().graph), 1_700_000_000_000);
    expect(only?.name).toBe("Graph");
    expect(only?.cmd).toBe("histogram price");
    expect(only?.asset.path).toBe("graph/1/41.svg");
    expect(only?.intrinsic_pt).toEqual([400, 300]);
    expect(only?.scheme).toBe("stratum");
  });

  test("two graphs in one envelope get distinct keys", () => {
    const graph = payloadOfEveryKind().graph;
    const envelope = { ...envelopeOf(graph), payloads: [graph, graph] };
    const keys = itemsOf(envelope, 0).map((i) => i.key);
    expect(new Set(keys).size).toBe(2);
  });
});

describe("capacity (the counter)", () => {
  test("five hundred graphs leave fifty rows", () => {
    const state = deckOf(500);
    expect(state.items).toHaveLength(DECK_CAPACITY);
    // Newest first: the last one pushed is the first one shown.
    expect(state.items[0]?.key).toBe("499:0");
  });

  test("a pinned graph is never evicted, however many arrive after it", () => {
    let state = push(emptyDeck(), item(0));
    state = setPinned(state, "0:0", true);
    for (let i = 1; i <= 500; i += 1) state = push(state, item(i));
    expect(state.items.some((i) => i.key === "0:0")).toBe(true);
    // ...and pinning does not eat the budget for unpinned ones.
    expect(state.items.filter((i) => !i.pinned)).toHaveLength(DECK_CAPACITY);
  });

  test("re-pushing the same result replaces the row instead of adding one", () => {
    let state = push(emptyDeck(), item(7));
    state = setPinned(state, "7:0", true);
    state = push(state, item(7, { cmd: "histogram v7, bin(20)" }));
    expect(state.items).toHaveLength(1);
    expect(state.items[0]?.cmd).toBe("histogram v7, bin(20)");
    // The pin is the user's, not the engine's: a re-render must not drop it.
    expect(state.items[0]?.pinned).toBe(true);
  });
});

describe("pin, clear and graph drop _all", () => {
  test("clear keeps exactly the pinned rows", () => {
    let state = deckOf(10);
    state = setPinned(state, "3:0", true);
    state = setPinned(state, "7:0", true);
    const kept = clear(state);
    expect(kept.items.map((i) => i.key)).toEqual(["7:0", "3:0"]);
  });

  test("graph drop _all obeys the same pin guarantee", () => {
    let state = deckOf(5);
    state = setPinned(state, "2:0", true);
    expect(dropAll(state).items.map((i) => i.key)).toEqual(["2:0"]);
  });

  test("clearing drops the selection of everything it removed", () => {
    let state = deckOf(4);
    state = setPinned(state, "1:0", true);
    state = toggleSelected(state, "1:0");
    state = toggleSelected(state, "2:0");
    const kept = clear(state);
    expect(kept.selected).toEqual(["1:0"]);
  });

  test("collapse is per row and changes nothing else", () => {
    const state = setCollapsed(deckOf(3), "1:0", true);
    expect(state.items.filter((i) => i.collapsed).map((i) => i.key)).toEqual(["1:0"]);
  });
});

describe("compare (06 §6.7: select 2-4)", () => {
  test("one selection is not a comparison", () => {
    const state = toggleSelected(deckOf(4), "1:0");
    expect(comparison(state)).toHaveLength(0);
  });

  test("two to four are, in deck order rather than click order", () => {
    let state = deckOf(4);
    state = toggleSelected(state, "1:0");
    state = toggleSelected(state, "3:0");
    expect(comparison(state).map((i) => i.key)).toEqual(["3:0", "1:0"]);
  });

  test("a fifth pick drops the oldest rather than refusing", () => {
    let state = deckOf(6);
    for (const key of ["0:0", "1:0", "2:0", "3:0", "4:0"]) {
      state = toggleSelected(state, key);
    }
    expect(state.selected).toHaveLength(COMPARE_MAX);
    expect(state.selected).not.toContain("0:0");
  });

  test("picking a selected row deselects it", () => {
    let state = toggleSelected(deckOf(3), "1:0");
    state = toggleSelected(state, "1:0");
    expect(state.selected).toEqual([]);
  });

  test("a key that is not in the deck cannot be selected", () => {
    expect(toggleSelected(deckOf(2), "99:0").selected).toEqual([]);
  });

  test("matched scale is the largest box, not each figure normalised alone", () => {
    const items = [item(1, { intrinsic_pt: [396, 288] }), item(2, { intrinsic_pt: [500, 200] })];
    expect(matchedScale(items)).toEqual([500, 288]);
    expect(matchedScale([])).toEqual([4, 3]);
  });
});

describe("timestamps", () => {
  test("a real time renders as a clock and a missing one renders as nothing", () => {
    expect(clockTime(0)).toBe("");
    expect(clockTime(Number.NaN)).toBe("");
    expect(clockTime(new Date(2026, 7, 22, 9, 5, 3).getTime())).toBe("09:05:03");
  });
});
