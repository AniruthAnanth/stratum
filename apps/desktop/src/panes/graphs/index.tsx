/**
 * The Graph Deck — spec §18, `06-ui-architecture.md` §6.7.
 *
 * > Graphs appear inline or in a reusable viewer by default; **Open in window**
 * > is opt-in. Support pin, compare, detach, export, collapse, clear.
 *
 * **Nothing here opens a window.** `bridge().openPaneWindow` is reached from
 * exactly one `onClick` in this file and from nowhere else, and
 * `graphs.test.tsx` pushes sixty graphs into a mounted deck against a bridge
 * that counts window opens, asserts zero, then clicks the button and asserts
 * one. §18's "stack results, don't spawn windows" is the sentence that made old
 * Stata unusable for anyone with more than three graphs; it deserves an
 * assertion rather than a comment.
 *
 * The figure itself is the SVG `stratum-graph` produced — the same bytes the
 * inline card shows, the same bytes `graph export` writes, fetched over
 * `stratum-asset://` because the payload carries an `AssetRef` and never the
 * bytes (ARCHITECTURE C23). Injected rather than `<img src>`-referenced for the
 * reason `renderers/graph/` gives: every asset request must carry
 * `X-Stratum-Token` and an `<img>` cannot.
 */

import { For, type JSX, Show, createSignal } from "solid-js";
import { render } from "solid-js/web";
import { registerPane } from "../../dock/panes";
import { bridge } from "../../platform/bridge";
import { assetUrl } from "../../renderers";
import type { AssetRefView, ResultEnvelopeView } from "../../renderers";
import { Button, PaneHeader } from "../../ui";
import {
  COMPARE_MAX,
  COMPARE_MIN,
  type DeckItem,
  type DeckState,
  EXPORT_FORMATS,
  type ExportFormat,
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

import "./graphs.css";

// ---------------------------------------------------------------------------
// Thumbnails: fetched once per asset, ever
// ---------------------------------------------------------------------------

/**
 * `AssetRef.path` → the SVG source, or `undefined` while it is in flight.
 *
 * Keyed by path and never invalidated, which is safe because an asset path is
 * `graph/{session}/{result}.svg` and a re-render mints a new `ResultId`. The
 * cache is what makes the counter in the design note true: **pinning,
 * collapsing, selecting for compare and re-rendering add zero fetches**. A
 * `createResource` per row would refetch on every one of those, because each
 * changes the item object and Solid's `<For>` keys by reference.
 */
const THUMBNAILS = new Map<string, () => string | undefined>();
const THUMBNAIL_STATE = new Map<string, "loading" | "ready" | "failed">();

/** Test seam, in the idiom of `renderers/actions.ts`'s `resetRawOutputRepairs`. */
export function resetGraphThumbnails(): void {
  THUMBNAILS.clear();
  THUMBNAIL_STATE.clear();
}

export function thumbnail(asset: AssetRefView): () => string | undefined {
  const cached = THUMBNAILS.get(asset.path);
  if (cached !== undefined) return cached;

  const [svg, setSvg] = createSignal<string | undefined>(undefined);
  THUMBNAILS.set(asset.path, svg);
  THUMBNAIL_STATE.set(asset.path, "loading");

  void (async () => {
    try {
      const response = await bridge().fetchAsset(assetUrl(asset.path));
      const mime = response.headers.get("content-type") ?? asset.mime;
      if (!mime.startsWith("image/svg+xml")) {
        THUMBNAIL_STATE.set(asset.path, "failed");
        return;
      }
      setSvg(await response.text());
      THUMBNAIL_STATE.set(asset.path, "ready");
    } catch {
      // A deck row that could not load is a row with a caption and an empty
      // frame, not a thrown error that takes the pane down with it.
      THUMBNAIL_STATE.set(asset.path, "failed");
    }
  })();

  return svg;
}

// ---------------------------------------------------------------------------
// Opening a window — the ONLY place that does
// ---------------------------------------------------------------------------

/** 06 §6.7: "creates a `graph:<name>` webview (§13)". Opt-in, always. */
export async function openGraphWindow(item: DeckItem): Promise<string> {
  return bridge().openPaneWindow({
    role: "graph",
    paneId: "graphs",
    label: `graph:${item.key}`,
  });
}

// ---------------------------------------------------------------------------
// The pane
// ---------------------------------------------------------------------------

export interface GraphsPaneProps {
  state: DeckState;
  onStateChange: (next: DeckState) => void;
  /**
   * `Export ▸`. The deck does not render anything: formats other than the SVG
   * already in hand come from `EngineRequest::GraphRender { format, width_pt }`
   * (A9/R-2), so the exported figure and the displayed figure are produced by
   * the same Rust renderer. Absent handler ⇒ the menu is not offered, rather
   * than offered and dead.
   */
  onExport?: (item: DeckItem, format: ExportFormat) => void;
  /** Overridable for tests; the default is {@link openGraphWindow}. */
  onOpenWindow?: (item: DeckItem) => void;
}

export function GraphsPane(props: GraphsPaneProps): JSX.Element {
  const update = (next: DeckState): void => props.onStateChange(next);
  const compared = (): DeckItem[] => comparison(props.state);
  const pinnedCount = (): number => props.state.items.filter((i) => i.pinned).length;

  return (
    <section class="deck" data-pane="graphs">
      {/* `Compare` is not a mode you enter. Picking two rows IS the gesture, so
          the header carries the count and the way out of it rather than a
          button that turns a view on — one fewer state for the user to be in,
          which is the §39 direction. */}
      <PaneHeader
        title="Graphs"
        actions={
          <>
            <span class="deck__count t-micro" data-deck-count>
              {`${String(props.state.items.length)} graph${
                props.state.items.length === 1 ? "" : "s"
              } \u00b7 ${String(pinnedCount())} pinned`}
            </span>
            <Button
              variant="quiet"
              disabled={props.state.selected.length === 0}
              data-deck-deselect
              onClick={() => update({ ...props.state, selected: [] })}
            >
              {`Comparing ${String(props.state.selected.length)}/${String(COMPARE_MAX)}`}
            </Button>
            <Button
              variant="quiet"
              disabled={props.state.items.every((i) => i.pinned)}
              data-deck-clear
              onClick={() => update(clear(props.state))}
            >
              Clear
            </Button>
          </>
        }
      />

      <Show when={compared().length >= COMPARE_MIN}>
        <ComparisonStrip items={compared()} />
      </Show>

      <div class="deck__scroll" data-deck-scroll>
        <Show
          when={props.state.items.length > 0}
          fallback={
            <p class="deck__empty">
              No graphs yet. Every graph a command draws lands here as well as under the code.
            </p>
          }
        >
          <For each={props.state.items}>
            {(item) => (
              <DeckRow
                item={item}
                selected={props.state.selected.includes(item.key)}
                onToggleSelect={() => update(toggleSelected(props.state, item.key))}
                onTogglePin={() => update(setPinned(props.state, item.key, !item.pinned))}
                onToggleCollapse={() =>
                  update(setCollapsed(props.state, item.key, !item.collapsed))
                }
                onExport={props.onExport}
                onOpenWindow={props.onOpenWindow ?? ((i) => void openGraphWindow(i))}
              />
            )}
          </For>
        </Show>
      </div>
    </section>
  );
}

interface DeckRowProps {
  item: DeckItem;
  selected: boolean;
  onToggleSelect: () => void;
  onTogglePin: () => void;
  onToggleCollapse: () => void;
  onExport?: (item: DeckItem, format: ExportFormat) => void;
  onOpenWindow: (item: DeckItem) => void;
}

function DeckRow(props: DeckRowProps): JSX.Element {
  const svg = (): string | undefined => thumbnail(props.item.asset)();
  const [w, h] = [props.item.intrinsic_pt[0], props.item.intrinsic_pt[1]];

  return (
    <figure
      class="deck__item"
      data-deck-item={props.item.key}
      data-selected={props.selected ? "" : undefined}
      data-pinned={props.item.pinned ? "" : undefined}
      data-collapsed={props.item.collapsed ? "" : undefined}
    >
      <figcaption class="deck__caption">
        <button
          type="button"
          class="deck__pick"
          aria-pressed={props.selected}
          data-deck-pick
          onClick={props.onToggleSelect}
        >
          <span class="deck__name">{props.item.name}</span>
          <span class="deck__cmd">{props.item.cmd}</span>
        </button>
        <span class="deck__time t-micro" data-deck-time>
          {clockTime(props.item.at)}
        </span>
      </figcaption>

      <Show when={!props.item.collapsed}>
        <div
          class="deck__frame"
          style={{
            "aspect-ratio": `${String(w)} / ${String(h)}`,
          }}
        >
          <Show when={svg()} fallback={<div class="deck__placeholder" data-deck-placeholder />}>
            {(markup) => (
              // Injected, not referenced: the bytes come from our own graphics
              // crate over the authenticated asset scheme, and CSP
              // `script-src 'self'` makes anything in them inert.
              <div
                class="deck__canvas"
                role="img"
                aria-label={props.item.cmd}
                innerHTML={markup()}
              />
            )}
          </Show>
        </div>
      </Show>

      <div class="deck__actions">
        <Button
          variant="quiet"
          aria-pressed={props.item.pinned}
          data-deck-pin
          onClick={props.onTogglePin}
        >
          {props.item.pinned ? "Pinned" : "Pin"}
        </Button>
        <Button
          variant="quiet"
          aria-pressed={props.item.collapsed}
          data-deck-collapse
          onClick={props.onToggleCollapse}
        >
          {props.item.collapsed ? "Expand" : "Collapse"}
        </Button>
        <Show when={props.onExport !== undefined}>
          <For each={EXPORT_FORMATS}>
            {(format) => (
              <Button
                variant="quiet"
                data-deck-export={format.id}
                onClick={() => props.onExport?.(props.item, format)}
              >
                {format.label}
              </Button>
            )}
          </For>
        </Show>
        {/* The ONE window-opening call site in this unit. */}
        <Button
          variant="quiet"
          icon="detach"
          data-deck-open-window
          onClick={() => props.onOpenWindow(props.item)}
        >
          Open in window
        </Button>
      </div>
    </figure>
  );
}

/** 06 §6.7: "select 2–4 → side-by-side at matched scale". */
function ComparisonStrip(props: { items: DeckItem[] }): JSX.Element {
  const box = (): readonly [number, number] => matchedScale(props.items);
  return (
    <div class="deck__compare" data-deck-comparison>
      <For each={props.items}>
        {(item) => (
          <figure class="deck__compare-item" data-deck-compare-item={item.key}>
            <div
              class="deck__frame"
              style={{ "aspect-ratio": `${String(box()[0])} / ${String(box()[1])}` }}
            >
              <Show when={thumbnail(item.asset)()}>
                {(markup) => (
                  <div class="deck__canvas" role="img" aria-label={item.cmd} innerHTML={markup()} />
                )}
              </Show>
            </div>
            <figcaption class="deck__compare-caption t-micro">{item.cmd}</figcaption>
          </figure>
        )}
      </For>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/**
 * The deck's state, owned outside the component so the engine's event loop can
 * push into it without the pane being mounted — a graph drawn while the Graphs
 * pane is closed is still in the deck when it is opened, which is what "every
 * graph is *also* pushed to the deck" means.
 */
export interface GraphDeck {
  state: () => DeckState;
  set: (next: DeckState) => void;
  /** Push every graph payload in an envelope. `at` is its `started_at_ms`. */
  ingest: (envelope: ResultEnvelopeView, at: number) => void;
  /** `graph drop _all` — pinned figures survive (06 §6.7). */
  dropAll: () => void;
}

export function createGraphDeck(): GraphDeck {
  const [state, setState] = createSignal<DeckState>(emptyDeck());
  return {
    state,
    set: (next) => setState(next),
    ingest: (envelope, at) => {
      for (const item of itemsOf(envelope, at)) {
        setState((s) => push(s, item));
      }
    },
    dropAll: () => setState(dropAll),
  };
}

/** Mount the deck into the dock. Returns the unregister. */
export function registerGraphsPane(
  deck: GraphDeck,
  handlers: Pick<GraphsPaneProps, "onExport" | "onOpenWindow"> = {},
): () => void {
  return registerPane("graphs", (host, register) => {
    const dispose = render(
      () => (
        <GraphsPane
          state={deck.state()}
          onStateChange={deck.set}
          onExport={handlers.onExport}
          onOpenWindow={handlers.onOpenWindow}
        />
      ),
      host,
    );
    register(dispose);
  });
}
