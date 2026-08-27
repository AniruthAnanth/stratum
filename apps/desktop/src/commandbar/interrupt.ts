/**
 * Break — 06 §9.1's Break chord and the `⏹` toolbar button beside it, over
 * CONTRACTS §6's cancellation ladder.
 *
 * The chord itself is deliberately not written out here. 06 §9.1 gives a
 * per-platform pair; the *binding* is the platform-neutral `Mod+.` the presets
 * carry, and the *label a human reads* is the host's — `menu_accelerator`, i.e.
 * `MenuHost::accelerator(ActionId, KeymapPreset)`. A comment that restates the
 * Windows half is a second answer to a question only the host may answer, and
 * `stratum-platform`'s `frontend_accelerator_literals` test bans it under
 * `apps/desktop/src` with no carve-out for comments.
 *
 * # Why Break needs a home outside the editor
 *
 * The keymap presets bind `Mod+.` to `run.break`, and W13's descriptor for that
 * verb is `enabled: () => active !== null` — an editor must be focused. That is
 * correct for the editor and wrong for Classic, where 06 §9.6 puts the do-file
 * editor in a **separate window by default**: in the main window there is no
 * editor at all, the verb is disabled, `dispatchKeydown` reports "ignored" (it
 * must — "an unregistered or disabled verb must fall through to the platform,
 * or Mod+C stops copying"), and the keystroke arrives at the Command pane. So
 * Break is served here, and a run started from the Command window can always be
 * interrupted from the Command window.
 *
 * Escalated in W16's return rather than fixed by re-registering `run.break`:
 * `registerCommand` is last-writer-wins on the id, and one unit silently
 * replacing another's verb is worse than the gap it closes.
 *
 * # The ladder is not implemented here
 *
 * CONTRACTS §6: `Interrupt` → 2 000 ms → *Force stop* → `Abort` → 4 000 ms →
 * kill and offer replay. That escalation is the run chrome's (W15) and the
 * host's (W17); this module owns one thing — "the user asked to stop" — and
 * hands it to a sink. A second ladder here would be a second answer to how long
 * `bootstrap` gets before we kill the engine.
 */

export type CancelLevelName = "interrupt" | "abort";

export interface BreakRequest {
  readonly level: CancelLevelName;
}

export type InterruptSink = (request: BreakRequest) => void | Promise<void>;

export interface InterruptCounters {
  /** Break requests raised. */
  breaks: number;
}

const ZERO: InterruptCounters = { breaks: 0 };
export const interruptCounters: InterruptCounters = { ...ZERO };
export function resetInterruptCounters(): void {
  Object.assign(interruptCounters, ZERO);
}

const recorded: BreakRequest[] = [];
const recordingSink: InterruptSink = (request) => {
  recorded.push(request);
};

let sink: InterruptSink = recordingSink;

export function setInterruptSink(next: InterruptSink | null): void {
  sink = next ?? recordingSink;
}

export function recordedBreaks(): readonly BreakRequest[] {
  return recorded;
}

/** `Mod+.`, and the `⏹` toolbar button. */
export function requestBreak(level: CancelLevelName = "interrupt"): void {
  interruptCounters.breaks += 1;
  void sink({ level });
}

/** Test seam. */
export function resetInterruptState(): void {
  recorded.length = 0;
  sink = recordingSink;
  resetInterruptCounters();
}
