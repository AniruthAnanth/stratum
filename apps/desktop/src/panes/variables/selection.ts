/**
 * Which variables are selected — [GSM] 2, *The Variables window*:
 *
 * > Click once on a variable in the Variables window to select it. Multiple
 * > variables can be selected in the usual fashion, either by Command-clicking
 * > on nonadjacent variables or by clicking on a variable and Shift-clicking on
 * > a second variable to select all intervening variables.
 *
 * and, from *The Properties window*:
 *
 * > If a single variable is selected in the Variables window, its properties are
 * > displayed. If there are multiple variables selected in the Variables window,
 * > the Properties window will display properties that are common across all
 * > selected variables.
 * >
 * > Clicking the arrow buttons next to the lock icon will select the previous or
 * > next variable shown in the Variables window, and that selection will be
 * > reflected in the Properties window.
 *
 * Three panes read this: Variables draws it, Properties follows it, and the
 * Data Editor's sidebar (06 §9.7 — "the **same** Variables and Properties
 * components … one implementation, two mounts") gets it for free. So the
 * selection is a module, not component state: two mounts of the same component
 * with private selections would show a user two different "current variables"
 * in one window, which is the defect [GSM] 2's last sentence rules out.
 *
 * # Why this is not in `state/vars.ts`
 *
 * That store is W12's and its selection is a single `string | undefined`. The
 * manual's selection is a *set* with a primary — Shift-click needs an anchor,
 * `Keep only selected variables` needs the set, and the Properties arrows need
 * an ordering. Rather than reach across R0 for one field, the set lives here and
 * the **primary is mirrored into** `selectVariable()` on every change, so W12's
 * store and anything already reading it (the Modern sidebar's detail drawer,
 * 06 §11) stay correct. Escalated in W16's return: the right long-term home is
 * `state/vars.ts`, and this file becomes a re-export when it grows the field.
 */

import { createSignal } from "solid-js";
import { selectVariable } from "../../state/vars";

export interface VarSelection {
  /** Selected names, in the pane's current display order. */
  readonly names: readonly string[];
  /** The one whose properties are shown; the last row the user touched. */
  readonly primary: string | undefined;
  /** Shift-click's other end. Not the primary: Shift extends from the anchor. */
  readonly anchor: string | undefined;
}

export const EMPTY_SELECTION: VarSelection = {
  names: [],
  primary: undefined,
  anchor: undefined,
};

const [selection, setSelection] = createSignal<VarSelection>(EMPTY_SELECTION);

export const variableSelection = selection;

export interface SelectionCounters {
  /** Selection changes. A hover must never add one. */
  changes: number;
}

const ZERO: SelectionCounters = { changes: 0 };
export const selectionCounters: SelectionCounters = { ...ZERO };
export function resetSelectionCounters(): void {
  Object.assign(selectionCounters, ZERO);
}

function commit(next: VarSelection): void {
  selectionCounters.changes += 1;
  setSelection(next);
  // W12's single-selection store follows the primary. One direction only: this
  // module is the authority, and a two-way sync between a set and a scalar is a
  // loop waiting for the first `selectVariable(undefined)`.
  selectVariable(next.primary);
}

/** A plain click. */
export function selectOnly(name: string): void {
  commit({ names: [name], primary: name, anchor: name });
}

/** `Mod+click` on a nonadjacent variable. */
export function toggleSelection(name: string): void {
  const current = selection();
  const has = current.names.includes(name);
  const names = has ? current.names.filter((n) => n !== name) : [...current.names, name];
  commit({
    names,
    primary: has ? names.at(-1) : name,
    anchor: name,
  });
}

/**
 * `Shift+click`: "all intervening variables", in the pane's **display** order.
 *
 * Display order, not dataset order — the manual describes the gesture over the
 * list as shown, and the list can be sorted or filtered. Selecting a dataset
 * range while the pane is sorted by Label would select variables the user can
 * see no connection between.
 */
export function extendSelection(displayed: readonly string[], name: string): void {
  const current = selection();
  const anchor = current.anchor ?? current.primary;
  const from = anchor === undefined ? -1 : displayed.indexOf(anchor);
  const to = displayed.indexOf(name);
  if (from < 0 || to < 0) {
    selectOnly(name);
    return;
  }
  const [lo, hi] = from <= to ? [from, to] : [to, from];
  commit({
    names: displayed.slice(lo, hi + 1),
    primary: name,
    anchor: current.anchor ?? anchor,
  });
}

/** `Select all` — "all variables in the dataset that satisfy the filter". */
export function selectAll(displayed: readonly string[]): void {
  commit({
    names: [...displayed],
    primary: displayed.at(-1),
    anchor: displayed[0],
  });
}

export function clearSelection(): void {
  commit(EMPTY_SELECTION);
}

/**
 * The Properties pane's `◀ ▶`.
 *
 * Steps the primary through the displayed order and collapses the selection to
 * one variable, which is what the arrows do in Stata: they are a way to walk the
 * list, not a way to extend a set. Returns the new primary, or `undefined` when
 * there is nowhere to step — the caller disables the button on that.
 */
export function stepSelection(displayed: readonly string[], delta: -1 | 1): string | undefined {
  if (displayed.length === 0) return undefined;
  const current = selection().primary;
  const at = current === undefined ? -1 : displayed.indexOf(current);
  // From nothing, `▶` lands on the first row and `◀` on the last, so a fresh
  // pane responds to both arrows rather than to one of them.
  const next = at < 0 ? (delta === 1 ? 0 : displayed.length - 1) : at + delta;
  const name = displayed[next];
  if (name === undefined) return undefined;
  selectOnly(name);
  return name;
}

/**
 * Drop names that are no longer in the dataset.
 *
 * Called after `drop`/`keep`/`use`. A selection that outlives its variables is
 * how the Properties pane comes to offer `label variable` on a name the engine
 * has never heard of.
 */
export function pruneSelection(existing: readonly string[]): void {
  const current = selection();
  const live = new Set(existing);
  const names = current.names.filter((n) => live.has(n));
  if (names.length === current.names.length) return;
  commit({
    names,
    primary:
      current.primary !== undefined && live.has(current.primary) ? current.primary : names.at(-1),
    anchor: current.anchor !== undefined && live.has(current.anchor) ? current.anchor : undefined,
  });
}

/** Test seam. */
export function resetSelectionState(): void {
  setSelection(EMPTY_SELECTION);
  selectVariable(undefined);
  resetSelectionCounters();
}
