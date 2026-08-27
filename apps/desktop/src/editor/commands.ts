/**
 * Every editor verb — 06 §5.4 (run), §4.8 (sections), §4.6 (cards), §4.9 (view).
 *
 * All of them are `CommandDescriptor`s in W12's registry, because that is what
 * makes a keystroke, a palette entry, a gutter click and a native menu item the
 * same thing. A keystroke bound to a verb nobody registered does nothing and
 * falls through to the platform, which is why this module registering late is
 * safe.
 *
 * # The write fence (A15)
 *
 * **This unit does not write the document.** Only `doc_save`, `section_rename`,
 * `section_move` and an accepted AI diff may, and all four go through
 * `stratum-workspace` (W26), gated by `assert_comment_only` and
 * `assert_statement_partition_preserved`. So every verb here that would change
 * text delegates to the {@link SectionWriter} seam and reports itself
 * unavailable when W26 has not installed one. {@link writeDocument} is the
 * single choke point, it counts what passes through it, and
 * `editor.doc.test.ts` greps this unit's own source to prove there is no second
 * one.
 */

import { EditorSelection } from "@codemirror/state";
import type { EditorView } from "@codemirror/view";
import { type CommandDescriptor, registerCommands } from "../keys/registry";
import { blockAtCursor } from "./blocks/blockField";
import { notifyStatusChanged } from "./blocks/blockGutter";
import type { RunVerb } from "./blocks/run";
import { resolveRun, submitRun } from "./blocks/run";
import { counters } from "./blocks/segmenter";
import { helpTopicAt, openHelpAt } from "./lang/help";
import { anchorForBlock, anchorsIn, detachAnchor, setCardUi } from "./results/anchor";
import { toggleCollapsed } from "./results/collapse";
import { orphanResults } from "./results/orphans";
import { displayStatus } from "./results/widget";
import { toggleDocumentView } from "./sections/docview";
import { toggleFoldAt } from "./sections/fold";
import { sectionAt, sectionWriter, sections } from "./sections/markers";

// ---------------------------------------------------------------------------
// Which editor a verb acts on
// ---------------------------------------------------------------------------

let active: EditorView | null = null;

/** The focused editor. `setup.ts` keeps this current from the focus handler. */
export function setActiveEditor(view: EditorView | null): void {
  active = view;
}

export function activeEditor(): EditorView | null {
  return active;
}

// ---------------------------------------------------------------------------
// The one sanctioned write path
// ---------------------------------------------------------------------------

/** The four writers of A15, and nothing else, ever. */
export type WriteReason = "doc_save" | "section_rename" | "section_move" | "ai_diff_accepted";

/**
 * The single place in `editor/**` that may change document text.
 *
 * It does not perform the write itself — it cannot: the gated writers live in
 * W26. It exists so that "does the editor write the document" is a question with
 * one answer and one call site, and so that the counter is impossible to bypass
 * without failing the source check in `editor.doc.test.ts`.
 */
export function writeDocument(
  reason: WriteReason,
  perform: () => Promise<boolean>,
): Promise<boolean> {
  counters.documentWrites += 1;
  void reason;
  return perform();
}

// ---------------------------------------------------------------------------
// Verbs
// ---------------------------------------------------------------------------

const RUN_VERBS: readonly RunVerb[] = [
  "run.block",
  "run.blockAndAdvance",
  "run.selection",
  "run.line",
  "run.statement",
  "run.section",
  "run.above",
  "run.below",
  "run.fromHere",
  "run.toCursor",
  "run.file",
  "run.fileClean",
  "run.entryPoint",
  "run.break",
];

const RUN_TITLES: Readonly<Record<RunVerb, string>> = {
  "run.block": "Run block",
  "run.blockAndAdvance": "Run block and advance",
  "run.selection": "Run selection",
  "run.line": "Run line",
  "run.statement": "Run statement",
  "run.section": "Run section",
  "run.above": "Run everything above",
  "run.below": "Run everything below",
  "run.fromHere": "Run from here",
  "run.toCursor": "Run to cursor",
  "run.file": "Run file",
  "run.fileClean": "Run do-file from clean state",
  "run.entryPoint": "Run project entry point",
  "run.allStale": "Run all stale blocks",
  "run.break": "Break",
};

function runCommands(): CommandDescriptor[] {
  return RUN_VERBS.map((verb) => ({
    id: verb,
    title: RUN_TITLES[verb],
    category: "Run",
    enabled: () => active !== null,
    run() {
      const view = active;
      if (view === null) return;
      submitRun(view, verb);
    },
  }));
}

/**
 * `run.allStale` — 06 §5.3, in document order.
 *
 * Which blocks are stale is asked of {@link displayStatus}, the same function the
 * gutter and the card rail use. A second staleness rule here is exactly how a
 * top bar comes to say `3 stale` while the gutter shows four.
 */
const runAllStale: CommandDescriptor = {
  id: "run.allStale",
  title: RUN_TITLES["run.allStale"],
  category: "Run",
  enabled: () => active !== null,
  run() {
    const view = active;
    if (view === null) return;
    const blocks = resolveRun(view.state, "run.file").filter((block) => {
      const rec = anchorForBlock(view.state, block);
      return displayStatus(rec, block) === "stale";
    });
    if (blocks.length > 0) submitRun(view, "run.fromHere", { blocks });
  },
};

function sectionCommands(): CommandDescriptor[] {
  const withSection = (fn: (view: EditorView, at: number) => void): (() => void) => {
    return () => {
      const view = active;
      if (view === null) return;
      fn(view, view.state.selection.main.head);
    };
  };

  return [
    {
      id: "section.run",
      title: "Run section",
      category: "Section",
      enabled: () => active !== null,
      run: withSection((view) => {
        submitRun(view, "run.section");
      }),
    },
    {
      id: "section.runAllAbove",
      title: "Run all sections above",
      category: "Section",
      enabled: () => active !== null,
      run: withSection((view, at) => {
        const here = sectionAt(view.state, at);
        if (here === null) return;
        const blocks = resolveRun(view.state, "run.file").filter((b) => b.outerTo <= here.from);
        if (blocks.length > 0) submitRun(view, "run.above", { blocks });
      }),
    },
    {
      id: "section.runAllBelow",
      title: "Run all sections below",
      category: "Section",
      enabled: () => active !== null,
      run: withSection((view, at) => {
        const here = sectionAt(view.state, at);
        if (here === null) return;
        const blocks = resolveRun(view.state, "run.file").filter((b) => b.outerFrom >= here.to);
        if (blocks.length > 0) submitRun(view, "run.below", { blocks });
      }),
    },
    {
      id: "section.collapse",
      title: "Collapse section",
      category: "Section",
      enabled: () => active !== null,
      run: withSection((view, at) => {
        toggleFoldAt(view, at);
      }),
    },
    {
      id: "section.clearOutput",
      title: "Clear section output",
      category: "Section",
      enabled: () => active !== null,
      // Sidecar only. 06 §4.8: clearing output must never touch the document,
      // and the result itself survives in the scrollback either way (§6.1).
      run: withSection((view, at) => {
        const here = sectionAt(view.state, at);
        if (here === null) return;
        const effects = anchorsIn(view.state, here.from, here.to).map((a) =>
          detachAnchor.of(a.rec.id),
        );
        if (effects.length > 0) view.dispatch({ effects });
      }),
    },
    {
      id: "section.collapseOutput",
      title: "Collapse section output",
      category: "Section",
      enabled: () => active !== null,
      run: withSection((view, at) => {
        const here = sectionAt(view.state, at);
        if (here === null) return;
        const effects = anchorsIn(view.state, here.from, here.to).map((a) =>
          setCardUi.of({ id: a.rec.id, ui: { ...a.rec.ui, collapsed: true } }),
        );
        if (effects.length > 0) view.dispatch({ effects });
      }),
    },
    // The three that change TEXT. They are W26's, by A15, and are registered
    // here only so the palette and the section menu can name them; every one of
    // them refuses when no gated writer is installed rather than reaching for
    // `view.dispatch`.
    {
      id: "section.rename",
      title: "Rename section",
      category: "Section",
      enabled: () => active !== null && sectionWriter() !== null,
      run(args) {
        const view = active;
        const writer = sectionWriter();
        if (view === null || writer === null) return;
        const here = sectionAt(view.state, view.state.selection.main.head);
        const title = typeof args === "string" ? args : undefined;
        if (here === null || title === undefined) return;
        void writeDocument("section_rename", () => writer.rename(view, here, title));
      },
    },
    {
      id: "section.moveUp",
      title: "Move section up",
      category: "Section",
      enabled: () => active !== null && sectionWriter() !== null,
      run: () => moveSection(-1),
    },
    {
      id: "section.moveDown",
      title: "Move section down",
      category: "Section",
      enabled: () => active !== null && sectionWriter() !== null,
      run: () => moveSection(1),
    },
    {
      id: "section.insertAbove",
      title: "Insert section above",
      category: "Section",
      enabled: () => active !== null && sectionWriter() !== null,
      run: (args) => insertSection(args, -1),
    },
    {
      id: "section.insertBelow",
      title: "Insert section below",
      category: "Section",
      enabled: () => active !== null && sectionWriter() !== null,
      run: (args) => insertSection(args, 1),
    },
    {
      id: "section.goto",
      title: "Go to section",
      category: "Section",
      enabled: () => active !== null,
      run(args) {
        const view = active;
        if (view === null) return;
        const id =
          typeof args === "object" && args !== null ? (args as { id?: number }).id : undefined;
        const target = sections(view.state).find((s) => s.id === id);
        if (target === undefined) return;
        view.dispatch({
          selection: EditorSelection.cursor(target.from),
          scrollIntoView: true,
        });
        view.focus();
      },
    },
  ];
}

function insertSection(args: unknown, side: -1 | 1): void {
  const view = active;
  const writer = sectionWriter();
  if (view === null || writer === null) return;
  const here = sectionAt(view.state, view.state.selection.main.head);
  const line = view.state.doc.lineAt(view.state.selection.main.head);
  const at = side < 0 ? (here === null ? line.from : here.from) : line.to;
  const title = typeof args === "string" ? args : "New section";
  void writeDocument("section_move", () => writer.insert(view, at, title));
}

function moveSection(direction: -1 | 1): void {
  const view = active;
  const writer = sectionWriter();
  if (view === null || writer === null) return;
  const here = sectionAt(view.state, view.state.selection.main.head);
  if (here === null) return;
  void writeDocument("section_move", () => writer.move(view, here, direction));
}

function cardCommands(): CommandDescriptor[] {
  return [
    {
      id: "card.toggleCollapse",
      title: "Collapse output",
      category: "Results",
      enabled: () => active !== null,
      run() {
        const view = active;
        if (view === null) return;
        const block = blockAtCursor(view.state);
        const rec = block === null ? null : anchorForBlock(view.state, block);
        if (block === null || rec === null) return;
        // Collapse INTENT is durable and keyed by the code hash (sidecar); the
        // per-card flag is the view. Both move, neither writes the document.
        const collapsed = toggleCollapsed(rec.executedHash);
        view.dispatch({
          effects: setCardUi.of({ id: rec.id, ui: { ...rec.ui, collapsed } }),
        });
      },
    },
    {
      id: "card.clearOutput",
      title: "Clear block output",
      category: "Results",
      enabled: () => active !== null,
      run() {
        const view = active;
        if (view === null) return;
        const block = blockAtCursor(view.state);
        const rec = block === null ? null : anchorForBlock(view.state, block);
        if (rec === null) return;
        view.dispatch({ effects: detachAnchor.of(rec.id) });
      },
    },
    {
      id: "results.showDetached",
      title: "Show detached results",
      category: "Results",
      enabled: () => orphanResults().length > 0,
      run() {
        // The list is the product; presenting it is W14's Results pane. Keeping
        // the verb here means the palette can offer it before that pane exists.
      },
    },
  ];
}

function viewCommands(): CommandDescriptor[] {
  return [
    {
      id: "editor.toggleFold",
      title: "Fold or unfold",
      category: "View",
      enabled: () => active !== null,
      run() {
        const view = active;
        if (view === null) return;
        toggleFoldAt(view, view.state.selection.main.head);
      },
    },
    {
      id: "editor.toggleDocView",
      title: "Toggle Document View",
      category: "View",
      enabled: () => active !== null,
      run(args) {
        const view = active;
        if (view === null) return;
        const on =
          typeof args === "object" && args !== null ? (args as { on?: boolean }).on : undefined;
        toggleDocumentView(view, on ?? true);
      },
    },
    {
      id: "editor.help",
      title: "Help for the command at the cursor",
      category: "Help",
      enabled: () => {
        const view = active;
        return view !== null && helpTopicAt(view.state, view.state.selection.main.head) !== null;
      },
      run() {
        const view = active;
        if (view === null) return;
        openHelpAt(view.state, view.state.selection.main.head);
      },
    },
    {
      id: "editor.refreshStatuses",
      title: "Refresh block statuses",
      category: "Run",
      enabled: () => active !== null,
      run() {
        if (active !== null) notifyStatusChanged(active);
      },
    },
  ];
}

/** Everything, registered in one call. Returns the disposer W12's registry wants. */
export function registerEditorCommands(): () => void {
  return registerCommands([
    ...runCommands(),
    runAllStale,
    ...sectionCommands(),
    ...cardCommands(),
    ...viewCommands(),
  ]);
}
