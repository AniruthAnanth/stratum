/**
 * Submitting a command — spec §10, 06 §9.1, §10.
 *
 * > Commands submitted here are echoed into Results with a leading `. `, land in
 * > History with their `_rc`, and get an inline `↑ Add to do-file` affordance.
 *
 * All three of those are consequences of one call, and they happen for every
 * origin: the Command pane, a History double-click, the Variables pane's
 * `keep`/`drop`, and every Properties edit. That is what makes "working by
 * right-clicking is just like working directly in the Command window" ([GSM] 2)
 * true here rather than aspirational — there is one submission path and the
 * panes do not have private ones.
 *
 * # The sink
 *
 * The IPC boundary is injected, exactly as W13 injects its `RunSink`: this unit
 * ships with a recording sink so the whole Classic layout is drivable and
 * testable with no engine behind it, and W17 installs the real one, which
 * issues `exec_submit` with `RunIntent::CommandBar { text }` (CONTRACTS §6).
 * There is deliberately no second fake engine here — W07's mock replays the
 * golden log over the real transport and is what the sink should be pointed at.
 */

import { type HistoryEntry, appendHistory, historyState } from "../state/history";

export type SubmitOrigin = HistoryEntry["origin"];

export interface SubmitRequest {
  readonly text: string;
  readonly origin: SubmitOrigin;
}

export interface SubmitOutcome {
  /** Stata's `_rc`. 0 is success; History colours a non-zero red. */
  readonly rc: number;
  /** Wall time, RECORDED for the ghost row. Never asserted (ADR-017). */
  readonly durationMs?: number;
}

export type SubmitSink = (request: SubmitRequest) => Promise<SubmitOutcome> | SubmitOutcome;

export interface SubmitCounters {
  /** Commands handed to the sink. */
  submissions: number;
  /** Rows appended to History. Must equal `submissions` minus empty lines. */
  historyAppends: number;
  /** Submissions refused because the text was blank. */
  blanks: number;
}

const ZERO: SubmitCounters = { submissions: 0, historyAppends: 0, blanks: 0 };
export const submitCounters: SubmitCounters = { ...ZERO };
export function resetSubmitCounters(): void {
  Object.assign(submitCounters, ZERO);
}

const recorded: SubmitRequest[] = [];

const recordingSink: SubmitSink = (request) => {
  recorded.push(request);
  return { rc: 0 };
};

let sink: SubmitSink = recordingSink;

export function setSubmitSink(next: SubmitSink | null): void {
  sink = next ?? recordingSink;
}

/** What the default sink saw. The whole Classic path is testable from here. */
export function recordedSubmissions(): readonly SubmitRequest[] {
  return recorded;
}

/** The last outcome, for the ghost row. */
export interface LastSubmission extends SubmitRequest, SubmitOutcome {}

let last: LastSubmission | undefined;
export function lastSubmission(): LastSubmission | undefined {
  return last;
}

/**
 * History sequence numbers.
 *
 * Derived from the tail rather than from a module counter, because two windows
 * share one history (06 §13.1) and a per-window counter would produce two rows
 * claiming `seq: 7`.
 */
function nextSeq(): number {
  return (historyState.entries.at(-1)?.seq ?? 0) + 1;
}

/**
 * Submit one command.
 *
 * The History row is appended **after** the sink answers, carrying the real
 * `_rc`. Appending optimistically with `rc: 0` and patching it later would make
 * the red-on-failure rule flicker, and a History pane that briefly shows a
 * failed command as successful is worse than one that appears a frame later.
 *
 * A blank line is not a command: Stata's Command window ignores Enter on an
 * empty line, and a blank row in History is noise the user cannot delete
 * meaningfully.
 */
export async function submitCommand(
  text: string,
  origin: SubmitOrigin = "commandbar",
): Promise<SubmitOutcome | undefined> {
  const command = text.replace(/\s+$/u, "");
  if (command.trim() === "") {
    submitCounters.blanks += 1;
    return undefined;
  }

  submitCounters.submissions += 1;
  const request: SubmitRequest = { text: command, origin };
  const outcome = await sink(request);

  appendHistory({ seq: nextSeq(), command, rc: outcome.rc, origin });
  submitCounters.historyAppends += 1;
  last = { ...request, ...outcome };
  return outcome;
}

/**
 * Submit several commands in order — History's **Do selected**.
 *
 * > Stata will attempt to run all the selected commands, even those containing
 * > errors, and will not stop even if a command causes an error. ([GSM] 2)
 *
 * That sentence is why this is a loop that ignores `rc` rather than an
 * early-return. It is not the do-file semantics, and it is deliberate: the
 * History window is an interactive surface, not a script.
 */
export async function submitAll(
  commands: readonly string[],
  origin: SubmitOrigin = "history",
): Promise<void> {
  for (const command of commands) {
    await submitCommand(command, origin);
  }
}

/** Test seam. */
export function resetSubmitState(): void {
  recorded.length = 0;
  last = undefined;
  sink = recordingSink;
  resetSubmitCounters();
}
