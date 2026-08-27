/**
 * The Command window, as the rest of the app addresses it — spec §10, 06 §10.
 *
 * Four different panes put text into the Command window and none of them may
 * know how it is built:
 *
 *  * History, single click — "copy it to the Command window, **replacing the
 *    contents**" ([GSM] 2, verified against the shipped manual);
 *  * Variables, double click and the one-click paste column — "puts the selected
 *    variable **at the insertion point** in the Command window";
 *  * Properties — every edit "will create a command that appears in the Results
 *    and Command windows";
 *  * the keymap's `commandbar.focus`.
 *
 * So this module is a registry of one handle, and everything above talks to the
 * verbs rather than to a component. `replace` and `insertAtCaret` are separate
 * verbs precisely because the manual distinguishes them, and getting that wrong
 * is the kind of thing a twenty-year user notices in the first ten minutes.
 *
 * # The headless fallback
 *
 * With no bar mounted — under vitest, in Focus mode before `Mod+L` opens the
 * overlay, in a detached pane window — the verbs still work against a plain
 * string buffer. A `sendToCommand` that silently did nothing when the pane
 * happened to be closed would be a bug that only appears in one layout.
 */

import { createSignal } from "solid-js";

export interface CommandBarHandle {
  /** Current text. Multi-line when the user has used `Shift+Enter`. */
  text(): string;
  /** Replace the whole contents and put the caret at the end. */
  replace(text: string): void;
  /** Insert at the caret, as Stata's Variables window does. */
  insertAtCaret(text: string): void;
  /** Caret offset, in UTF-16 code units. */
  caret(): number;
  focus(): void;
  hasFocus(): boolean;
  /** `Esc` — "clears the Command window" ([U] 10.3). */
  clear(): void;
}

/** Headless: the verbs are total even when nothing is mounted. */
function headless(): CommandBarHandle {
  let buffer = "";
  let at = 0;
  return {
    text: () => buffer,
    replace(text) {
      buffer = text;
      at = text.length;
      bump((v) => v + 1);
    },
    insertAtCaret(text) {
      buffer = buffer.slice(0, at) + text + buffer.slice(at);
      at += text.length;
      bump((v) => v + 1);
    },
    caret: () => at,
    focus: () => {},
    hasFocus: () => false,
    clear() {
      buffer = "";
      at = 0;
      bump((v) => v + 1);
    },
  };
}

const fallback = headless();
let live: CommandBarHandle | undefined;

/** Bumped on every change, so a headless test can observe the buffer. */
const [revision, bump] = createSignal(0);
export const commandBarRevision = revision;

/** The mounted bar registers here and deregisters on unmount. */
export function setCommandBarHandle(handle: CommandBarHandle | undefined): void {
  live = handle;
  bump((v) => v + 1);
}

export function commandBar(): CommandBarHandle {
  return live ?? fallback;
}

export function isCommandBarMounted(): boolean {
  return live !== undefined;
}

// ---------------------------------------------------------------------------
// The verbs other panes use
// ---------------------------------------------------------------------------

/**
 * History's single click: replace, do not run.
 *
 * The gesture that resubmits is the double click, and conflating the two is the
 * one mistake in this pane that costs a user a re-run of a forty-second
 * `bootstrap` they did not ask for.
 */
export function sendToCommand(text: string): void {
  commandBar().replace(text);
  commandBar().focus();
}

/**
 * Variables' double click and one-click paste: insert at the caret.
 *
 * A space is added when the caret is not already after whitespace, because the
 * manual's own advice about F-key macros ("The space at the end of list is
 * important") is the same problem: a varlist pasted flush against the previous
 * token produces `summarizeprice`, and the user blames the pane.
 */
export function insertVarlist(names: readonly string[]): void {
  if (names.length === 0) return;
  const bar = commandBar();
  const before = bar.text().slice(0, bar.caret());
  const needsSpace = before.length > 0 && !/\s$/.test(before);
  bar.insertAtCaret(`${needsSpace ? " " : ""}${names.join(" ")}`);
  bar.focus();
}

export function focusCommandBar(): void {
  commandBar().focus();
}

/** Test seam. */
export function resetCommandBarHandle(): void {
  live = undefined;
  fallback.clear();
}
