/**
 * Deterministic completion — 06 §4.1, CONTRACTS §14, ADR A11.
 *
 * # The hard contract
 *
 * `StratumSegmenter.complete(pos)` is documented as **< 2 ms** and is
 * synchronous wasm on the main thread. That is why completion in this product
 * can be `activateOnTyping` without a debounce: there is no request, no
 * cancellation, and no popup that arrives after the user has typed past it.
 *
 * # Why the popup is not wired here yet
 *
 * `autocompletion()` lives in `@codemirror/autocomplete`, which this app does
 * not depend on, and `apps/desktop/package.json` is W12's file (R0). So this
 * module ships the SOURCE — the part that is actually ours — as a plain function
 * over the segmenter, shaped exactly like a CM6 `CompletionSource` so that
 * wiring it is one line in `setup.ts` once the dependency lands. The types below
 * are structural duplicates of that package's, declared locally and marked as
 * such; they are five fields and they are frozen by CM6's own API, so the
 * duplication cannot rot silently.
 *
 * The AI source (06 §16.1, `boost: -50`) is module 07's and is not declared
 * here: an empty second source would look like a wired feature.
 */

import type { EditorState } from "@codemirror/state";
import type { CompletionItem, CompletionKind } from "../../wasm/types";
import { stateSegmenter } from "../blocks/blockField";

/** Structural mirror of `@codemirror/autocomplete`'s `Completion`. */
export interface EditorCompletion {
  label: string;
  type?: string;
  detail?: string;
  apply?: string;
  boost?: number;
}

/** Structural mirror of `@codemirror/autocomplete`'s `CompletionResult`. */
export interface EditorCompletionResult {
  from: number;
  to: number;
  options: EditorCompletion[];
  /** Set when the environment behind these items was itself capped (A11). */
  truncated: boolean;
}

/**
 * Icon/sort group per kind.
 *
 * `icons: false` in the extension list means these never become pictures; they
 * are the sort group and the right-aligned kind label. A variable and a local
 * macro that sort together is the difference between a usable popup and a list.
 */
const KIND_ORDER: Readonly<Record<CompletionKind, number>> = {
  command: 0,
  option: 1,
  variable: 2,
  local: 3,
  global: 4,
  scalar: 5,
  matrix: 6,
  frame: 7,
  value_label: 8,
  stored_estimate: 9,
  stored_result: 10,
  function: 11,
  path: 12,
  keyword: 13,
};

/**
 * The deterministic source.
 *
 * Returns `null` for "nothing to offer", which is CM6's contract for a source
 * that declines — not an empty list, which would suppress the other sources.
 */
export function stataDeterministicCompletions(
  state: EditorState,
  pos: number,
): EditorCompletionResult | null {
  const seg = stateSegmenter(state);
  if (seg === null) return null;

  const list = seg.raw().complete(pos);
  if (list.items.length === 0) return null;

  return {
    from: list.from,
    to: list.to,
    truncated: list.truncated,
    options: list.items.map(toCompletion),
  };
}

function toCompletion(item: CompletionItem): EditorCompletion {
  const completion: EditorCompletion = {
    label: item.label,
    type: item.kind,
    // A11: `detail` is never a variable label. The engine decides what goes
    // here; re-deriving it in the frontend is how the two start disagreeing.
    ...(item.detail === null ? {} : { detail: item.detail }),
    ...(item.insert === null ? {} : { apply: item.insert }),
    // Ordering is the engine's `rank` within `kind`, and `kind` groups come in
    // the order above. Negated because CM6 sorts by DESCENDING boost.
    boost: -(KIND_ORDER[item.kind] * 1000 + item.rank),
  };
  return completion;
}

/** Deterministic quick fixes at a position — the same engine, same contract. */
export function stataQuickFixes(state: EditorState, pos: number): { label: string }[] {
  const seg = stateSegmenter(state);
  if (seg === null) return [];
  return seg.raw().quickFixes(pos);
}
