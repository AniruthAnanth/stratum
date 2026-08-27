/**
 * Run requests — 06 §5.4 (the verbs) and §5.5 (race safety).
 *
 * # What a run is, from the editor's side
 *
 * Resolve the verb to a list of blocks, take each block's `code_hash` as the
 * segmenter reports it right now, create an anchor per block so the gutter and
 * the card have somewhere to live, and hand the request to the sink. The sink is
 * the IPC boundary and it is injected: this unit ships with a recording sink so
 * the whole editor is drivable — and testable — with no engine behind it, and
 * W17 installs the real one.
 *
 * # Why the hash travels with the request
 *
 * 06 §5.5: the kernel re-segments the text it was sent and compares hashes;
 * a mismatch answers `BlockMismatch` and the UI re-syncs rather than executing
 * text the user cannot see. That check is also the wasm/native divergence alarm,
 * so the hash is not optional book-keeping — it is the only thing standing
 * between a segmenter bug and a researcher running code they never wrote.
 *
 * # No document writes
 *
 * Running never edits the document. Not the caret-advance of
 * `run.blockAndAdvance` (a selection change), not the queueing, not the
 * anchoring. `counters.documentWrites` stays at 0 across the whole run suite.
 */

import type { EditorState } from "@codemirror/state";
import { EditorSelection } from "@codemirror/state";
import { EditorView } from "@codemirror/view";
import type { DocumentId } from "../../ipc/hand";
import { beginRun } from "../results/anchor";
import { allBlocks, blockAt, blockAtCursor, cardAnchor, stateSegmenter } from "./blockField";
import type { Block } from "./segmenter";
import { blockHash, counters } from "./segmenter";

/** 06 §5.4's verb list, verbatim. Every one is a command id in `commands.ts`. */
export type RunVerb =
  | "run.block"
  | "run.blockAndAdvance"
  | "run.selection"
  | "run.line"
  | "run.statement"
  | "run.section"
  | "run.above"
  | "run.below"
  | "run.fromHere"
  | "run.toCursor"
  | "run.file"
  | "run.fileClean"
  | "run.entryPoint"
  | "run.allStale"
  | "run.break";

/** Interactive against current state, or clean in a fresh environment (§15). */
export type RunMode = "interactive" | "clean";

/** Where the request came from. `OnEditorRun` inline mode keys off this. */
export type RunOrigin = "editor" | "commandbar" | "history" | "palette";

/** One block, as 06 §5.5 puts it on the wire. */
export interface RunBlockRef {
  /** Executable extent start, UTF-16 code units in the document we sent. */
  readonly from: number;
  /** Executable extent end. */
  readonly to: number;
  /** The hash the kernel must agree with. */
  readonly code_hash: string;
  /** Occurrence index of that hash — the second half of the client key. */
  readonly ordinal: number;
}

export interface RunRequest {
  readonly doc: DocumentId | undefined;
  readonly verb: RunVerb;
  readonly blocks: readonly RunBlockRef[];
  readonly mode: RunMode;
  readonly origin: RunOrigin;
}

/** The IPC boundary. W17 installs the real one; the default records. */
export type RunSink = (request: RunRequest) => void | Promise<void>;

const recorded: RunRequest[] = [];

let sink: RunSink = (request) => {
  recorded.push(request);
};

export function setRunSink(next: RunSink | null): void {
  sink = next ?? ((request) => void recorded.push(request));
}

/** What the default sink saw. The editor's whole run path is testable from here. */
export function recordedRuns(): readonly RunRequest[] {
  return recorded;
}

/** Test seam. */
export function resetRuns(): void {
  recorded.length = 0;
}

/** Which document the editor is running. Set by the host when a file is opened. */
let currentDoc: DocumentId | undefined;

export function setRunDocument(doc: DocumentId | undefined): void {
  currentDoc = doc;
}

// ---------------------------------------------------------------------------
// Verb → blocks
// ---------------------------------------------------------------------------

/**
 * The blocks a verb covers, in document order.
 *
 * Non-executable regions — comment runs, blank space, section markers — are
 * dropped here rather than at the call sites, so "run everything above" cannot
 * accidentally ask the kernel to execute a comment.
 */
export function resolveRun(state: EditorState, verb: RunVerb): Block[] {
  const blocks = allBlocks(state);
  const runnable = (list: readonly Block[]): Block[] => list.filter((b) => b.executable);
  const main = state.selection.main;

  switch (verb) {
    case "run.block":
    case "run.blockAndAdvance": {
      const block = blockAtCursor(state);
      return block === null ? [] : runnable([block]);
    }
    case "run.selection": {
      if (main.empty) {
        const block = blockAtCursor(state);
        return block === null ? [] : runnable([block]);
      }
      return runnable(blocks.filter((b) => b.outerFrom <= main.to && b.outerTo >= main.from));
    }
    case "run.line":
    case "run.statement": {
      // The distinction is the segmenter's: `run.line` is the block containing
      // the caret's line, which for a `///`-continued command is the whole
      // command — running half of a continued statement is never what a user
      // means, and Stata itself would refuse it.
      const block = blockAt(state, state.doc.lineAt(main.head).from);
      return block === null ? [] : runnable([block]);
    }
    case "run.section": {
      const seg = stateSegmenter(state);
      if (seg === null) return [];
      const sections = seg.sections();
      const here = sections.find((s) => s.from <= main.head && main.head <= s.to);
      if (here === undefined) return runnable(blocks);
      return runnable(blocks.filter((b) => b.outerFrom >= here.from && b.outerTo <= here.to));
    }
    case "run.above": {
      const block = blockAtCursor(state);
      if (block === null) return [];
      return runnable(blocks.filter((b) => b.index < block.index));
    }
    case "run.below": {
      const block = blockAtCursor(state);
      if (block === null) return [];
      return runnable(blocks.filter((b) => b.index > block.index));
    }
    case "run.fromHere": {
      const block = blockAtCursor(state);
      if (block === null) return [];
      return runnable(blocks.filter((b) => b.index >= block.index));
    }
    case "run.toCursor": {
      const block = blockAtCursor(state);
      if (block === null) return [];
      return runnable(blocks.filter((b) => b.index <= block.index));
    }
    case "run.file":
    case "run.fileClean":
    case "run.entryPoint":
      return runnable(blocks);
    case "run.allStale":
      // Which blocks are stale is a question about anchors, so the caller passes
      // the list explicitly through `submitRun`. Answering it here would put a
      // second staleness rule in the codebase.
      return [];
    case "run.break":
      return [];
  }
}

// ---------------------------------------------------------------------------
// Submit
// ---------------------------------------------------------------------------

export interface SubmitOptions {
  readonly mode?: RunMode;
  readonly origin?: RunOrigin;
  /** Overrides `resolveRun`, for `run.allStale` and the gutter's click targets. */
  readonly blocks?: readonly Block[];
}

/**
 * Submit a run and open an anchor per block, in ONE transaction.
 *
 * One transaction matters: 06 §15.1 budgets `Mod+Enter` → `▶ running` glyph on
 * screen at 16 ms *independent of the kernel*, and that is only achievable if
 * the glyph state is decided locally and painted in the same frame as the
 * keystroke. The engine is told afterwards and its answer patches the record.
 */
export function submitRun(view: EditorView, verb: RunVerb, options: SubmitOptions = {}): boolean {
  const blocks = options.blocks ?? resolveRun(view.state, verb);
  if (verb === "run.break") {
    counters.ipcCalls += 1;
    void sink({
      doc: currentDoc,
      verb,
      blocks: [],
      mode: "interactive",
      origin: options.origin ?? "editor",
    });
    return true;
  }
  if (blocks.length === 0) return false;

  const refs: RunBlockRef[] = [];
  const effects = blocks.map((block) => {
    const hash = blockHash(block);
    refs.push({ from: block.from, to: block.to, code_hash: hash, ordinal: block.hashOrdinal });
    return beginRun.of({
      at: cardAnchor(view.state, block),
      executedHash: hash,
      executedOrdinal: block.hashOrdinal,
      label: firstLine(view.state, block),
    });
  });

  view.dispatch({ effects });

  counters.ipcCalls += 1;
  void sink({
    doc: currentDoc,
    verb,
    blocks: refs,
    mode: options.mode ?? (verb === "run.fileClean" ? "clean" : "interactive"),
    origin: options.origin ?? "editor",
  });

  if (verb === "run.blockAndAdvance") advance(view, blocks[blocks.length - 1] as Block);
  return true;
}

/**
 * Move the caret to the first line of the next RUNNABLE block and put it at 30 %
 * of the viewport — 06 §5.4, matching the north-star flow in spec §36.
 *
 * A selection change, never a document change.
 */
function advance(view: EditorView, from: Block): void {
  const seg = stateSegmenter(view.state);
  const next = seg?.nextRunnable(from) ?? null;
  if (next === null) return;
  view.dispatch({
    selection: EditorSelection.cursor(next.from),
    scrollIntoView: true,
    effects: EditorView.scrollIntoView(next.from, { y: "start", yMargin: viewportThird(view) }),
  });
}

function viewportThird(view: EditorView): number {
  const height = view.scrollDOM.clientHeight;
  return height > 0 ? Math.round(height * 0.3) : 0;
}

/** The echoed command a card shows, and the label an orphan keeps. */
function firstLine(state: EditorState, block: Block): string {
  const line = state.doc.lineAt(block.from);
  const end = Math.min(line.to, block.to);
  return state.doc.sliceString(block.from, Math.max(block.from, end)).trim();
}
