/**
 * The Command window's verbs — 06 §5.4 ("all verbs are commands with ids").
 *
 * Every id here is already named by a shipped keymap. `resources/keymaps/
 * stata.json` binds `history.previous`, `history.next`, `commandbar.complete`
 * and `stata.functionKey`; `modern.json` binds `commandbar.focus`. Until this
 * module registers them those keystrokes resolve to nothing and fall through to
 * the platform — which is W12's design and why the shell could ship first, and
 * also why this file is the moment the Stata preset starts being the Stata
 * preset.
 *
 * The palette reads the same registry, so every one of these is also a
 * searchable verb rather than a keystroke somebody has to know about.
 */

import { type CommandDescriptor, registerCommands } from "../keys/registry";
import { completeAt } from "./complete";
import { functionKeyAction } from "./fkeys";
import { commandBar, focusCommandBar, sendToCommand } from "./handle";
import { requestBreak } from "./interrupt";
import { addAsNewBlock, addToDoFile } from "./promote";
import { historyPrefixMatch, recallNext, recallPrevious, setHistoryPrefixMatch } from "./recall";
import { lastSubmission, submitCommand } from "./submit";

const asRecord = (args: unknown): Record<string, unknown> =>
  typeof args === "object" && args !== null ? (args as Record<string, unknown>) : {};

/**
 * The verbs.
 *
 * `history.previous` / `history.next` act on the Command window whether or not
 * it is focused, because the keymap's `when: commandBarFocus` already decides
 * that for the keystroke — and the palette entry must still work, since a user
 * who found "Previous command" in the palette has by definition not got focus
 * in the Command window.
 */
const COMMAND_BAR_COMMANDS: readonly CommandDescriptor[] = [
  {
    id: "commandbar.focus",
    title: "Focus the Command window",
    category: "Command",
    run: () => focusCommandBar(),
  },
  {
    id: "commandbar.submit",
    title: "Submit the command",
    category: "Command",
    run() {
      const bar = commandBar();
      const text = bar.text();
      if (text.trim() === "") return;
      bar.clear();
      void submitCommand(text, "commandbar");
    },
  },
  {
    id: "commandbar.clear",
    title: "Clear the Command window",
    category: "Command",
    // [U] 10.3: "Esc … Clears Command window."
    run: () => commandBar().clear(),
  },
  {
    id: "history.previous",
    title: "Previous command",
    category: "Command",
    run() {
      const bar = commandBar();
      const previous = recallPrevious(bar.text());
      if (previous !== undefined) bar.replace(previous);
    },
  },
  {
    id: "history.next",
    title: "Next command",
    category: "Command",
    run() {
      const bar = commandBar();
      const next = recallNext();
      if (next !== undefined) bar.replace(next);
    },
  },
  {
    id: "commandbar.complete",
    title: "Complete variable or filename",
    category: "Command",
    // The mounted pane serves Tab through its own CM6 keymap so the caret
    // arithmetic stays inside the view; this path is the palette's and the
    // headless one, and it uses the same `completeAt`.
    run() {
      const bar = commandBar();
      const outcome = completeAt(bar.text(), bar.caret());
      if (outcome === null || outcome.insert.length <= outcome.prefix.length) return;
      const text = bar.text();
      bar.replace(text.slice(0, outcome.from) + outcome.insert + text.slice(outcome.to));
    },
  },
  {
    id: "commandbar.togglePrefixMatch",
    title: "Match the typed prefix when stepping through history",
    category: "Command",
    // 06 §9.1: opt-in, default OFF, "because Stata's is unfiltered and muscle
    // memory is literal". The verb exists so it is discoverable; the default
    // is what the acceptance bullet is about.
    run: () => setHistoryPrefixMatch(!historyPrefixMatch()),
  },
  {
    id: "stata.functionKey",
    title: "Insert an F-key macro",
    category: "Command",
    run(args) {
      const n = asRecord(args)["n"];
      if (typeof n !== "number") return;
      const action = functionKeyAction(n);
      if (action === undefined) return;
      const bar = commandBar();
      bar.insertAtCaret(action.insert);
      if (action.submit) {
        const text = bar.text();
        bar.clear();
        void submitCommand(text, "commandbar");
      }
    },
  },
  {
    id: "commandbar.addToDoFile",
    title: "Add command to do-file",
    category: "Command",
    // Spec §10's named action. Takes the last submission when the ghost row is
    // gone, and the current text when it is not — which is what a user means by
    // "add this command" in both cases.
    run() {
      const pending = commandBar().text().trim();
      const command = pending === "" ? lastSubmission()?.text : pending;
      if (command !== undefined) addToDoFile(command);
    },
  },
  {
    id: "commandbar.addAsNewBlock",
    title: "Add command to do-file as a new block",
    category: "Command",
    run() {
      const last = lastSubmission();
      const pending = commandBar().text().trim();
      if (pending !== "") {
        addAsNewBlock(pending);
        return;
      }
      if (last !== undefined) addAsNewBlock(last.text);
    },
  },
  {
    id: "commandbar.break",
    title: "Break",
    category: "Command",
    // The Command window's own Break. See `interrupt.ts` for why `run.break`
    // cannot be the only one in the Classic layout.
    run: () => requestBreak(),
  },
  {
    id: "commandbar.sendText",
    title: "Send text to the Command window",
    category: "Command",
    // The programmatic form of History's single click. Panes call
    // `sendToCommand` directly; this exists so the palette, a native menu and
    // the e2e harness all have an id to name.
    run(args) {
      const text = asRecord(args)["text"];
      if (typeof text === "string") sendToCommand(text);
    },
  },
];

export function registerCommandBarCommands(): () => void {
  return registerCommands(COMMAND_BAR_COMMANDS);
}

export { COMMAND_BAR_COMMANDS };
