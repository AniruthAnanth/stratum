/**
 * The Graph Deck, mounted — spec §18, 06 §6.7.
 *
 * The acceptance bullet this file exists for:
 *
 * > Graphs appear inline **and** in the Graph Deck; `Open in window` is
 * > **opt-in**. Nothing spawns a window on its own (§18).
 *
 * "Nothing spawns a window on its own" is asserted against a bridge that counts
 * `openPaneWindow`, after sixty graphs have been pushed into a mounted deck,
 * pinned, collapsed and selected for comparison. Then the button is clicked and
 * the count is one. That is the difference between the promise and the comment.
 *
 * The fetch counter is the other half: a deck row fetches its bytes **once**,
 * and pinning, collapsing, selecting and re-rendering add zero fetches
 * (ADR-017 — a counter, not a duration).
 */

import { createSignal } from "solid-js";
import { render } from "solid-js/web";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import type { ResultId } from "../../ipc/hand";
import { detachedBridge, setBridge } from "../../platform/bridge";
import { envelopeOf, payloadOfEveryKind } from "../../renderers/fixtures";
import { type DeckItem, type DeckState, emptyDeck, push, setPinned } from "./deck";
import { GraphsPane, createGraphDeck, openGraphWindow, resetGraphThumbnails } from "./index";

const SVG = '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 4 3"><title>t</title></svg>';

const roots: (() => void)[] = [];
let opened: string[] = [];
let fetched: string[] = [];

function svgResponse(): Response {
  return new Response(SVG, { headers: { "content-type": "image/svg+xml" } });
}

beforeEach(() => {
  opened = [];
  fetched = [];
  resetGraphThumbnails();
  setBridge(
    detachedBridge({
      fetchAsset: (url) => {
        fetched.push(url);
        return Promise.resolve(svgResponse());
      },
      openPaneWindow: (options) => {
        opened.push(options.label);
        return Promise.resolve(options.label);
      },
    }),
  );
});

afterEach(() => {
  while (roots.length > 0) roots.pop()?.();
  document.body.replaceChildren();
  setBridge(undefined);
});

function item(n: number, over: Partial<DeckItem> = {}): DeckItem {
  return {
    key: `${String(n)}:0`,
    result: n as unknown as ResultId,
    name: `Graph${String(n)}`,
    cmd: `histogram v${String(n)}`,
    at: new Date(2026, 7, 22, 9, 5, 3).getTime(),
    asset: { path: `graph/1/${String(n)}.svg`, mime: "image/svg+xml", bytes: 1024 },
    intrinsic_pt: [396, 288],
    scheme: "stratum",
    pinned: false,
    collapsed: false,
    ...over,
  };
}

function mountDeck(initial: DeckState): {
  host: HTMLElement;
  state: () => DeckState;
  set: (s: DeckState) => void;
} {
  const [state, set] = createSignal(initial);
  const host = document.createElement("div");
  document.body.append(host);
  roots.push(render(() => <GraphsPane state={state()} onStateChange={set} />, host));
  return { host, state, set };
}

const flush = (): Promise<void> => new Promise((r) => setTimeout(r, 0));

function deckOf(n: number): DeckState {
  let state = emptyDeck();
  for (let i = 0; i < n; i += 1) state = push(state, item(i));
  return state;
}

// ---------------------------------------------------------------------------
// §18 — nothing spawns a window on its own
// ---------------------------------------------------------------------------

describe("windows are opt-in (spec §18)", () => {
  test("sixty graphs, pinned, collapsed and compared, open no windows", async () => {
    const deck = mountDeck(deckOf(60));
    await flush();

    // Everything a user can do to a deck row, short of the one button.
    deck.set(setPinned(deck.state(), "59:0", true));
    for (const key of ["59:0", "58:0"]) {
      deck.host
        .querySelector<HTMLButtonElement>(`[data-deck-item="${key}"] [data-deck-pick]`)
        ?.click();
    }
    deck.host
      .querySelector<HTMLButtonElement>('[data-deck-item="57:0"] [data-deck-collapse]')
      ?.click();
    await flush();

    expect(opened).toEqual([]);
  });

  test("the button, and only the button, opens one", async () => {
    const deck = mountDeck(deckOf(3));
    await flush();
    expect(opened).toEqual([]);

    deck.host
      .querySelector<HTMLButtonElement>('[data-deck-item="2:0"] [data-deck-open-window]')
      ?.click();
    await flush();

    expect(opened).toEqual(["graph:2:0"]);
  });

  test("the window it opens is a graph role bound to this pane", async () => {
    await openGraphWindow(item(9));
    expect(opened).toEqual(["graph:9:0"]);
  });

  test("a host-supplied handler replaces the bridge call entirely", async () => {
    const onOpenWindow = vi.fn();
    const [state] = createSignal(deckOf(1));
    const host = document.createElement("div");
    document.body.append(host);
    roots.push(
      render(
        () => <GraphsPane state={state()} onStateChange={() => {}} onOpenWindow={onOpenWindow} />,
        host,
      ),
    );
    host.querySelector<HTMLButtonElement>("[data-deck-open-window]")?.click();
    await flush();
    expect(onOpenWindow).toHaveBeenCalledTimes(1);
    expect(opened).toEqual([]);
  });
});

// ---------------------------------------------------------------------------
// the filmstrip
// ---------------------------------------------------------------------------

describe("the filmstrip", () => {
  test("shows name, command and timestamp for each row", async () => {
    const deck = mountDeck(deckOf(2));
    await flush();
    const row = deck.host.querySelector('[data-deck-item="1:0"]');
    expect(row?.textContent).toContain("Graph1");
    expect(row?.textContent).toContain("histogram v1");
    expect(row?.querySelector("[data-deck-time]")?.textContent).toBe("09:05:03");
  });

  test("renders at most the deck capacity however many arrived", async () => {
    const deck = mountDeck(deckOf(500));
    await flush();
    // The counter: the DOM never grows past the cap, so scrolling a long
    // session's deck is not O(graphs ever drawn).
    expect(deck.host.querySelectorAll("[data-deck-item]")).toHaveLength(50);
  });

  test("the empty state says what the deck is for, and is not an apology", async () => {
    const deck = mountDeck(emptyDeck());
    await flush();
    expect(deck.host.querySelector("[data-deck-item]")).toBeNull();
    expect(deck.host.textContent).toContain("No graphs yet");
  });

  test("the figure is the SVG the engine drew, injected over the asset scheme", async () => {
    const deck = mountDeck(deckOf(1));
    await flush();
    const canvas = deck.host.querySelector('[data-deck-item="0:0"] .deck__canvas');
    expect(canvas?.querySelector("svg")).not.toBeNull();
    expect(canvas?.getAttribute("aria-label")).toBe("histogram v0");
    expect(fetched[0]).toBe("stratum-asset://localhost/graph/1/0.svg");
  });

  test("a collapsed row keeps its caption and drops its figure", async () => {
    const deck = mountDeck(deckOf(1));
    await flush();
    deck.host.querySelector<HTMLButtonElement>("[data-deck-collapse]")?.click();
    await flush();
    expect(deck.host.querySelector(".deck__canvas")).toBeNull();
    expect(deck.host.textContent).toContain("histogram v0");
  });
});

// ---------------------------------------------------------------------------
// the fetch counter (ADR-017)
// ---------------------------------------------------------------------------

describe("each graph's bytes are fetched exactly once", () => {
  test("pin, collapse, select and re-render add no fetches", async () => {
    const deck = mountDeck(deckOf(3));
    await flush();
    expect(fetched).toHaveLength(3);

    deck.host.querySelector<HTMLButtonElement>("[data-deck-pin]")?.click();
    deck.host.querySelector<HTMLButtonElement>("[data-deck-pick]")?.click();
    deck.host.querySelector<HTMLButtonElement>('[data-deck-item="1:0"] [data-deck-pick]')?.click();
    deck.set({ ...deck.state() });
    await flush();

    expect(fetched).toHaveLength(3);
  });

  test("the comparison strip reuses the rows' bytes rather than refetching", async () => {
    const deck = mountDeck(deckOf(3));
    await flush();
    const before = fetched.length;

    deck.host.querySelector<HTMLButtonElement>('[data-deck-item="2:0"] [data-deck-pick]')?.click();
    deck.host.querySelector<HTMLButtonElement>('[data-deck-item="1:0"] [data-deck-pick]')?.click();
    await flush();

    expect(deck.host.querySelectorAll("[data-deck-compare-item]")).toHaveLength(2);
    expect(fetched).toHaveLength(before);
  });

  test("a failed fetch leaves a placeholder, not a broken pane", async () => {
    setBridge(
      detachedBridge({
        fetchAsset: () => Promise.reject(new Error("no host")),
        openPaneWindow: (o) => Promise.resolve(o.label),
      }),
    );
    resetGraphThumbnails();
    const deck = mountDeck(deckOf(1));
    await flush();
    expect(deck.host.querySelector("[data-deck-placeholder]")).not.toBeNull();
    expect(deck.host.textContent).toContain("histogram v0");
  });
});

// ---------------------------------------------------------------------------
// export
// ---------------------------------------------------------------------------

describe("export", () => {
  test("the deck reports the request and renders nothing itself", async () => {
    const onExport = vi.fn();
    const [state] = createSignal(deckOf(1));
    const host = document.createElement("div");
    document.body.append(host);
    roots.push(
      render(
        () => <GraphsPane state={state()} onStateChange={() => {}} onExport={onExport} />,
        host,
      ),
    );
    await flush();

    host.querySelector<HTMLButtonElement>('[data-deck-export="png2"]')?.click();
    expect(onExport).toHaveBeenCalledTimes(1);
    const [, format] = onExport.mock.calls[0] as [DeckItem, { format: string; scale: number }];
    expect(format.format).toBe("png");
    expect(format.scale).toBe(2);
  });

  test("with no handler the menu is absent rather than dead", async () => {
    const deck = mountDeck(deckOf(1));
    await flush();
    expect(deck.host.querySelector("[data-deck-export]")).toBeNull();
  });
});

// ---------------------------------------------------------------------------
// ingestion
// ---------------------------------------------------------------------------

describe("createGraphDeck", () => {
  test("ingests the graph payloads of an envelope and ignores the rest", () => {
    const deck = createGraphDeck();
    const kinds = payloadOfEveryKind();
    deck.ingest(envelopeOf(kinds.estimation), 0);
    expect(deck.state().items).toHaveLength(0);
    deck.ingest(envelopeOf(kinds.graph), 1_700_000_000_000);
    expect(deck.state().items).toHaveLength(1);
    expect(deck.state().items[0]?.cmd).toBe("histogram price");
  });

  test("graph drop _all keeps the pinned figures", () => {
    const deck = createGraphDeck();
    deck.ingest(envelopeOf(payloadOfEveryKind().graph), 0);
    const key = deck.state().items[0]?.key ?? "";
    deck.set(setPinned(deck.state(), key, true));
    deck.dropAll();
    expect(deck.state().items).toHaveLength(1);
  });
});
