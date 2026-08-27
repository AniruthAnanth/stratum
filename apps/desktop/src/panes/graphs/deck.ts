/**
 * The Graph Deck's model — spec §18, `06-ui-architecture.md` §6.7.
 *
 * > Every graph is *also* pushed to the **Graph Deck**, a pane holding the last
 * > 50 graphs as a vertical filmstrip with name, command and timestamp. Deck
 * > actions: `Pin` (survives `graph drop _all` and clear), `Compare` (select 2–4
 * > → side-by-side at matched scale), `Export ▸`, `Open in window` (**opt-in**),
 * > `Collapse`, `Clear`. Nothing spawns a window on its own.
 *
 * Every rule in that paragraph is a pure function here, and the component draws
 * the result. Two reasons, both load-bearing:
 *
 *  * The capacity rule is a **counter** (ADR-017). "The deck holds at most 50
 *    however many graphs the session produced" is a property of `push`, testable
 *    without a DOM, and asserted against 500 pushes rather than against 51.
 *  * Pinning is a *retention* rule, not a decoration: a pinned graph survives
 *    `Clear` and survives being pushed out by newer ones. Retention logic that
 *    lives inside a render function is retention logic nobody can test.
 */

import type { ResultId } from "../../ipc/hand";
import type { AssetRefView, GraphPayloadView, ResultEnvelopeView } from "../../renderers";

/** 06 §6.7: "a pane holding the last 50 graphs". */
export const DECK_CAPACITY = 50;

/** 06 §6.7: "select 2–4 → side-by-side at matched scale". */
export const COMPARE_MIN = 2;
/** See {@link COMPARE_MIN}. */
export const COMPARE_MAX = 4;

export interface DeckItem {
  /**
   * Identity. A result can carry more than one graph payload, so the index is
   * part of the key — without it a `twoway … , by(region)` would collapse to one
   * deck entry and the others would be unreachable.
   */
  readonly key: string;
  readonly result: ResultId;
  readonly name: string;
  readonly cmd: string;
  /** `ResultEnvelope.started_at_ms`. */
  readonly at: number;
  readonly asset: AssetRefView;
  readonly intrinsic_pt: readonly [number, number];
  readonly scheme: string;
  readonly pinned: boolean;
  readonly collapsed: boolean;
}

export interface DeckState {
  /** Newest first — the filmstrip reads top-down and the newest graph is what
   * the user just made. */
  readonly items: readonly DeckItem[];
  /** Keys selected for `Compare`, in the order they were picked. */
  readonly selected: readonly string[];
}

export function emptyDeck(): DeckState {
  return { items: [], selected: [] };
}

/**
 * Pull every graph payload out of one envelope, oldest-first within it.
 *
 * `at` is passed rather than read off the envelope: `ResultEnvelope` carries
 * `started_at_ms` on the wire, but `renderers/types.ts` — W16's file, and the
 * one this pane codes against until W17 generates `ipc/types.ts` — declares only
 * the fields a *renderer* draws, and no card shows a timestamp. Declaring it
 * here would be the hand-written second declaration §12 forbids, so the caller
 * supplies the number it already has.
 */
export function itemsOf(envelope: ResultEnvelopeView, at: number): DeckItem[] {
  const out: DeckItem[] = [];
  envelope.payloads.forEach((payload, i) => {
    if (payload.kind !== "graph") return;
    const graph = payload as GraphPayloadView;
    out.push({
      key: `${String(envelope.result)}:${String(i)}`,
      result: envelope.result,
      name: graph.name,
      cmd: graph.source_cmd,
      at,
      asset: graph.asset,
      intrinsic_pt: graph.intrinsic_pt,
      scheme: graph.scheme,
      pinned: false,
      collapsed: false,
    });
  });
  return out;
}

/**
 * Add a graph to the front.
 *
 * Re-pushing a key that is already present **replaces it in place** rather than
 * adding a second row: a re-rendered graph (`GraphRender` bumps
 * `ResultEnvelope.revision` for the same result) is the same graph, and two rows
 * for it would make `Clear` ambiguous and `Compare` able to compare a figure
 * with itself.
 */
export function push(state: DeckState, item: DeckItem): DeckState {
  const existing = state.items.findIndex((i) => i.key === item.key);
  const rest = existing === -1 ? state.items : state.items.filter((_, i) => i !== existing);
  // A replacement keeps its pin and its collapse: those are the user's, not the
  // engine's, and a re-render must not silently unpin a figure.
  const carried =
    existing === -1
      ? item
      : {
          ...item,
          pinned: state.items[existing]?.pinned ?? false,
          collapsed: state.items[existing]?.collapsed ?? false,
        };
  return prune({ ...state, items: [carried, ...rest] });
}

/**
 * Enforce the capacity.
 *
 * Pinned items are never evicted, and they do not count against the budget for
 * unpinned ones — "Pin survives `graph drop _all` and clear" would be a lie if
 * pinning fifty graphs meant the deck could then hold no new ones.
 */
function prune(state: DeckState): DeckState {
  let unpinned = 0;
  const kept = state.items.filter((item) => {
    if (item.pinned) return true;
    unpinned += 1;
    return unpinned <= DECK_CAPACITY;
  });
  if (kept.length === state.items.length) return state;
  const live = new Set(kept.map((i) => i.key));
  return { items: kept, selected: state.selected.filter((k) => live.has(k)) };
}

export function setPinned(state: DeckState, key: string, pinned: boolean): DeckState {
  return prune({
    ...state,
    items: state.items.map((i) => (i.key === key ? { ...i, pinned } : i)),
  });
}

export function setCollapsed(state: DeckState, key: string, collapsed: boolean): DeckState {
  return {
    ...state,
    items: state.items.map((i) => (i.key === key ? { ...i, collapsed } : i)),
  };
}

/** `Clear` — everything unpinned goes; the selection follows it. */
export function clear(state: DeckState): DeckState {
  const kept = state.items.filter((i) => i.pinned);
  const live = new Set(kept.map((i) => i.key));
  return { items: kept, selected: state.selected.filter((k) => live.has(k)) };
}

/**
 * `graph drop _all`. Identical to `Clear` by design — 06 §6.7 gives pinning the
 * same guarantee against both — and named separately so the call site reads as
 * what happened.
 */
export const dropAll = clear;

/**
 * Toggle a key's membership of the comparison set.
 *
 * Selecting a fifth drops the oldest selection rather than refusing: a user who
 * clicks a fifth graph means "compare this one", and a modal "you may only
 * select four" for a limit the UI could just enforce is the kind of friction
 * §39 is about.
 */
export function toggleSelected(state: DeckState, key: string): DeckState {
  if (state.selected.includes(key)) {
    return { ...state, selected: state.selected.filter((k) => k !== key) };
  }
  if (!state.items.some((i) => i.key === key)) return state;
  const next = [...state.selected, key];
  return { ...state, selected: next.slice(-COMPARE_MAX) };
}

/** The selected items, in deck order, or `[]` when the selection is not 2–4. */
export function comparison(state: DeckState): DeckItem[] {
  if (state.selected.length < COMPARE_MIN) return [];
  const set = new Set(state.selected);
  return state.items.filter((i) => set.has(i.key));
}

/**
 * The common box for a side-by-side comparison: "at matched scale" means every
 * figure is drawn into the same aspect box, so a taller graph is visibly taller
 * rather than being independently normalised into a lie.
 */
export function matchedScale(items: readonly DeckItem[]): readonly [number, number] {
  let w = 0;
  let h = 0;
  for (const item of items) {
    w = Math.max(w, item.intrinsic_pt[0]);
    h = Math.max(h, item.intrinsic_pt[1]);
  }
  return w > 0 && h > 0 ? [w, h] : [4, 3];
}

/** `HH:MM:SS` from a `UnixMs`. */
export function clockTime(at: number): string {
  if (!Number.isFinite(at) || at <= 0) return "";
  const d = new Date(at);
  const pad = (n: number): string => String(n).padStart(2, "0");
  return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}

/**
 * The export formats the deck offers.
 *
 * `06` §6.7 lists "PNG @1×/2×/3×, SVG, PDF, EPS". **EPS is absent here on
 * purpose**: `stratum_proto::GraphFormat` is `{ Svg, Png, Pdf }`, the wire has
 * no spelling for EPS, and `EngineRequest::GraphRender` is the only path to a
 * format the deck does not already hold. Offering a menu item that cannot be
 * satisfied is the exit-10 failure A22 was raised about, so the item is not
 * offered and the gap is reported instead.
 */
export const EXPORT_FORMATS = [
  { id: "svg", label: "SVG", format: "svg", scale: 1 },
  { id: "png1", label: "PNG @1×", format: "png", scale: 1 },
  { id: "png2", label: "PNG @2×", format: "png", scale: 2 },
  { id: "png3", label: "PNG @3×", format: "png", scale: 3 },
  { id: "pdf", label: "PDF", format: "pdf", scale: 1 },
] as const;

export type ExportFormat = (typeof EXPORT_FORMATS)[number];
