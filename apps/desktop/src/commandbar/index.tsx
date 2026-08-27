/**
 * The Command window — spec §10, 06 §9.1 and §10.
 *
 * One import surface, so the shell wires the whole thing with two calls:
 *
 * ```ts
 * const disposePane = registerCommandBarPane({ segmenter });
 * const disposeVerbs = registerCommandBarCommands();
 * ```
 *
 * The bar exists in **every** layout (spec §10). Classic docks it as the
 * Command pane — the one dock component that is not a `PaneId` — Modern puts it
 * at the foot of the editor, and Focus opens it as an overlay on `Mod+L`. The
 * variant is presentation; the verbs, the history stepping, the completion and
 * the promotion path are the same objects in all three.
 */

import { render } from "solid-js/web";
import { registerPane } from "../dock/panes";
import type { StratumSegmenter } from "../wasm/types";
import { CommandBar, type CommandBarProps } from "./view";

export { CommandBar, GHOST_MS, MAX_ROWS, MIN_ROWS, type CommandBarProps } from "./view";
export { registerCommandBarCommands, COMMAND_BAR_COMMANDS } from "./commands";
export {
  commandBar,
  commandBarRevision,
  focusCommandBar,
  insertVarlist,
  isCommandBarMounted,
  resetCommandBarHandle,
  sendToCommand,
  setCommandBarHandle,
  type CommandBarHandle,
} from "./handle";
export {
  lastSubmission,
  recordedSubmissions,
  resetSubmitCounters,
  resetSubmitState,
  setSubmitSink,
  submitAll,
  submitCommand,
  submitCounters,
  type SubmitOrigin,
  type SubmitOutcome,
  type SubmitRequest,
  type SubmitSink,
} from "./submit";
export {
  historyPrefixMatch,
  recallNext,
  recallPrevious,
  recallCounters,
  resetRecall,
  resetRecallCounters,
  setHistoryPrefixMatch,
} from "./recall";
export {
  completeAt,
  completeCounters,
  longestCommonPrefix,
  resetCompleteCounters,
  setCandidateSource,
  tokenAtCaret,
  type CandidateSource,
  type CompletionOutcome,
  type CompletionTarget,
} from "./complete";
export {
  functionKeyAction,
  functionKeyText,
  resetFunctionKeys,
  setFunctionKey,
} from "./fkeys";
export {
  addAsNewBlock,
  addToDoFile,
  historyBlockText,
  promoteCounters,
  resetPromoteCounters,
  sendHistoryToDoFile,
  setDoFileInserter,
  type DoFileInserter,
  type PromotedResult,
} from "./promote";
export {
  interruptCounters,
  recordedBreaks,
  requestBreak,
  resetInterruptCounters,
  resetInterruptState,
  setInterruptSink,
  type BreakRequest,
  type InterruptSink,
} from "./interrupt";

export interface CommandBarPaneOptions extends CommandBarProps {
  segmenter?: StratumSegmenter;
}

/** Registers the Command pane with W12's dock. Returns the disposer. */
export function registerCommandBarPane(options: CommandBarPaneOptions = {}): () => void {
  return registerPane(
    "commandbar",
    (host, register) => {
      register(render(() => <CommandBar {...options} />, host));
    },
    "Command",
  );
}
