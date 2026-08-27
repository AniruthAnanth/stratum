/**
 * PgUp / PgDn — 06 §9.1, and [U] 10.5 on this machine:
 *
 * > Press PgUp and Stata loads the last command you typed into the Command
 * > window. Press it again and Stata loads the line before that, and so on.
 * > PgDn goes in the opposite direction.
 *
 * **Unfiltered, and that is the acceptance bullet.** 06 §9.1: "Partial-line
 * prefix filtering is opt-in (`commandbar.historyPrefixMatch`, default off,
 * because Stata's is unfiltered and muscle memory is literal)." A user who has
 * typed `reg` and presses PgUp gets the previous command, not the previous
 * command starting with `reg` — because that is what twenty years of fingers
 * expect, and a nicer rule is still the wrong one.
 *
 * # Two cursors, and why
 *
 * The default path delegates to `state/history.ts`, whose cursor is shared by
 * every window (06 §13.1: the windows share one history). Prefix mode cannot:
 * its walk skips entries, so its position is not a position in the shared list
 * and pushing it into the shared cursor would make two windows with different
 * seeds fight over one integer. So prefix mode keeps a local cursor, and
 * switching modes resets both — which is also what stops a mode change from
 * teleporting the user to an unrelated command.
 */

import { createSignal } from "solid-js";
import { historyState, nextCommand, previousCommand, resetCursor } from "../state/history";

/**
 * `commandbar.historyPrefixMatch`. Off, per 06 §9.1.
 *
 * It lives here rather than in `state/settings.ts` because that file is W12's
 * and R0 forbids reaching across for it; the `Settings` interface is the right
 * long-term home and this is one line to move. Flagged in W16's return.
 */
const [prefixMatch, setPrefixMatch] = createSignal(false);

export const historyPrefixMatch = prefixMatch;

export function setHistoryPrefixMatch(on: boolean): void {
  if (on === prefixMatch()) return;
  setPrefixMatch(on);
  resetRecall();
}

export interface RecallCounters {
  /** PgUp presses served. */
  steps: number;
  /** Entries examined. On the unfiltered path this must equal `steps`. */
  scanned: number;
}

const ZERO: RecallCounters = { steps: 0, scanned: 0 };
export const recallCounters: RecallCounters = { ...ZERO };
export function resetRecallCounters(): void {
  Object.assign(recallCounters, ZERO);
}

// -- prefix mode's own state -------------------------------------------------

/** One past the newest entry means "not stepping", as in `state/history.ts`. */
let localCursor = 0;
let seed = "";
let draft = "";

/**
 * Abandon any in-progress walk. Called on every edit to the command text, so
 * PgUp always starts from the newest entry — Stata's behaviour, and the reason
 * a half-typed command is never lost to a stray PgUp.
 */
export function resetRecall(text = ""): void {
  resetCursor(text);
  localCursor = historyState.entries.length;
  seed = "";
  draft = text;
  recallCounters.scanned = 0;
}

/**
 * PgUp. `undefined` at the oldest entry, which the caller renders as "nothing
 * happened" rather than as an error — pressing PgUp at the top of a session is
 * a normal thing to do.
 */
export function recallPrevious(current: string): string | undefined {
  recallCounters.steps += 1;
  if (!prefixMatch()) {
    recallCounters.scanned += 1;
    return previousCommand(current);
  }

  const entries = historyState.entries;
  if (localCursor === entries.length) {
    seed = current;
    draft = current;
  }
  for (let i = localCursor - 1; i >= 0; i--) {
    recallCounters.scanned += 1;
    const entry = entries[i];
    if (entry === undefined) continue;
    if (entry.command.startsWith(seed)) {
      localCursor = i;
      return entry.command;
    }
  }
  return undefined;
}

/** PgDn. Past the newest entry it restores the draft the user was typing. */
export function recallNext(): string | undefined {
  recallCounters.steps += 1;
  if (!prefixMatch()) {
    recallCounters.scanned += 1;
    return nextCommand();
  }

  const entries = historyState.entries;
  for (let i = localCursor + 1; i < entries.length; i++) {
    recallCounters.scanned += 1;
    const entry = entries[i];
    if (entry === undefined) continue;
    if (entry.command.startsWith(seed)) {
      localCursor = i;
      return entry.command;
    }
  }
  if (localCursor >= entries.length) return undefined;
  localCursor = entries.length;
  return draft;
}
