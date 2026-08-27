/**
 * Explicit cells — spec §3, 06 §4.8.
 *
 * `// %% Label`, `//%% Label` and `* %% Label` are all valid Stata comments, and
 * that is the whole design: the markers stay in the source, the file still runs
 * in Stata 18, and nothing here needs a sidecar to find them again. Recognition
 * belongs to the segmenter (`sections()` comes out of the same wasm module that
 * produces blocks), so this file draws them and answers questions about them; it
 * does not parse them.
 *
 * # This file writes nothing
 *
 * `section.rename` and `section.move*` DO edit the document — the name lives in
 * the source, which is correct — and by A15 they are commands owned by W26 and
 * gated by `assert_comment_only` / `assert_statement_partition_preserved`. So
 * they are a seam here, not an implementation: {@link setSectionWriter} is how
 * W26 supplies them, and until it does the commands report themselves
 * unavailable rather than reaching for `view.dispatch`.
 */

import type { EditorState } from "@codemirror/state";
import { Decoration, EditorView, ViewPlugin } from "@codemirror/view";
import type { DecorationSet, ViewUpdate } from "@codemirror/view";
import type { SectionView } from "../../wasm/types";
import { segGeneration, stateSegmenter } from "../blocks/blockField";

/** Every section in the document, in order. Cheap: one flat view, no decode. */
export function sections(state: EditorState): SectionView[] {
  return stateSegmenter(state)?.sections() ?? [];
}

/** The section containing `pos`, or `null` above the first marker. */
export function sectionAt(state: EditorState, pos: number): SectionView | null {
  const list = sections(state);
  for (let i = list.length - 1; i >= 0; i--) {
    const section = list[i];
    if (section !== undefined && section.from <= pos && pos <= section.to) return section;
  }
  return null;
}

/** The label text, sliced from the document — the marker IS the storage. */
export function sectionTitle(state: EditorState, section: SectionView): string {
  return state.doc.sliceString(section.titleFrom, section.titleTo).trim();
}

// ---------------------------------------------------------------------------
// The write seam (A15 — W26 owns the four gated writers)
// ---------------------------------------------------------------------------

export interface SectionWriter {
  rename(view: EditorView, section: SectionView, title: string): Promise<boolean>;
  move(view: EditorView, section: SectionView, direction: -1 | 1): Promise<boolean>;
  /**
   * `section.insertAbove` / `section.insertBelow` (spec §3).
   *
   * A FIFTH writer that A15's gated list does not name — inserting a `// %%`
   * marker necessarily changes the document. It is routed through the same seam
   * rather than implemented here, because a write this unit performs itself is a
   * write nothing gates. Flagged in W13's return.
   */
  insert(view: EditorView, at: number, title: string): Promise<boolean>;
}

let writer: SectionWriter | null = null;

/** W26 installs the gated writers. Nothing in this unit may substitute for them. */
export function setSectionWriter(next: SectionWriter | null): void {
  writer = next;
}

export function sectionWriter(): SectionWriter | null {
  return writer;
}

// ---------------------------------------------------------------------------
// Decorations
// ---------------------------------------------------------------------------

const SECTION_LINE = Decoration.line({ class: "cm-sectionHead" });
const SIGIL = Decoration.mark({ class: "cm-sectionSigil" });

/**
 * Section head rules and the dimmed `%%` sigil.
 *
 * Viewport-scoped like everything else on this path. The sigil is dimmed to
 * 35 % and never hidden in Source View: the source is the truth, and a marker
 * you cannot see is a marker you delete by accident. Document View hides it —
 * that is `docview.ts`'s job, through a class on the editor root.
 */
export const sectionDecorations = ViewPlugin.fromClass(
  class {
    decorations: DecorationSet;
    private gen = -1;

    constructor(readonly view: EditorView) {
      this.decorations = build(view);
      this.gen = segGeneration(view.state);
    }

    update(update: ViewUpdate): void {
      const gen = segGeneration(update.state);
      if (gen === this.gen && !update.viewportChanged) return;
      this.gen = gen;
      this.decorations = build(update.view);
    }
  },
  { decorations: (plugin) => plugin.decorations },
);

function build(view: EditorView): DecorationSet {
  const { from, to } = view.viewport;
  const ranges: { from: number; to: number; value: Decoration }[] = [];
  for (const section of sections(view.state)) {
    const markerStart = lineStart(view.state, section);
    if (markerStart === null || markerStart > to) continue;
    if (markerStart < from) continue;
    ranges.push({ from: markerStart, to: markerStart, value: SECTION_LINE });
    if (section.titleFrom > markerStart) {
      ranges.push({ from: markerStart, to: section.titleFrom, value: SIGIL });
    }
  }
  return Decoration.set(
    ranges.map((r) => r.value.range(r.from, r.to)),
    true,
  );
}

function lineStart(state: EditorState, section: SectionView): number | null {
  if (section.markerLine < 0 || section.markerLine >= state.doc.lines) return null;
  return state.doc.line(section.markerLine + 1).from;
}

export const sectionTheme = EditorView.baseTheme({
  ".cm-sectionHead": {
    borderTop: "var(--hairline, 1px) solid var(--border-strong)",
    fontSize: "var(--fs-micro, 11px)",
    textTransform: "uppercase",
    letterSpacing: "var(--ls-micro, .06em)",
    fontWeight: "500",
  },
  ".cm-sectionSigil": { opacity: ".35" },
  "&.cm-docView .cm-sectionSigil": { display: "none" },
});
