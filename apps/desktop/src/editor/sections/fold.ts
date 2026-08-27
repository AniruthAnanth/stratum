/**
 * Section and block folding — 06 §4.8, spec §3 ("collapse cell").
 *
 * # Why folding is ours and not `@codemirror/language`'s
 *
 * 06 §4.8 already specifies "a `foldService` provided by us from the section
 * index, not from the syntax tree" — the section structure is comment markers,
 * which no syntax tree of Stata contains. What is different here is only the
 * plumbing: `@codemirror/language` is not a dependency of this app (see
 * `lang/parser.ts` for the escalation), so the fold state is a `StateField` of
 * `Decoration.replace({ block: true })` rather than that package's. The
 * mechanism underneath is identical — CodeMirror's own folding is a replace
 * decoration too.
 *
 * Folding is a VIEW operation. It replaces nothing in the document, and the
 * folded ranges map through edits like every other range in this unit.
 */

import { RangeSet, StateEffect, StateField } from "@codemirror/state";
import type { EditorState } from "@codemirror/state";
import { Decoration, EditorView, WidgetType } from "@codemirror/view";
import type { DecorationSet } from "@codemirror/view";
import { structuralFoldRange } from "../lang/fold";
import { sectionAt, sections } from "./markers";

/** Fold `[from, to)`. */
export const foldRange = StateEffect.define<{ from: number; to: number }>();
/** Unfold anything overlapping `[from, to]`. */
export const unfoldRange = StateEffect.define<{ from: number; to: number }>();

/** The "… 14 lines" placeholder. One instance per fold; they are cheap. */
class FoldPlaceholder extends WidgetType {
  constructor(readonly lines: number) {
    super();
  }

  override eq(other: WidgetType): boolean {
    return other instanceof FoldPlaceholder && other.lines === this.lines;
  }

  override toDOM(): HTMLElement {
    const el = document.createElement("span");
    el.className = "cm-foldPlaceholder";
    el.textContent = `⋯ ${this.lines} lines`;
    el.setAttribute("role", "button");
    el.setAttribute("aria-label", `${this.lines} folded lines, activate to expand`);
    return el;
  }

  override ignoreEvent(): boolean {
    return false;
  }
}

export const foldField = StateField.define<DecorationSet>({
  create() {
    return Decoration.none;
  },
  update(value, tr) {
    let folded = value.map(tr.changes);
    for (const effect of tr.effects) {
      if (effect.is(foldRange)) {
        const lines =
          tr.state.doc.lineAt(effect.value.to).number -
          tr.state.doc.lineAt(effect.value.from).number;
        folded = folded.update({
          add: [
            Decoration.replace({
              block: true,
              widget: new FoldPlaceholder(Math.max(1, lines)),
            }).range(effect.value.from, effect.value.to),
          ],
          sort: true,
        });
      } else if (effect.is(unfoldRange)) {
        const { from, to } = effect.value;
        folded = folded.update({
          filter: (f, t) => t <= from || f >= to,
          filterFrom: from,
          filterTo: to,
        });
      }
    }
    return folded;
  },
  provide: (field) => EditorView.decorations.from(field),
});

/** Whether anything overlapping `[from, to]` is folded. */
export function isFolded(state: EditorState, from: number, to: number): boolean {
  const set = state.field(foldField, false) ?? RangeSet.empty;
  let found = false;
  set.between(from, to, () => {
    found = true;
    return false;
  });
  return found;
}

/**
 * The range a fold at `pos` should cover.
 *
 * A section marker folds its whole section; anything else folds the block it is
 * in, which is what makes a 200-line `program define` collapse to one line. The
 * marker line itself stays visible in both cases — a fold you cannot see the
 * head of is a fold you cannot undo.
 */
export function foldableAt(state: EditorState, pos: number): { from: number; to: number } | null {
  const section = sectionAt(state, pos);
  const list = sections(state);
  const onMarker = list.find((s) => state.doc.lineAt(pos).number === s.markerLine + 1);
  if (onMarker !== undefined) {
    const head = state.doc.line(onMarker.markerLine + 1);
    return head.to < onMarker.to ? { from: head.to, to: onMarker.to } : null;
  }
  const structural = structuralFoldRange(state, pos);
  if (structural !== null) return structural;
  return section === null ? null : { from: state.doc.lineAt(pos).to, to: section.to };
}

export function toggleFoldAt(view: EditorView, pos: number): boolean {
  const range = foldableAt(view.state, pos);
  if (range === null) return false;
  const effect = isFolded(view.state, range.from, range.to)
    ? unfoldRange.of(range)
    : foldRange.of(range);
  view.dispatch({ effects: effect });
  return true;
}

export const foldTheme = EditorView.baseTheme({
  ".cm-foldPlaceholder": {
    color: "var(--text-meta)",
    fontSize: "var(--fs-micro, 11px)",
    padding: "0 var(--sp-6, 6px)",
    cursor: "pointer",
  },
});
