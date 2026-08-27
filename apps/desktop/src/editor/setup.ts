/**
 * The editor — 06 §4.1's extension list, assembled.
 *
 * # What is here, and what is missing and why
 *
 * §4.1 names twenty-odd extensions. Fourteen of them come from
 * `@codemirror/state` and `@codemirror/view`, which this app depends on, and are
 * wired below. Seven come from `@codemirror/language`, `@codemirror/commands`,
 * `@codemirror/autocomplete` and `@codemirror/search`, which it does not:
 *
 *   `history()`, `foldGutter()`, `indentOnInput()`, `bracketMatching()`,
 *   `closeBrackets()`, `autocompletion()`, `highlightSelectionMatches()`
 *
 * `apps/desktop/package.json` and `pnpm-lock.yaml` are W12's files and R0
 * forbids reaching across for them, so this unit built everything its acceptance
 * bullets are written against on the two packages that are present, and the four
 * missing dependencies are escalated in its return rather than worked around
 * silently. Nothing below is a substitute for them: folding and the completion
 * source are ours by design (06 §4.8, §4.2), and the other five are one line
 * each the day the manifest changes.
 *
 * # Compartments
 *
 * Theme, keymap, wrapping and Document View are compartments so that a
 * preference change RECONFIGURES rather than re-creates: switching layout preset
 * must not lose undo history or scroll position (§4.1). The inline-results mode
 * is a `StateEffect` on `resultsField` instead of a compartment, because the
 * mode is data the field already owns and a compartment would mean two places
 * that know which mode is current.
 */

import { Compartment, EditorState, StateEffect } from "@codemirror/state";
import type { Extension } from "@codemirror/state";
import {
  EditorView,
  crosshairCursor,
  drawSelection,
  dropCursor,
  highlightSpecialChars,
  lineNumbers,
  rectangularSelection,
} from "@codemirror/view";
import { registerPane } from "../dock/panes";
import type { InlineResultsMode } from "../ipc/hand";
import { editorKeymapExtension } from "../keys/editor";
import type { StratumSegmenter } from "../wasm/types";
import { blockAt, blockField } from "./blocks/blockField";
import { blockGutter, blockGutterTheme, runningLines } from "./blocks/blockGutter";
import { blockHoverPlugin } from "./blocks/hover";
import { EditorSegmenter, counters, segmenterFacet } from "./blocks/segmenter";
import { setActiveEditor } from "./commands";
import { helpTheme, stataHoverHelp } from "./lang/help";
import { stataHighlight, stataHighlightTheme } from "./lang/highlight";
import { anchorById, anchorsIn, resultsField, setCardUi, setInlineMode } from "./results/anchor";
import { recordOrphans } from "./results/orphans";
import { scrollCompensation } from "./results/scrollAnchor";
import { applyCardStateFlags, displayStatus } from "./results/widget";
import {
  docViewCompartment,
  docViewTheme,
  documentView,
  sourceOffsetForNode,
} from "./sections/docview";
import { foldField, foldTheme } from "./sections/fold";
import { sectionDecorations, sectionTheme } from "./sections/markers";

/** Reconfigured, never re-created. */
export const themeCompartment = new Compartment();
export const wrapCompartment = new Compartment();

/** What an editor needs to exist. Everything else arrives through a seam. */
export interface EditorCtx {
  /** The segmenter, if wasm has already initialised. May be attached later. */
  readonly segmenter?: StratumSegmenter | undefined;
  /** Starting inline-results mode; the layout's `defaults.inlineResults`. */
  readonly inlineResults?: InlineResultsMode;
  /** Document View on at open, from the sidecar. */
  readonly docView?: boolean;
  /** Extra extensions the host wants — the AI hint layer, a test probe. */
  readonly extra?: Extension;
}

/**
 * 06 §4.1's list.
 *
 * Order matters in two places and nowhere else: the block gutter is registered
 * before `lineNumbers` so it sits to its LEFT, and the keymap compartment is
 * `Prec.highest` inside W12's own extension so nothing here can pre-empt it.
 */
export function stataEditor(ctx: EditorCtx = {}): Extension[] {
  const seg = ctx.segmenter === undefined ? null : new EditorSegmenter(ctx.segmenter);
  return [
    ...(seg === null ? [] : [segmenterFacet.of(seg)]),

    blockGutter(),
    lineNumbers(),
    highlightSpecialChars(),
    drawSelection(),
    dropCursor(),
    EditorState.allowMultipleSelections.of(true),
    rectangularSelection(),
    crosshairCursor(),

    // --- ours ---
    blockField,
    stataHighlight,
    blockHoverPlugin,
    runningLines,
    resultsField,
    scrollCompensation,
    sectionDecorations,
    foldField,
    docViewCompartment.of(documentView(ctx.docView === true)),
    stataHoverHelp,

    themeCompartment.of([
      stataHighlightTheme,
      blockGutterTheme,
      sectionTheme,
      foldTheme,
      docViewTheme,
      helpTheme,
      cardTheme,
      baseEditorTheme,
    ]),
    wrapCompartment.of([]),
    editorKeymapExtension(),

    EditorView.scrollMargins.of(() => ({ top: 24, bottom: 120 })),
    EditorView.updateListener.of(onUpdate),
    EditorView.domEventHandlers({
      focus(_event, view) {
        setActiveEditor(view);
        return false;
      },
      mousedown: onMouseDown,
    }),

    ...(ctx.extra === undefined ? [] : [ctx.extra]),
  ];
}

/** Create a view. The host owns the parent element; this owns everything in it. */
export function createEditor(parent: HTMLElement, doc: string, ctx: EditorCtx = {}): EditorView {
  const view = new EditorView({
    parent,
    state: EditorState.create({ doc, extensions: stataEditor(ctx) }),
  });
  if (ctx.inlineResults !== undefined) {
    view.dispatch({ effects: setInlineMode.of(ctx.inlineResults) });
  }
  setActiveEditor(view);
  return view;
}

/**
 * Register the editor as the dock's `editor` pane.
 *
 * **`apps/desktop/src/panes/editor/**` is owned by no unit.** `PaneId "editor"`
 * is one of the thirteen in CONTRACTS §12 and `docs/ownership.toml` claims a
 * directory for eleven of the others; the editor's is missing, in exactly the
 * way `"sections"` was before ARCHITECT amendment A34 assigned it here. Rather
 * than create a file no unit owns (R0), the registration lives in the editor's
 * own module, which is defensible on its merits — the editor knows how to mount
 * itself — and is one call for whoever wires the shell:
 *
 * ```ts
 * registerEditorPane("", { inlineResults: layout.defaults.inlineResults });
 * ```
 *
 * Flagged in W13's return so the partition can be fixed deliberately.
 */
export function registerEditorPane(doc: string, ctx: EditorCtx = {}): () => void {
  return registerPane("editor", (host, register) => {
    const view = createEditor(host, doc, ctx);
    register(() => {
      view.destroy();
    });
  });
}

/**
 * Attach a segmenter to a live editor.
 *
 * The editor mounts before wasm has finished instantiating, and that is
 * deliberate: showing the text immediately and the block outline a frame later
 * beats an empty editor (`boot/segmenter.ts` says the same). Reconfiguring a
 * static facet requires rebuilding the configuration, so this replaces the whole
 * extension set — once, at startup, never on an interaction path — and
 * `blockField` notices the new segmenter and re-synchronises it in the same
 * transaction.
 */
export function attachSegmenter(
  view: EditorView,
  seg: StratumSegmenter,
  ctx: EditorCtx = {},
): void {
  view.dispatch({
    effects: StateEffect.reconfigure.of(stataEditor({ ...ctx, segmenter: seg })),
  });
}

// ---------------------------------------------------------------------------
// The update listener
// ---------------------------------------------------------------------------

/**
 * Everything that must happen after a transaction, in one place.
 *
 * Three jobs, and each one is bounded by the VIEWPORT rather than the document:
 * drain the orphans this transaction produced, push the display status onto the
 * cards on screen, and keep the active-editor pointer current. None of them
 * dispatches, so none of them can loop.
 */
function onUpdate(update: {
  readonly view: EditorView;
  readonly state: EditorState;
  readonly docChanged: boolean;
  readonly viewportChanged: boolean;
}): void {
  const results = update.state.field(resultsField, false);
  if (results !== undefined && results.orphaned.length > 0) recordOrphans(results.orphaned);

  if (!update.docChanged && !update.viewportChanged) return;

  const { from, to } = update.view.viewport;
  const cards = anchorsIn(update.state, from, to).map((a) => ({
    id: a.rec.id,
    state: displayStatus(a.rec, blockAt(update.state, a.at)),
  }));
  if (cards.length > 0) applyCardStateFlags(update.view, cards);
}

/**
 * One delegated listener for every card action and every rendered-prose click.
 *
 * 500 cards would otherwise be 500 listeners. Card DOM carries `data-anchor` and
 * `data-action`; rendered markdown carries `data-src`. Both are read here.
 */
function onMouseDown(event: MouseEvent, view: EditorView): boolean {
  const target = event.target;
  if (!(target instanceof Element)) return false;

  const action = target.closest<HTMLElement>("[data-action]");
  if (action !== null) {
    const id = Number.parseInt(action.dataset["anchor"] ?? "", 10);
    const found = Number.isFinite(id) ? anchorById(view.state, id) : null;
    if (found !== null && action.dataset["action"] === "raw") {
      view.dispatch({
        effects: setCardUi.of({ id, ui: { ...found.rec.ui, raw: !found.rec.ui.raw } }),
      });
      return true;
    }
    return false;
  }

  // 06 §4.9: clicking rendered prose places the caret in the source line.
  const src = sourceOffsetForNode(target);
  if (src !== null) {
    view.dispatch({ selection: { anchor: src }, scrollIntoView: true });
    return true;
  }
  return false;
}

/** Reset the per-window counters. Tests bracket a keystroke with this. */
export { counters };

// ---------------------------------------------------------------------------
// Theme
// ---------------------------------------------------------------------------

const baseEditorTheme = EditorView.baseTheme({
  "&": {
    font: "var(--fs-code, 13px)/var(--lh-code, 20px) var(--font-mono)",
    color: "var(--text-body)",
    background: "var(--canvas)",
  },
  ".cm-gutters": {
    background: "var(--canvas)",
    border: "none",
    color: "var(--text-meta)",
    fontSize: "var(--fs-micro, 11px)",
  },
  ".cm-lineNumbers .cm-gutterElement": { color: "var(--text-disabled)" },
});

/**
 * Card chrome — 06 §4.6.
 *
 * The 2 px state rail is the strongest system element in the product: the gutter
 * glyph, the running hairline and the card rail carry the same colour for the
 * same block, which is what connects code to result without any box, shadow or
 * border. `data-display` is written by `applyCardStateFlags` on the frame the
 * status changed, so a stale card greys instantly and with no IPC.
 */
const cardTheme = EditorView.baseTheme({
  ".cm-resultCard": {
    display: "flex",
    gap: "var(--sp-8, 8px)",
    margin: "var(--card-gap-above, 8px) 0 var(--card-gap-below, 12px)",
    padding: "0",
    contain: "layout style paint",
  },
  ".cm-cardRail": {
    width: "var(--w-state-rail, 2px)",
    flex: "0 0 auto",
    background: "var(--state-never-run)",
  },
  ".cm-resultCard[data-display='current'] .cm-cardRail": { background: "var(--state-ok)" },
  ".cm-resultCard[data-display='current_unverifiable'] .cm-cardRail": {
    background: "var(--state-ok)",
  },
  ".cm-resultCard[data-display='running'] .cm-cardRail": { background: "var(--accent)" },
  ".cm-resultCard[data-display='failed'] .cm-cardRail, .cm-resultCard[data-display='broken'] .cm-cardRail":
    { background: "var(--state-failed)" },
  ".cm-resultCard[data-display='interrupted'] .cm-cardRail": {
    background: "var(--state-interrupted)",
  },
  // Stale: a DASHED amber rail and the body at .62, header at full opacity, so
  // you can still read what produced the numbers you are being told not to trust.
  ".cm-resultCard[data-display='stale'] .cm-cardRail": {
    background: "repeating-linear-gradient(var(--state-stale) 0 3px, transparent 3px 6px)",
  },
  ".cm-resultCard[data-display='stale'] .cm-cardBody": { opacity: ".62" },
  ".cm-cardInner": { flex: "1 1 auto", minWidth: "0" },
  ".cm-cardHeader": {
    display: "flex",
    alignItems: "center",
    gap: "var(--sp-6, 6px)",
    padding: "var(--card-pad-y, 8px) var(--card-pad-x, 12px) var(--sp-4, 4px)",
  },
  ".cm-cardEcho": {
    font: "var(--fs-code, 13px)/var(--lh-code, 20px) var(--font-mono)",
    color: "var(--text-body)",
    overflow: "hidden",
    textOverflow: "ellipsis",
    whiteSpace: "nowrap",
    flex: "1 1 auto",
  },
  ".cm-cardReadout": {
    font: "var(--fs-micro, 11px)/var(--lh-micro, 14px) var(--font-mono)",
    color: "var(--text-meta)",
    flex: "0 0 auto",
  },
  ".cm-cardBody": { padding: "0 var(--card-pad-x, 12px)", overflow: "auto" },
  // Streaming grows into a FIXED viewport: the card does not resize at all while
  // running, so nothing below it moves and no scroll compensation is needed.
  ".cm-cardStreaming": { overflowY: "auto", overscrollBehavior: "contain" },
  ".cm-cardRaw": { margin: "0", font: "inherit", whiteSpace: "pre" },
  ".cm-cardActions": {
    display: "flex",
    gap: "var(--sp-12, 12px)",
    padding: "var(--sp-4, 4px) var(--card-pad-x, 12px) var(--card-pad-y, 8px)",
  },
  ".cm-cardAction": {
    font: "var(--fs-micro, 11px)/var(--lh-micro, 14px) var(--font-sans)",
    color: "var(--text-meta)",
    background: "none",
    border: "none",
    padding: "0",
    cursor: "pointer",
  },
  ".cm-cardAction:hover": { color: "var(--text-body)", textDecoration: "underline" },
  // 06 §14.6 and §17: cards appear with zero animation. The single exception is
  // the one height change on completion, and even that is suppressed for
  // `prefers-reduced-motion`.
  "@media (prefers-reduced-motion: no-preference)": {
    ".cm-cardBody": {
      transition: "height var(--motion-collapse, 120ms) var(--motion-collapse-easing, ease)",
    },
  },
});
