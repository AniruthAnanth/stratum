/**
 * Command history — 06 §9.3, and the PgUp/PgDn contract of §9.1.
 *
 * "PgUp steps backward through command history, PgDn forward. Non-negotiable."
 * That sentence is why the cursor lives in a store rather than inside the
 * command bar: every window that can run commands gets its own command bar and
 * **they share one history** (§13.1), so the entries and the stepping rule
 * cannot be a component's private state.
 *
 * Stepping semantics are Stata's, and they are not the shell's: the cursor is
 * one-past-the-end when idle, PgUp walks toward older entries, PgDn walks back
 * and returns the draft the user was typing when it reaches the end again.
 */

import { createSignal } from "solid-js";
import { createStore, produce } from "solid-js/store";

export interface HistoryEntry {
  seq: number;
  command: string;
  /** Stata's `_rc`. 0 is success; the History pane colours a non-zero red. */
  rc: number;
  origin: "commandbar" | "editor" | "history" | "palette" | "menu";
}

/** 06 §15.2 caps the log, not the history; this cap is the pane's own. */
export const HISTORY_CAP = 20_000;

interface HistoryState {
  entries: HistoryEntry[];
  filter: string;
}

const [history, setHistory] = createStore<HistoryState>({ entries: [], filter: "" });

/** One-past-the-end means "not stepping". */
const [cursor, setCursor] = createSignal(0);
const [draft, setDraft] = createSignal("");

export const historyState = history;
export const historyCursor = cursor;

export function appendHistory(entry: HistoryEntry): void {
  setHistory(
    produce((s) => {
      s.entries.push(entry);
      if (s.entries.length > HISTORY_CAP) s.entries.splice(0, s.entries.length - HISTORY_CAP);
    }),
  );
  resetCursor();
}

export function setHistoryFilter(filter: string): void {
  setHistory("filter", filter);
}

/** The rows the pane draws: filtered, oldest first, as Stata's Review pane is. */
export function visibleHistory(): HistoryEntry[] {
  const needle = history.filter.trim().toLowerCase();
  if (needle === "") return history.entries;
  return history.entries.filter((e) => e.command.toLowerCase().includes(needle));
}

/**
 * The cursor sits one past the newest entry when the user is typing fresh text.
 * Any edit to the command bar resets it, so PgUp always starts from the newest.
 */
export function resetCursor(draftText = ""): void {
  setCursor(history.entries.length);
  setDraft(draftText);
}

/** PgUp. Returns the command to put in the bar, or `undefined` at the oldest entry. */
export function previousCommand(currentText: string): string | undefined {
  const at = cursor();
  if (at === history.entries.length) setDraft(currentText);
  if (at === 0) return undefined;
  const next = at - 1;
  setCursor(next);
  return history.entries[next]?.command;
}

/** PgDn. Past the newest entry it restores the draft the user had been typing. */
export function nextCommand(): string | undefined {
  const at = cursor();
  if (at >= history.entries.length) return undefined;
  const next = at + 1;
  setCursor(next);
  return next === history.entries.length ? draft() : history.entries[next]?.command;
}

/** Test seam. */
export function resetHistoryState(): void {
  setHistory({ entries: [], filter: "" });
  setCursor(0);
  setDraft("");
}
