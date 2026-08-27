/**
 * Structural fold ranges — 06 §4.8.
 *
 * The foldable structure of a do-file is the segmenter's, not a syntax tree's:
 * a `foreach … { … }`, a `program define … end`, a `mata … end` and a `#delimit`
 * region are all things `RegionKind` already names, and re-deriving them from
 * tokens would be a second segmenter with a different opinion.
 *
 * Sections are handled in `sections/fold.ts`, which layers the `// %%` index on
 * top of what this file answers.
 */

import type { EditorState } from "@codemirror/state";
import { blockAt } from "../blocks/blockField";
import type { Block } from "../blocks/segmenter";

/** A fold range: everything after the head line, up to the block's last line. */
export interface FoldRange {
  readonly from: number;
  readonly to: number;
}

/** Region kinds whose body is worth folding. A one-line `gen` is not. */
export function isFoldableBlock(block: Block): boolean {
  const kind = block.kind;
  if (kind === null) return false;
  return kind.kind === "brace" || kind.kind === "end_block";
}

/**
 * The fold range for the structure at `pos`, or `null`.
 *
 * The head line always stays visible. A fold whose own header is hidden is a
 * fold the user cannot find again, and CodeMirror will happily create one.
 */
export function structuralFoldRange(state: EditorState, pos: number): FoldRange | null {
  const block = blockAt(state, pos);
  if (block === null || !isFoldableBlock(block)) return null;
  const head = state.doc.lineAt(block.from);
  const end = Math.min(block.to, state.doc.length);
  return head.to < end ? { from: head.to, to: end } : null;
}

/** Every foldable structure overlapping a range — the fold gutter's query. */
export function structuralFoldsIn(state: EditorState, from: number, to: number): FoldRange[] {
  const out: FoldRange[] = [];
  let pos = from;
  while (pos <= to) {
    const block = blockAt(state, pos);
    if (block === null) break;
    if (isFoldableBlock(block)) {
      const range = structuralFoldRange(state, block.from);
      if (range !== null) out.push(range);
    }
    if (block.outerTo <= pos) break;
    pos = block.outerTo + 1;
  }
  return out;
}
