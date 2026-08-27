/**
 * Document View — spec §24, 06 §4.9.
 *
 * `Mod+Shift+V` replaces each maximal `//:` comment run with typeset prose.
 * Code lines are untouched and stay fully editable, which is the whole point:
 * this is a *view* of an ordinary `.do` file, stored in the sidecar, never in
 * the source. Toggling it back leaves a file byte-identical to the one that was
 * opened — asserted in `editor.doc.test.ts` along with everything else that must
 * not write.
 *
 * # The live-preview rule
 *
 * A rendered run un-renders to its source whenever the selection intersects it.
 * Without that, putting the caret in a paragraph is impossible and the prose is
 * read-only in an editor. With it, clicking into text does what clicking into
 * text does everywhere else.
 *
 * The rule is implemented by comparing which run the selection is in, not by
 * rebuilding on every selection change: moving the caret within one paragraph,
 * or anywhere in the code, changes nothing and costs one comparison.
 */

import { Compartment, StateField } from "@codemirror/state";
import type { EditorState, Extension } from "@codemirror/state";
import { Decoration, EditorView, WidgetType } from "@codemirror/view";
import type { DecorationSet } from "@codemirror/view";
import { segGeneration, stateSegmenter } from "../blocks/blockField";
import { type NarrativeLine, renderNarrative, stripNarrativePrefix } from "./markdown";

/** Reconfigured, never re-created — toggling must not lose undo or scroll. */
export const docViewCompartment = new Compartment();

/** One rendered narrative run. */
class MarkdownWidget extends WidgetType {
  constructor(
    readonly lines: readonly NarrativeLine[],
    readonly key: string,
  ) {
    super();
  }

  override eq(other: WidgetType): boolean {
    return other instanceof MarkdownWidget && other.key === this.key;
  }

  override toDOM(): HTMLElement {
    const el = document.createElement("div");
    el.className = "cm-mdBlock";
    renderNarrative(el, this.lines);
    return el;
  }

  /**
   * Events reach the editor.
   *
   * Deliberately the opposite of a result card: 06 §4.9 wants a click on
   * rendered prose to place the caret in the corresponding source line, and
   * `setup.ts` maps the click to `data-src` to do it.
   */
  override ignoreEvent(): boolean {
    return false;
  }
}

interface DocViewState {
  readonly deco: DecorationSet;
  readonly gen: number;
  /** Start offset of the run the selection is inside, or -1. */
  readonly open: number;
}

const EMPTY: DocViewState = { deco: Decoration.none, gen: -1, open: -1 };

const docViewField = StateField.define<DocViewState>({
  create(state) {
    return rebuild(state);
  },
  update(value, tr) {
    const gen = segGeneration(tr.state);
    const open = openRun(tr.state);
    if (gen === value.gen && open === value.open && !tr.docChanged) return value;
    return rebuild(tr.state);
  },
  provide: (field) => EditorView.decorations.from(field, (value) => value.deco),
});

/** Which narrative run the selection is inside, by start offset, or -1. */
function openRun(state: EditorState): number {
  const seg = stateSegmenter(state);
  if (seg === null) return -1;
  const head = state.selection.main;
  for (const run of seg.narrativeRegions()) {
    if (head.to >= run.from && head.from <= run.to) return run.from;
  }
  return -1;
}

function rebuild(state: EditorState): DocViewState {
  const seg = stateSegmenter(state);
  const gen = segGeneration(state);
  if (seg === null) return { ...EMPTY, gen };
  const open = openRun(state);

  const ranges: { from: number; to: number; value: Decoration }[] = [];
  for (const run of seg.narrativeRegions()) {
    if (run.from === open) continue;
    const lines: NarrativeLine[] = [];
    const first = state.doc.lineAt(run.from).number;
    const last = state.doc.lineAt(Math.min(run.to, state.doc.length)).number;
    for (let n = first; n <= last; n++) {
      const line = state.doc.line(n);
      const stripped = stripNarrativePrefix(line.text);
      if (stripped === null) continue;
      lines.push({ text: stripped.text, at: line.from + stripped.skip });
    }
    if (lines.length === 0) continue;
    const from = state.doc.line(first).from;
    const to = state.doc.line(last).to;
    ranges.push({
      from,
      to,
      value: Decoration.replace({
        block: true,
        widget: new MarkdownWidget(lines, `${gen}:${from}:${to}`),
      }),
    });
  }

  return {
    gen,
    open,
    deco: Decoration.set(
      ranges.map((r) => r.value.range(r.from, r.to)),
      true,
    ),
  };
}

/** The extension the compartment holds. `false` is Source View — nothing at all. */
export function documentView(enabled: boolean): Extension {
  return enabled ? [docViewField, EditorView.editorAttributes.of({ class: "cm-docView" })] : [];
}

/** Toggle Document View. `Mod+Shift+V`; a view operation, never a document one. */
export function toggleDocumentView(view: EditorView, enabled: boolean): void {
  view.dispatch({ effects: docViewCompartment.reconfigure(documentView(enabled)) });
}

/**
 * Where a click inside rendered prose should put the caret.
 *
 * `data-src` is written by the markdown renderer on every produced node, so this
 * is a walk up the ancestor chain rather than a coordinate guess.
 */
export function sourceOffsetForNode(node: Node | null): number | null {
  let el = node instanceof Element ? node : (node?.parentElement ?? null);
  while (el !== null) {
    const src = (el as HTMLElement).dataset?.["src"];
    if (src !== undefined) {
      const at = Number.parseInt(src, 10);
      return Number.isFinite(at) ? at : null;
    }
    el = el.parentElement;
  }
  return null;
}

export const docViewTheme = EditorView.baseTheme({
  ".cm-mdBlock": {
    fontFamily: "var(--font-serif)",
    fontSize: "var(--fs-mid, 15px)",
    lineHeight: "var(--lh-mid, 25px)",
    maxWidth: "68ch",
    color: "var(--text-body)",
    padding: "var(--sp-8, 8px) 0",
  },
  ".cm-mdHeading": {
    fontFamily: "var(--font-sans)",
    fontWeight: "600",
    margin: "0 0 var(--sp-4, 4px)",
  },
  ".cm-mdParagraph": { margin: "0 0 var(--sp-8, 8px)" },
  ".cm-mdList": { margin: "0 0 var(--sp-8, 8px) var(--sp-16, 16px)" },
  ".cm-mdCode, .cm-mdInlineCode": {
    fontFamily: "var(--font-mono)",
    fontSize: "var(--fs-code, 13px)",
  },
  // 06 §4.9: Document View dims the run gutters to 40% and centres the column.
  "&.cm-docView .cm-blockGutter": { opacity: ".4" },
});
