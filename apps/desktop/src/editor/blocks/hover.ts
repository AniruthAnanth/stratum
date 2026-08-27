/**
 * Gutter hover — 06 §4.5.
 *
 * **Hover tracking dispatches no state.** A `pointermove` fires tens of times a
 * second while the pointer is anywhere over the editor; turning each one into a
 * CodeMirror transaction would re-run every state field, every gutter and every
 * decoration provider in the configuration for a mouse that moved three pixels
 * inside the block it was already in.
 *
 * So this plugin keeps the hovered block in a plain field and communicates by
 * writing ONE attribute on cached DOM. The CSS in `blockGutter.ts` swaps the
 * status glyph for the run triangle declaratively. The cost is therefore
 * proportional to the number of block boundaries the pointer crosses — about one
 * attribute pair per block — and not to the number of pixels it travels.
 * `counters.hoverAttributeWrites` is asserted against a 400-event drag in
 * `editor.perf.test.ts`.
 */

import { ViewPlugin } from "@codemirror/view";
import type { EditorView, ViewUpdate } from "@codemirror/view";
import { blockAt } from "./blockField";
import { counters } from "./segmenter";

const hovered = new WeakMap<EditorView, { index: number | null }>();

/** Which block index the pointer is over, or `null`. */
export function hoveredBlock(view: EditorView): number | null {
  return hovered.get(view)?.index ?? null;
}

export const blockHoverPlugin = ViewPlugin.fromClass(
  class {
    private index: number | null = null;

    constructor(readonly view: EditorView) {
      hovered.set(view, { index: null });
      view.dom.addEventListener("pointermove", this.move);
      view.dom.addEventListener("pointerleave", this.leave);
    }

    /**
     * Re-apply after the gutter re-rendered.
     *
     * Scrolling rebuilds the marker DOM, which does not carry `data-hover`; the
     * pointer has not moved, so nothing else would restore it and the run
     * affordance would vanish under a stationary cursor. One `querySelector` per
     * update that actually changed the gutter, and only when a block is hovered
     * at all.
     */
    update(update: ViewUpdate): void {
      if (this.index === null) return;
      if (!update.docChanged && !update.viewportChanged) return;
      this.paint(this.index, true);
    }

    destroy(): void {
      this.view.dom.removeEventListener("pointermove", this.move);
      this.view.dom.removeEventListener("pointerleave", this.leave);
      hovered.delete(this.view);
    }

    private readonly move = (event: PointerEvent): void => {
      const pos = this.posAt(event);
      const block = pos === null ? null : blockAt(this.view.state, pos);
      const next = block === null || !block.executable ? null : block.index;
      // The early return is the entire optimisation, and it is why this is a
      // pointer handler rather than a transaction: the common case is that the
      // pointer moved within the block it was already in, and the common case
      // does nothing at all.
      if (next === this.index) return;
      this.set(next);
    };

    private readonly leave = (): void => {
      this.set(null);
    };

    private set(next: number | null): void {
      this.clear();
      this.index = next;
      hovered.set(this.view, { index: next });
      if (next !== null) this.paint(next, false);
    }

    /**
     * `posAtCoords` against a document that has no layout.
     *
     * CodeMirror reads client rectangles to answer this, and an environment that
     * reports every rectangle as zero — jsdom, a hidden pane, a window being
     * restored — can make it throw from inside the scan. A pointer handler that
     * throws does so on every mouse move, so this fails to "no block" instead,
     * which is exactly what a hover over nothing means.
     */
    private posAt(event: PointerEvent): number | null {
      try {
        return this.view.posAtCoords({ x: event.clientX, y: event.clientY }, false);
      } catch {
        return null;
      }
    }

    private clear(): void {
      const previous = this.view.dom.querySelector<HTMLElement>(".cm-blockMark[data-hover]");
      if (previous === null) return;
      previous.removeAttribute("data-hover");
      counters.hoverAttributeWrites += 1;
    }

    private paint(index: number, ifMissing: boolean): void {
      const el = this.view.dom.querySelector<HTMLElement>(`.cm-blockMark[data-block="${index}"]`);
      if (el === null) return;
      if (ifMissing && el.hasAttribute("data-hover")) return;
      el.setAttribute("data-hover", "1");
      counters.hoverAttributeWrites += 1;
    }
  },
);
