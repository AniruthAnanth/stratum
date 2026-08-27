/**
 * `blockField` — 06 §4.4. The one place segmentation is driven from.
 *
 * A `StateField` and not a `ViewPlugin`, because the block outline is a fact
 * about the *document* and must exist in a state that was never attached to a
 * view: `commands.ts` resolves "the block under the cursor" from
 * `EditorState`, and the e2e harness drives states with no DOM at all.
 *
 * # Why the value is two numbers and a reference
 *
 * The obvious value is `Block[]`. It is the wrong one. Materialising the array
 * inside `update()` decodes every region on every keystroke whether or not
 * anything reads it — and on a keystroke inside a 3 000-region file with the
 * gutter scrolled off screen, nothing does. So the field stores the segmenter
 * and the generation it last produced, and the array is decoded on demand and
 * cached for that generation (`EditorSegmenter.blocks`). `counters
 * .regionDecodePasses` is what keeps that claim honest.
 *
 * The generation is in the value so that dependent extensions can compare
 * cheaply: `prev.gen !== next.gen` is the exact test for "the outline moved",
 * and it is false for a selection-only transaction, which is most of them.
 */

import { StateEffect, StateField } from "@codemirror/state";
import type { EditorState, Transaction } from "@codemirror/state";
import type { Block, EditorSegmenter } from "./segmenter";
import { segmenterOf } from "./segmenter";

/**
 * Force a full re-synchronisation of the wasm mirror from the document.
 *
 * The recovery path for `BlockMismatch` (06 §5.5) — the kernel re-segments the
 * text it was sent and disagreed with us — and for a segmenter that finished
 * initialising after the editor mounted. Costs one `setDoc`, which is O(doc);
 * it is not on any interaction path and must never be put on one.
 */
export const resyncSegmentation = StateEffect.define<null>();

/** What `blockField` holds. See the file header for why it is not `Block[]`. */
export interface BlockIndex {
  /** Segmentation generation. Changes only when the outline actually changed. */
  readonly gen: number;
  /** `null` until wasm has initialised. The editor is usable before that. */
  readonly seg: EditorSegmenter | null;
}

const EMPTY: BlockIndex = { gen: -1, seg: null };

export const blockField = StateField.define<BlockIndex>({
  create(state) {
    const seg = segmenterOf(state);
    if (seg === null) return EMPTY;
    seg.setDoc(state.doc.toString());
    return { gen: seg.generation, seg };
  },

  update(value, tr) {
    const seg = segmenterOf(tr.state);
    if (seg === null) return value.seg === null ? value : EMPTY;

    // The segmenter arrived (or was swapped) after this field was created. A
    // full replace is the only correct move: the wasm mirror has never seen this
    // document, and splicing into an empty engine would desynchronise the two.
    if (seg !== value.seg || tr.effects.some((e) => e.is(resyncSegmentation))) {
      seg.setDoc(tr.state.doc.toString());
      return { gen: seg.generation, seg };
    }

    if (!tr.docChanged) return value;

    const gen = seg.applyChanges(tr.changes);
    return gen === value.gen ? value : { gen, seg };
  },
});

// ---------------------------------------------------------------------------
// Queries. Every consumer in this unit goes through these rather than reaching
// for the field, so "how do I get the block under the cursor" has one answer.
// ---------------------------------------------------------------------------

/** The segmenter this state is driving, or `null` before wasm is ready. */
export function stateSegmenter(state: EditorState): EditorSegmenter | null {
  return state.field(blockField, false)?.seg ?? null;
}

/** Every block. Decoded once per generation; see the file header. */
export function allBlocks(state: EditorState): readonly Block[] {
  return stateSegmenter(state)?.blocks() ?? [];
}

/** The block whose outer extent contains `pos`. */
export function blockAt(state: EditorState, pos: number): Block | null {
  return stateSegmenter(state)?.blockAt(pos) ?? null;
}

/** The block the primary cursor is in. */
export function blockAtCursor(state: EditorState): Block | null {
  return blockAt(state, state.selection.main.head);
}

/** Blocks overlapping a range — the viewport query. O(log n + k). */
export function blocksTouching(state: EditorState, from: number, to: number): Block[] {
  return stateSegmenter(state)?.blocksTouching(from, to) ?? [];
}

/** The block a 1-based line number belongs to. */
export function blockAtLine(state: EditorState, line: number): Block | null {
  if (line < 1 || line > state.doc.lines) return null;
  return blockAt(state, state.doc.line(line).from);
}

/**
 * Where a card for this block belongs: the END of the block's last line.
 *
 * Not `block.to`, which is the last code unit of the statement — a card anchored
 * there would render before a trailing comment on the same line. Not
 * `block.outerTo` either, which for a block with attached trailing comments
 * would put the card after them; the comment belongs to the block visually, so
 * the card goes below the whole thing. Both are the same offset for the common
 * case; they differ exactly on the lines where getting it wrong is visible.
 */
export function cardAnchor(state: EditorState, block: Block): number {
  // `outerTo` is EXCLUSIVE — consecutive outer extents tile the file, so a
  // block's `outerTo` is the next block's `outerFrom`, which is the first
  // character of the following line. Taking `lineAt(outerTo)` therefore lands a
  // line too low and puts the card below the next statement. The `-1` is the
  // whole difference between a card under its block and a card under someone
  // else's; it cost one failing anchor-deletion test to find.
  const end = Math.max(block.to, block.outerTo - 1);
  return state.doc.lineAt(Math.min(Math.max(end, 0), state.doc.length)).to;
}

/** The segmentation generation, or -1 before wasm is ready. */
export function segGeneration(state: EditorState): number {
  return state.field(blockField, false)?.gen ?? -1;
}

/** True when this transaction changed the outline, not merely the selection. */
export function outlineChanged(tr: Transaction, before: BlockIndex): boolean {
  return tr.state.field(blockField, false)?.gen !== before.gen;
}
