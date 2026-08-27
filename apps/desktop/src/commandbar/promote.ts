/**
 * Exploratory → reproducible — spec §10, §11; 06 §9.3, §10.
 *
 * > **Add command to do-file** (§10) inserts the command at the caret of the
 * > active editor.
 * > **Add as new block** inserts it as its own block with a blank line above and
 * > below, and immediately attaches the result card that was just produced —
 * > promoting an exploratory command into a notebook block without re-running
 * > it.
 *
 * and, from History (spec §11, 06 §9.3):
 *
 * > History supports selecting commands and **Send to do-file**, inserting them
 * > as a commented block.
 *
 * # This is not a `.do` writer, and the distinction is ADR-010's
 *
 * ADR-010 permits exactly four code paths to **write a `.do` file**:
 * `doc_save`, `section_rename`, `section_move`, and an accepted AI diff, all
 * inside `stratum-workspace`. Nothing here writes a file. These verbs insert
 * text into an open buffer at the caret — the same edit the user could make by
 * typing, arriving through CodeMirror's ordinary transaction path, undoable
 * with one `Mod+Z`, and reaching disk only when `doc_save` later runs and
 * reproduces the recorded EOL and BOM. A15's fence is about who may serialise a
 * document; this is about what the user typed.
 *
 * # Why the inserter is a seam
 *
 * The default reaches W13's `activeEditor()`, which is exported for exactly
 * this. It is injectable so that the Do-file Editor in its own window (06 §9.6:
 * in Classic the editor is a separate window by default) can register itself as
 * the target, and so this whole path is testable with no CodeMirror at all.
 */

import { EditorSelection } from "@codemirror/state";
import type { EditorView } from "@codemirror/view";
import { blockAt, cardAnchor, stateSegmenter } from "../editor/blocks/blockField";
import { blockHash } from "../editor/blocks/segmenter";
import { activeEditor } from "../editor/commands";
import { anchorForBlock, beginRun, updateRun } from "../editor/results/anchor";
import type { DatasetStateId, ExecId, ResultId } from "../ipc/hand";

/** The result the ghost row is offering to carry into the document. */
export interface PromotedResult {
  readonly result: ResultId;
  readonly exec?: ExecId;
  readonly dataset?: DatasetStateId;
  readonly durationMs?: number;
  readonly rc: number;
}

export interface DoFileInserter {
  /** §10's "Add command to do-file". `false` when there is no target editor. */
  insertAtCaret(text: string): boolean;
  /** §10's "Add as new block", with the just-produced card attached. */
  insertBlock(text: string, result?: PromotedResult): boolean;
}

export interface PromoteCounters {
  /** Insertions performed. */
  inserts: number;
  /** Insertions refused because nothing was open to insert into. */
  refused: number;
  /** Cards attached to a promoted block WITHOUT re-running it. */
  attached: number;
}

const ZERO: PromoteCounters = { inserts: 0, refused: 0, attached: 0 };
export const promoteCounters: PromoteCounters = { ...ZERO };
export function resetPromoteCounters(): void {
  Object.assign(promoteCounters, ZERO);
}

/**
 * The editor-backed inserter.
 *
 * Both verbs go through one `dispatch`, and the caret is placed after the
 * inserted text with `EditorSelection.cursor` so the user can keep typing —
 * inserting at the caret and leaving the caret before the insertion is the
 * behaviour that makes people stop using the button.
 */
function editorInserter(): DoFileInserter {
  const target = (): EditorView | null => activeEditor();

  return {
    insertAtCaret(text) {
      const view = target();
      if (view === null) {
        promoteCounters.refused += 1;
        return false;
      }
      const at = view.state.selection.main.head;
      view.dispatch({
        changes: { from: at, to: view.state.selection.main.anchor, insert: text },
        selection: EditorSelection.cursor(at + text.length),
        userEvent: "input.paste.stratum.addToDoFile",
      });
      promoteCounters.inserts += 1;
      return true;
    },

    insertBlock(text, result) {
      const view = target();
      if (view === null) {
        promoteCounters.refused += 1;
        return false;
      }
      // A block is delimited by blank lines, so "its own block" means a blank
      // line each side — and only where there is not one already, or repeated
      // promotions accumulate a growing gap.
      const doc = view.state.doc;
      const at = doc.lineAt(view.state.selection.main.head).to;
      const needsAbove = doc.lineAt(at).text.trim() !== "";
      const after = at < doc.length ? doc.lineAt(at + 1).text.trim() : "";
      const insert = `${needsAbove ? "\n\n" : "\n"}${text}${after === "" ? "\n" : "\n\n"}`;
      const bodyAt = at + (needsAbove ? 2 : 1);

      view.dispatch({
        changes: { from: at, insert },
        selection: EditorSelection.cursor(bodyAt + text.length),
        userEvent: "input.paste.stratum.addAsBlock",
      });
      promoteCounters.inserts += 1;

      if (result !== undefined) attachResult(view, bodyAt, result);
      return true;
    },
  };
}

/**
 * Attach an already-produced envelope to the block that was just inserted.
 *
 * This is the half of "Add as new block" that makes it worth having: the card
 * appears under the new code without the engine running anything, so promoting
 * a forty-second `bootstrap` out of the Command window costs nothing. It needs
 * the segmenter, because a card is keyed by `(code_hash, ordinal)` and only the
 * segmenter allocates those; before wasm is ready the text still lands and the
 * card does not, which is visibly incomplete rather than silently wrong.
 */
function attachResult(view: EditorView, pos: number, result: PromotedResult): void {
  if (stateSegmenter(view.state) === null) return;
  const block = blockAt(view.state, pos);
  if (block === null) return;

  view.dispatch({
    effects: [
      beginRun.of({
        at: cardAnchor(view.state, block),
        executedHash: blockHash(block),
        executedOrdinal: block.hashOrdinal,
        label: view.state.doc.lineAt(block.from).text.trim(),
      }),
    ],
  });

  // The anchor id is allocated inside W13's field, so it is READ BACK from the
  // block rather than guessed — `anchorForBlock` is the same lookup the gutter
  // and the card use, and guessing an id is how a card attaches to the wrong
  // block. `blockAt` is re-run against the post-transaction state because the
  // dispatch above may have re-segmented.
  const attached = blockAt(view.state, pos);
  const record = attached === null ? null : anchorForBlock(view.state, attached);
  if (record === null) return;

  view.dispatch({
    effects: [
      updateRun.of({
        id: record.id,
        patch: {
          result: result.result,
          exec: result.exec,
          dataset: result.dataset,
          durationMs: result.durationMs,
          kernel: { state: result.rc === 0 ? "current" : "failed" },
          streaming: false,
        },
      }),
    ],
  });
  promoteCounters.attached += 1;
}

let inserter: DoFileInserter = editorInserter();

export function setDoFileInserter(next: DoFileInserter | null): void {
  inserter = next ?? editorInserter();
}

export function addToDoFile(command: string): boolean {
  return inserter.insertAtCaret(command.endsWith("\n") ? command : `${command}\n`);
}

export function addAsNewBlock(command: string, result?: PromotedResult): boolean {
  return inserter.insertBlock(command, result);
}

/**
 * §11's "Send to do-file", the multi-command form, with 06 §9.3's exact header:
 *
 * ```stata
 * // from History — 2026-08-21 14:03
 * use survey.dta, clear
 * drop if missing(income)
 * ```
 *
 * The timestamp is passed in rather than read from the clock, because a
 * function that reads `Date.now()` is one a golden test cannot pin — and this
 * text ends up in a `.do` that Scenario D compares byte for byte.
 */
export function historyBlockText(
  commands: readonly string[],
  when: string,
  commentOut = false,
): string {
  const body = commands.map((c) => (commentOut ? `// ${c}` : c));
  return [`// from History — ${when}`, ...body, ""].join("\n");
}

export function sendHistoryToDoFile(
  commands: readonly string[],
  when: string,
  commentOut = false,
): boolean {
  if (commands.length === 0) return false;
  return inserter.insertAtCaret(historyBlockText(commands, when, commentOut));
}
