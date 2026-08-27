/**
 * Three-state header sort — 06 §9.4 and [GSM] 2, quoted in full because every
 * clause is a requirement:
 *
 * > You can change the display order of the variables in the Variables window by
 * > clicking on any column header. The first click sorts in ascending order, the
 * > second click sorts in descending order, and the third click puts the
 * > variables back in dataset order. … Sorting in the Variables window is live,
 * > so if you change a property of a variable when the Variables window is
 * > sorted by that property, it will automatically move the variable to its
 * > proper location. **Reordering the display order of the variables in the
 * > Variables window does not affect the order of the variables in the dataset
 * > itself.**
 *
 * Three consequences, and they are the three that are easy to get wrong:
 *
 *  1. **Three states, not two.** A two-state toggle strands the user: dataset
 *     order is the order `varlist` abbreviations, `order`, and every do-file the
 *     user has ever written are expressed in, and there has to be a way back to
 *     it that is not "reload the data".
 *  2. **Live.** This module returns a *permutation of indices* computed from the
 *     rows it is handed, so a re-render after `label variable` re-sorts by
 *     construction. Nothing caches a sorted copy of the list, because a cached
 *     copy is what stops being live.
 *  3. **Display-only.** There is no code path from here to `sort`, `order` or
 *     any other engine command. `variables.test.tsx` asserts that three header
 *     clicks submit nothing at all — the absence of a command is the property,
 *     so it is asserted against the submission recorder rather than against the
 *     rendered order.
 */

import type { VarColumnId } from "./columns";

export type SortDirection = "asc" | "desc";

export interface SortState {
  /** `undefined` is dataset order — the third click, and the initial state. */
  readonly column: VarColumnId | undefined;
  readonly direction: SortDirection;
}

export const DATASET_ORDER: SortState = { column: undefined, direction: "asc" };

export interface SortCounters {
  /** Header clicks that changed the sort. */
  sorts: number;
  /**
   * Comparator calls. The claim this proves is the one PRODUCT_SPEC §0a cares
   * about: comparisons happen when the ordering changes, never on a hover, a
   * selection or a filter keystroke. Dataset order does zero.
   */
  comparisons: number;
}

const ZERO: SortCounters = { sorts: 0, comparisons: 0 };
export const sortCounters: SortCounters = { ...ZERO };
export function resetSortCounters(): void {
  Object.assign(sortCounters, ZERO);
}

/** asc → desc → dataset order, and a different column restarts at asc. */
export function nextSort(current: SortState, column: VarColumnId): SortState {
  if (current.column !== column) return { column, direction: "asc" };
  if (current.direction === "asc") return { column, direction: "desc" };
  return DATASET_ORDER;
}

/** What the header cell announces: `aria-sort`'s vocabulary. */
export function ariaSort(
  state: SortState,
  column: VarColumnId,
): "ascending" | "descending" | "none" {
  if (state.column !== column) return "none";
  return state.direction === "asc" ? "ascending" : "descending";
}

/**
 * Compare two cell strings.
 *
 * Deliberately NOT `localeCompare`: the ICU collation a webview ships with
 * differs across macOS, Windows and WebKitGTK, so `localeCompare` would put
 * `_merge` before or after `make` depending on the platform — and Scenario E
 * compares the same analysis across all three. This is a lowercase codepoint
 * comparison, which is the same everywhere and is what "alphabetical order"
 * means for the ASCII-plus-underscore names Stata permits.
 *
 * Empty is last in both directions. A blank Label is missing information, not a
 * value that sorts before every other value, and burying eleven unlabelled
 * variables at the top of an ascending sort hides the ones the user can read.
 */
export function compareCells(a: string, b: string): number {
  sortCounters.comparisons += 1;
  if (a === b) return 0;
  if (a === "") return 1;
  if (b === "") return -1;
  const la = a.toLowerCase();
  const lb = b.toLowerCase();
  if (la < lb) return -1;
  if (la > lb) return 1;
  // Same letters, different case: fall back to the raw text so the order is
  // total rather than "whichever the sort algorithm happened to visit first".
  return a < b ? -1 : 1;
}

/**
 * The display permutation: indices into `rows`, in the order to draw them.
 *
 * Returning indices rather than rows is what keeps this display-only in a way a
 * reader can check — there is no sorted array of variables anywhere in the
 * frontend for a later change to mistake for the dataset.
 *
 * The sort is **stable by dataset index**: two variables with the same label
 * keep their dataset order relative to each other, so an ascending sort by Label
 * on a dataset where nothing is labelled is exactly dataset order rather than a
 * shuffle. `Array.prototype.sort` is specified stable since ES2019, but the
 * tie-break is written out anyway because the property being relied on is worth
 * saying, and because it also makes the comparator a total order.
 */
export function displayOrder<T>(
  rows: readonly T[],
  sort: SortState,
  key: ((row: T) => string) | undefined,
): number[] {
  const indices = rows.map((_, i) => i);
  if (sort.column === undefined || key === undefined) return indices;

  const keys = rows.map(key);
  const sign = sort.direction === "asc" ? 1 : -1;
  indices.sort((a, b) => {
    const cmp = compareCells(keys[a] as string, keys[b] as string);
    return cmp !== 0 ? sign * cmp : a - b;
  });
  return indices;
}

/** Bump `sorts`; called by the pane when a header click changed the state. */
export function countSort(): void {
  sortCounters.sorts += 1;
}
