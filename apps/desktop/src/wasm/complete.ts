/**
 * The completion half of the segmenter interface.
 *
 * The popup is on the keystroke path and CONTRACTS §14 gives `complete()` a hard
 * 2 ms budget, so everything here is synchronous, allocation-light and free of
 * any IPC. The rules that live in this file rather than in the editor are the
 * ones that must not vary between the two backends:
 *
 * * **Ordering is total.** Rank, then label, then kind. A popup whose order
 *   depends on the backend's iteration order would make the editor's own tests
 *   backend-dependent, which is the exact property W11a exists to prevent.
 * * **Truncation is the engine's, not the popup's.** `CompletionEnv` is capped
 *   by construction (A11); the popup reports what the engine shed and offers
 *   "more…", which is an explicit interaction rather than a keystroke.
 */

import type { CompletionItem, CompletionKind, CompletionList, StratumSegmenter } from "./types.ts";

/** Display group for a completion kind, in popup order. */
export const KIND_GROUPS: Record<CompletionKind, string> = {
  variable: "Variables",
  local: "Locals",
  global: "Globals",
  scalar: "Scalars",
  matrix: "Matrices",
  frame: "Frames",
  value_label: "Value labels",
  stored_estimate: "Estimates",
  stored_result: "Stored results",
  command: "Commands",
  option: "Options",
  function: "Functions",
  path: "Files",
  keyword: "Keywords",
};

/**
 * The one comparison the popup sorts by.
 *
 * Exported because the editor re-sorts after merging in its own history-based
 * candidates, and it must merge into the same order the segmenter produced.
 */
export function compareItems(a: CompletionItem, b: CompletionItem): number {
  return a.rank - b.rank || a.label.localeCompare(b.label) || a.kind.localeCompare(b.kind);
}

/**
 * "2 048 of 32 767", or `null` when nothing was shed.
 *
 * Digit grouping uses a narrow no-break space via `Intl.NumberFormat`, not a
 * comma: the number appears next to Stata output where a comma already means
 * something, and the group separator is locale-dependent everywhere else in the
 * UI too.
 */
export function truncationNotice(list: CompletionList, locale?: string): string | null {
  if (!list.truncated) return null;
  const fmt = new Intl.NumberFormat(locale);
  return `${fmt.format(list.offered)} of ${fmt.format(list.total)}`;
}

/** A backend-agnostic document edit. The editor turns it into a transaction. */
export interface CompletionEdit {
  /** Start of the replaced range, in UTF-16 code units. */
  from: number;
  /** End of the replaced range. */
  to: number;
  /** Text to insert. */
  insert: string;
  /** Where the cursor lands, relative to `from`. */
  cursor: number;
}

/**
 * Turn an accepted item into an edit.
 *
 * `insert` overrides `label` when the item carries one — a function completes as
 * `strpos(` with the cursor inside the parenthesis, which is the difference
 * between completion that helps and completion that has to be undone.
 */
export function acceptEdit(list: CompletionList, item: CompletionItem): CompletionEdit {
  const insert = item.insert ?? item.label;
  const caret = insert.indexOf("$0");
  if (caret >= 0) {
    const text = insert.slice(0, caret) + insert.slice(caret + 2);
    return { from: list.from, to: list.to, insert: text, cursor: caret };
  }
  return { from: list.from, to: list.to, insert, cursor: insert.length };
}

/**
 * The keystroke-path facade over a {@link StratumSegmenter}.
 *
 * It exists so the editor holds one object with a completion-shaped API rather
 * than reaching into the segmenter for four unrelated methods, and so the
 * environment generation — the thing that tells you whether the variable you
 * just created is completable yet — is observable in one place.
 */
export class Completions {
  private seg: StratumSegmenter;
  private lastEnvGeneration = 0;

  constructor(segmenter: StratumSegmenter) {
    this.seg = segmenter;
  }

  /**
   * Push the engine's msgpack `CompletionEnv`, exactly as it arrived on
   * `StateChanged`. Re-encoding it as JSON in the webview would put a parse of
   * the whole environment on the broadcast path.
   */
  setEnv(msgpack: Uint8Array): void {
    this.seg.setCompletionEnv(msgpack);
    this.lastEnvGeneration = this.seg.completionEnvGeneration();
  }

  /** Generation of the environment currently loaded. */
  get envGeneration(): number {
    return this.lastEnvGeneration;
  }

  /** Completion at a UTF-16 offset, already in popup order. */
  at(pos: number): CompletionList {
    const list = this.seg.complete(pos);
    return { ...list, items: [...list.items].sort(compareItems) };
  }

  /** Group an ordered list for a sectioned popup, preserving item order. */
  grouped(list: CompletionList): Array<{ group: string; items: CompletionItem[] }> {
    const out: Array<{ group: string; items: CompletionItem[] }> = [];
    for (const item of list.items) {
      const group = KIND_GROUPS[item.kind] ?? "Other";
      const last = out[out.length - 1];
      if (last && last.group === group) last.items.push(item);
      else out.push({ group, items: [item] });
    }
    return out;
  }
}
