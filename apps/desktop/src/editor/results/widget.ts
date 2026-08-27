/**
 * The inline result card — 06 §4.6 card anatomy, §4.7 re-runs, spec §4/§17.
 *
 * # What this file is and is not
 *
 * It is the card *shell*: the state rail, the glyph, the echoed command, the
 * `E41 · D17 · 0.08s` readout, the streaming log region and the action row. The
 * BODY — the coefficient table, the summarize table, the graph — belongs to W14
 * and arrives through {@link setCardBodyRenderer}. Until it does, the shell
 * renders the classic text it was given, which is not a placeholder: 06 §6.1
 * says the classic output always exists, so the degraded card is a real card.
 *
 * # No document writes
 *
 * Not one `view.dispatch({ changes })` in this file. Everything a card can do to
 * itself — collapse, expand, show raw, remember its height — is a `StateEffect`
 * or a sidecar write. `ignoreEvent()` returns true so a click inside a card
 * never moves the caret, which is also why a card cannot accidentally become an
 * editing surface.
 *
 * # Streaming does not resize the card
 *
 * 06 §4.6, and it is the whole reason scroll position survives a long `bootstrap`:
 * while a block is running its log lives in a FIXED-height internal scroller
 * pinned to its own bottom, so appending 4 000 lines changes the card's height by
 * exactly zero pixels and CodeMirror's height map never moves. On completion the
 * card expands ONCE, and that single change goes through `scrollAnchor.ts`.
 * `appendStreamingLog` writes straight to that scroller — no transaction, no
 * decoration rebuild, no widget construction.
 */

import { type EditorView, WidgetType } from "@codemirror/view";
import { createRoot } from "solid-js";
import type { BlockStatusState, HasBlockState, InlineResultsMode } from "../../ipc/hand";
import { worseOf } from "../../ipc/hand";
import { StateGlyph, stateLabel } from "../../ui";
import type { Block } from "../blocks/segmenter";
import { counters } from "../blocks/segmenter";
import type { ExecRecord } from "./anchor";

/**
 * `displayed = worseOf(local, kernel)` — CONTRACTS §3, ARCHITECTURE C20.
 *
 * The local verdict comes from the block's CURRENT hash against the hash the run
 * was submitted with: two string reads, so a block goes stale in the frame the
 * character was typed, with zero IPC and no wait for the kernel. That immediacy
 * is the whole of spec §12. `worseOf` is imported rather than reimplemented —
 * a second copy of the total order is how a UI starts disagreeing with itself.
 *
 * It lives in this file rather than in `anchor.ts` only to keep the module graph
 * acyclic: `anchor.ts` constructs widgets, so it may import this, and this may
 * not import it back at run time.
 */
export function displayStatus(
  rec: Pick<ExecRecord, "kernel" | "executedHash"> | null,
  block: Block | null,
): BlockStatusState {
  if (rec === null) return "never_run";
  if (block !== null && block.hashKey !== rec.executedHash) {
    return worseOf<HasBlockState>({ state: "stale" }, rec.kernel).state;
  }
  return rec.kernel.state;
}

// ---------------------------------------------------------------------------
// Glyphs — 14x14 inline SVG, never Unicode
// ---------------------------------------------------------------------------

/**
 * A cloneable `StateGlyph` per state.
 *
 * The glyph shapes belong to W12's `ui/StateGlyph.tsx` and are not copied here:
 * `○ ✓ ◌ ▶ ✕` typed as characters render at different sizes and baselines on
 * macOS, Windows and Linux and would make the gutter jitter as the status
 * changed, which is exactly what 06 §4.5 forbids. Rendering the component once
 * per state and cloning the node keeps one source of geometry and costs nine
 * Solid renders for the life of the window instead of one per marker per scroll.
 *
 * The roots are deliberately never disposed: there are at most nine, they hold
 * no subscriptions, and a cache that can be evicted would re-render on scroll.
 */
const glyphTemplates = new Map<BlockStatusState, Element>();

export function glyphNode(state: BlockStatusState): Element {
  let template = glyphTemplates.get(state);
  if (template === undefined) {
    template = createRoot(() => StateGlyph({ state }) as unknown as Element);
    glyphTemplates.set(state, template);
  }
  return template.cloneNode(true) as Element;
}

// ---------------------------------------------------------------------------
// The renderer seam (W14)
// ---------------------------------------------------------------------------

/** What W14 is handed to fill a card body with. */
export interface CardBodyContext {
  readonly rec: ExecRecord;
  readonly mode: InlineResultsMode;
  readonly view: EditorView;
}

/** Fills the body element. Returns true if it took ownership of the node. */
export type CardBodyRenderer = (body: HTMLElement, ctx: CardBodyContext) => boolean;

let bodyRenderer: CardBodyRenderer | null = null;

/** W14 installs the typed renderers here. One per window is enough. */
export function setCardBodyRenderer(renderer: CardBodyRenderer | null): void {
  bodyRenderer = renderer;
}

/** How many log lines the streaming scroller shows. 06 §4.6's default. */
export const STREAM_ROWS = 12;
/** Natural height cap after completion, in lines, before internal scrolling. */
export const SETTLED_ROWS = 40;

// ---------------------------------------------------------------------------
// The widget
// ---------------------------------------------------------------------------

export class ResultWidget extends WidgetType {
  constructor(
    readonly rec: ExecRecord,
    readonly mode: InlineResultsMode,
  ) {
    super();
    counters.cardWidgetsConstructed += 1;
  }

  /**
   * Controls DOM reuse. `ExecRecord` is replaced wholesale on every change, so
   * identity is both the cheapest and the most exact test available: two widgets
   * are equal precisely when nothing a card renders has moved.
   */
  override eq(other: WidgetType): boolean {
    return other instanceof ResultWidget && other.rec === this.rec && other.mode === this.mode;
  }

  /**
   * The height CodeMirror should reserve BEFORE the card is measured.
   *
   * Getting this close is what stops the scrollbar correcting itself as you
   * scroll past cards that have never been on screen — the Q2 failure mode. A
   * remembered measurement wins; otherwise the estimate is the streaming
   * scroller's fixed height, which is what the card is about to be.
   */
  override get estimatedHeight(): number {
    const remembered = this.rec.ui.measuredHeight;
    if (remembered !== undefined) return remembered;
    if (this.mode === "compact") return COMPACT_HEIGHT;
    return CARD_CHROME_PX + (this.rec.streaming ? STREAM_ROWS : SETTLED_ROWS / 4) * LINE_PX;
  }

  override toDOM(view: EditorView): HTMLElement {
    counters.cardDomMounts += 1;
    const root = document.createElement("div");
    root.className = "cm-resultCard";
    root.tabIndex = 0;
    root.dataset["anchor"] = String(this.rec.id);
    buildCard(root, this.rec, this.mode, view);
    return root;
  }

  override updateDOM(dom: HTMLElement, view: EditorView): boolean {
    counters.cardDomPatches += 1;
    buildCard(dom, this.rec, this.mode, view);
    return true;
  }

  /** Clicks inside a card never move the caret, and never edit the document. */
  override ignoreEvent(): boolean {
    return true;
  }

  override coordsAt(): null {
    return null;
  }
}

/** Card chrome (header + rules + action row) in px, for the height estimate. */
const CARD_CHROME_PX = 58;
/** One line of classic output. `--lh-code` is 20px in the generated tokens. */
const LINE_PX = 20;
/** 06 §4.6: `Compact` collapses every card to one 22 px line. */
const COMPACT_HEIGHT = 22;

/**
 * Build or patch a card in place.
 *
 * One function for both `toDOM` and `updateDOM` because a card that is built one
 * way and patched another is a card whose two paths drift; a re-run replacing a
 * card's contents (§4.7) has to produce exactly what a fresh mount would.
 */
function buildCard(
  root: HTMLElement,
  rec: ExecRecord,
  mode: InlineResultsMode,
  view: EditorView,
): void {
  root.dataset["state"] = rec.kernel.state;
  root.dataset["mode"] = mode;
  root.replaceChildren();

  const rail = document.createElement("div");
  rail.className = "cm-cardRail";
  root.append(rail);

  const inner = document.createElement("div");
  inner.className = "cm-cardInner";
  root.append(inner);

  // --- header --------------------------------------------------------------
  const header = document.createElement("div");
  header.className = "cm-cardHeader";
  header.append(glyphNode(rec.kernel.state));

  const echo = document.createElement("span");
  echo.className = "cm-cardEcho";
  echo.textContent = rec.label;
  echo.title = rec.label;
  header.append(echo);

  const readout = document.createElement("span");
  readout.className = "cm-cardReadout";
  readout.textContent = stateReadout(rec);
  header.append(readout);
  inner.append(header);

  if (mode === "compact" || rec.ui.collapsed) {
    // 06 §4.6: one 22 px line. The action row still carries `Raw ▸` (§17), so
    // compact is a smaller card and never a card with fewer affordances.
    inner.append(actionRow(rec, view));
    return;
  }

  // --- body ----------------------------------------------------------------
  const body = document.createElement("div");
  body.className = "cm-cardBody";
  if (rec.streaming) {
    body.classList.add("cm-cardStreaming");
    body.style.height = `${STREAM_ROWS * LINE_PX}px`;
  } else {
    body.style.maxHeight = `${SETTLED_ROWS * LINE_PX}px`;
  }
  const took = bodyRenderer?.(body, { rec, mode, view }) === true;
  if (!took) {
    // The classic text always exists (§6.1), so the shell has something honest
    // to show before W14's typed renderers land.
    const pre = document.createElement("pre");
    pre.className = "cm-cardRaw";
    pre.textContent = "";
    body.append(pre);
  }
  inner.append(body);
  inner.append(actionRow(rec, view));

  if (rec.streaming) pinToBottom(body);
}

/** `E41 · D17 · 0.08s` — spec §13's execution/state id, in 11 px mono. */
function stateReadout(rec: ExecRecord): string {
  const parts: string[] = [];
  if (rec.exec !== undefined) parts.push(`E${rec.exec}`);
  if (rec.dataset !== undefined) parts.push(`D${rec.dataset}`);
  if (rec.durationMs !== undefined) parts.push(`${(rec.durationMs / 1000).toFixed(2)}s`);
  return parts.length === 0 ? stateLabel(rec.kernel.state) : parts.join(" · ");
}

/**
 * The action row. **`Raw ▸` is present on every single card, always** (§17).
 *
 * The rest of the row is W14's — 06's A22 makes `ResultEnvelope.actions` data
 * computed in Rust, so the shell must not invent labels. `Raw ▸` is the one
 * exception because §17 promises it unconditionally and it needs no build
 * knowledge to be true.
 */
function actionRow(rec: ExecRecord, view: EditorView): HTMLElement {
  const row = document.createElement("div");
  row.className = "cm-cardActions";

  const raw = document.createElement("button");
  raw.type = "button";
  raw.className = "cm-cardAction";
  raw.dataset["action"] = "raw";
  raw.dataset["anchor"] = String(rec.id);
  raw.textContent = "Raw ▸";
  row.append(raw);

  // No handler wiring beyond the data attributes: `setup.ts` delegates from the
  // editor root, so 500 cards cost one listener rather than 500.
  void view;
  return row;
}

function pinToBottom(el: HTMLElement): void {
  el.scrollTop = el.scrollHeight;
}

// ---------------------------------------------------------------------------
// The two DOM-only paths
// ---------------------------------------------------------------------------

/**
 * Append streamed output to a running card.
 *
 * No transaction, no decoration rebuild, no widget: the scroller has a fixed
 * height, so appending cannot change the card's height and CodeMirror's height
 * map cannot move. This is what makes a 40-second `bootstrap` cost nothing on
 * the document layout.
 */
export function appendStreamingLog(view: EditorView, anchorId: number, text: string): boolean {
  const body = view.dom.querySelector<HTMLElement>(
    `.cm-resultCard[data-anchor="${anchorId}"] .cm-cardStreaming`,
  );
  if (body === null) return false;
  const pinned = body.scrollTop + body.clientHeight >= body.scrollHeight - 2;
  body.append(document.createTextNode(text));
  if (pinned) pinToBottom(body);
  counters.cardStreamAppends += 1;
  return true;
}

/**
 * Push the current display status onto the cards that are on screen.
 *
 * The hover trick from 06 §4.5, applied to staleness: an edit changes a block's
 * hash, the card must go stale *this frame*, and rebuilding the widget to do it
 * would cost a decoration rebuild per keystroke. Instead this writes one
 * attribute per card whose state actually changed — bounded by the viewport, not
 * by the document, and skipped entirely when nothing moved.
 */
export function applyCardStateFlags(
  view: EditorView,
  cards: readonly { readonly id: number; readonly state: BlockStatusState }[],
): void {
  for (const card of cards) {
    const el = view.dom.querySelector<HTMLElement>(`.cm-resultCard[data-anchor="${card.id}"]`);
    if (el === null || el.dataset["display"] === card.state) continue;
    el.dataset["display"] = card.state;
    counters.cardStateWrites += 1;
  }
}
