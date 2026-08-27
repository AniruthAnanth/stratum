/**
 * The block gutter — 06 §4.5, spec §12.
 *
 * 18 px, left of the line numbers, one marker on a block's FIRST line only.
 *
 * # Glyphs are SVG, never characters
 *
 * `○ ✓ ◌ ▶ ✕` are the spec's illustration, not its implementation. Those five
 * characters render at different sizes and on different baselines on macOS,
 * Windows and Linux, so a gutter built from them jitters horizontally every time
 * a block changes state — the one place in the product where a 1 px shift is
 * unmissable, because forty of them are stacked in a column. Every marker here
 * is a 14x14 inline SVG on the same grid, the same 1.25 px square-capped stroke,
 * as the rest of the icon set, cloned from W12's `ui/StateGlyph.tsx` so the
 * geometry has exactly one definition.
 *
 * # What a keystroke costs
 *
 * The marker set is memoised on `(generation, viewport, status revision)` and
 * the SAME `RangeSet` object is returned when none of those moved, which is what
 * makes CodeMirror skip the gutter entirely for a selection change. When it does
 * rebuild, it walks the blocks in the VIEWPORT — `blocksTouching` is
 * O(log n + k) — never the document. `counters.gutterRebuilds` and
 * `counters.gutterMarkersConstructed` hold both claims in
 * `editor.perf.test.ts`.
 */

import { RangeSet, StateEffect } from "@codemirror/state";
import { Decoration, EditorView, GutterMarker, ViewPlugin, gutter } from "@codemirror/view";
import type { DecorationSet, ViewUpdate } from "@codemirror/view";
import type { BlockStatusState } from "../../ipc/hand";
import { stateLabel } from "../../ui";
import { anchorForBlock } from "../results/anchor";
import { displayStatus, glyphNode } from "../results/widget";
import { allBlocks, blockAtLine, blocksTouching, segGeneration } from "./blockField";
import { submitRun } from "./run";
import type { Block } from "./segmenter";
import { counters } from "./segmenter";

/**
 * Dispatched when the kernel's opinion of any block changed.
 *
 * A no-op transaction with no changes and no selection — it exists so the gutter
 * and the card flags recompute. It is NOT a document write and it must never
 * grow one.
 */
export const statusChanged = StateEffect.define<null>();

/** Bumped by every `statusChanged`, so the marker memo can compare one number. */
let statusRevision = 0;

/** Opens the per-block context menu. W16 owns the menu; this is the seam. */
export type BlockMenuOpener = (view: EditorView, block: Block, event: MouseEvent) => void;

let openBlockMenu: BlockMenuOpener | null = null;

export function setBlockMenuOpener(opener: BlockMenuOpener | null): void {
  openBlockMenu = opener;
}

// ---------------------------------------------------------------------------
// Markers
// ---------------------------------------------------------------------------

/**
 * One marker per (state, hovered) pair, constructed at most twice per state for
 * the life of the window.
 *
 * A marker holds no per-block data — the block key goes on the DOM node, which
 * `toDOM` writes — so eighteen distinct instances cover a document of any size,
 * and `GutterMarker.eq` then lets CodeMirror reuse the DOM across scrolls.
 */
class BlockMarker extends GutterMarker {
  constructor(
    readonly state: BlockStatusState,
    readonly index: number,
    readonly runnable: boolean,
  ) {
    super();
    counters.gutterMarkersConstructed += 1;
  }

  override eq(other: GutterMarker): boolean {
    return (
      other instanceof BlockMarker &&
      other.state === this.state &&
      other.index === this.index &&
      other.runnable === this.runnable
    );
  }

  override toDOM(): HTMLElement {
    const wrap = document.createElement("span");
    wrap.className = "cm-blockMark";
    // Not a `BlockId`: identity is allocated by the engine and arrives in a
    // `BlockMap`. The region index is stable for the life of one segmentation,
    // which is exactly as long as this DOM node lives.
    wrap.dataset["block"] = String(this.index);
    wrap.setAttribute("role", "button");
    wrap.setAttribute("aria-label", `${stateLabel(this.state)} — run this block`);
    wrap.tabIndex = -1;

    const glyph = glyphNode(this.state);
    glyph.classList.add("glyph-state");
    wrap.append(glyph);

    if (this.runnable) {
      // The hover affordance. Present in the DOM at all times and revealed by
      // CSS on `[data-hover]`, so pointing at a block costs an attribute write
      // and never a render (§4.5).
      const run = glyphNode("running");
      run.classList.add("glyph-run");
      run.setAttribute("aria-hidden", "true");
      wrap.append(run);
    }
    return wrap;
  }
}

interface MarkerMemo {
  gen: number;
  from: number;
  to: number;
  revision: number;
  set: RangeSet<GutterMarker>;
}

const memos = new WeakMap<EditorView, MarkerMemo>();

function buildMarkers(view: EditorView): RangeSet<GutterMarker> {
  const state = view.state;
  const gen = segGeneration(state);
  const { from, to } = view.viewport;
  const memo = memos.get(view);
  if (
    memo !== undefined &&
    memo.gen === gen &&
    memo.from === from &&
    memo.to === to &&
    memo.revision === statusRevision
  ) {
    // The same object, so CodeMirror's own `markers != prevMarkers` check short
    // circuits and the gutter does no DOM work at all.
    return memo.set;
  }

  counters.gutterRebuilds += 1;
  const marks: { pos: number; marker: GutterMarker }[] = [];
  for (const block of blocksTouching(state, from, to)) {
    const rec = anchorForBlock(state, block);
    const status = displayStatus(rec, block);
    if (!block.executable && status === "never_run") continue;
    // The head line, not the outer start: a block with three lines of attached
    // comment above it gets its glyph beside the code, which is what the eye
    // connects to the card below.
    const at = state.doc.lineAt(Math.min(block.from, state.doc.length)).from;
    marks.push({ pos: at, marker: new BlockMarker(status, block.index, block.executable) });
  }
  const set = RangeSet.of(
    marks.map((m) => m.marker.range(m.pos)),
    true,
  );
  memos.set(view, { gen, from, to, revision: statusRevision, set });
  return set;
}

/** A spacer of the widest marker, so the gutter never changes width (§4.5). */
const spacer = new BlockMarker("never_run", -1, true);

export function blockGutter(): ReturnType<typeof gutter> {
  return gutter({
    class: "cm-blockGutter",
    markers: buildMarkers,
    initialSpacer: () => spacer,
    domEventHandlers: {
      mousedown(view, line, event) {
        const block = blockAtLine(view.state, view.state.doc.lineAt(line.from).number);
        if (block === null || !block.executable) return false;
        const mouse = event as MouseEvent;
        if (mouse.altKey) {
          // Run from the CLICKED block, not from the caret. The gutter names its
          // own target; borrowing the cursor's would run a different block from
          // the one under the pointer.
          const from = allBlocks(view.state).filter((b) => b.index >= block.index && b.executable);
          submitRun(view, "run.fromHere", { blocks: from });
        } else if (mouse.shiftKey) submitRun(view, "run.blockAndAdvance", { blocks: [block] });
        else submitRun(view, "run.block", { blocks: [block] });
        return true;
      },
      contextmenu(view, line, event) {
        const block = blockAtLine(view.state, view.state.doc.lineAt(line.from).number);
        if (block === null || openBlockMenu === null) return false;
        openBlockMenu(view, block, event as MouseEvent);
        return true;
      },
    },
  });
}

/** Bump the status revision and make the gutter recompute. No document write. */
export function notifyStatusChanged(view: EditorView): void {
  statusRevision += 1;
  view.dispatch({ effects: statusChanged.of(null) });
}

// ---------------------------------------------------------------------------
// The running hairline
// ---------------------------------------------------------------------------

const RUNNING_LINE = Decoration.line({ class: "cm-runningBlock" });

/**
 * A 1 px accent hairline down the left content edge of every line of a running
 * block. **No spinner anywhere in the product** (§4.5) — a spinner is the "web
 * app" tell, and a hairline says both "running" and "this much of the file".
 *
 * Viewport-scoped, and skipped entirely when nothing is running, which is almost
 * always.
 */
export const runningLines = ViewPlugin.fromClass(
  class {
    decorations: DecorationSet = Decoration.none;

    constructor(readonly view: EditorView) {
      this.decorations = build(view);
    }

    update(update: ViewUpdate): void {
      // A caret move cannot start or stop a run, and rebuilding here would cost
      // one region lookup per visible block for every arrow key. Effects are the
      // only other thing that can change what is running.
      const effectful = update.transactions.some((tr) => tr.effects.length > 0);
      if (!update.docChanged && !update.viewportChanged && !effectful) return;
      this.decorations = build(update.view);
    }
  },
  { decorations: (plugin) => plugin.decorations },
);

function build(view: EditorView): DecorationSet {
  const { from, to } = view.viewport;
  const marks: { pos: number; deco: Decoration }[] = [];
  for (const block of blocksTouching(view.state, from, to)) {
    const rec = anchorForBlock(view.state, block);
    if (rec === null || rec.kernel.state !== "running") continue;
    const first = view.state.doc.lineAt(block.from).number;
    const last = view.state.doc.lineAt(Math.min(block.to, view.state.doc.length)).number;
    for (let line = first; line <= last; line++) {
      marks.push({ pos: view.state.doc.line(line).from, deco: RUNNING_LINE });
    }
  }
  return Decoration.set(
    marks.map((m) => m.deco.range(m.pos)),
    true,
  );
}

// ---------------------------------------------------------------------------
// Style — 06 §4.5. Colours come from the generated tokens; none are declared.
// ---------------------------------------------------------------------------

export const blockGutterTheme = EditorView.baseTheme({
  ".cm-blockGutter": {
    width: "var(--w-gutter, 18px)",
    minWidth: "var(--w-gutter, 18px)",
    display: "flex",
    flexDirection: "column",
    alignItems: "center",
  },
  ".cm-blockMark": {
    display: "block",
    position: "relative",
    width: "14px",
    height: "14px",
    cursor: "pointer",
  },
  ".cm-blockMark .glyph-run": { display: "none", color: "var(--accent)" },
  // The hover swap is entirely declarative: `hover.ts` writes one attribute and
  // the browser does the rest, so pointing at a block dispatches no state and
  // renders nothing.
  ".cm-blockMark[data-hover] .glyph-state": { display: "none" },
  ".cm-blockMark[data-hover] .glyph-run": { display: "block" },
  ".cm-blockMark[data-hover]": { background: "var(--surface)" },
  ".cm-runningBlock": {
    boxShadow: "inset 1px 0 0 0 var(--accent)",
  },
});
