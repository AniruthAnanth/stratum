/**
 * The state readout, connected — spec §13; 06 §14.2, §5.3.
 *
 * > "`E41 · D17 · 12,481 obs` … is a thing no code editor has, and it is the
 * > first thing you see." — 06 §14.2
 *
 * `ui/chrome.tsx` (W12) owns how those parts are *drawn*: the separators, the
 * thousands grouping, the byte units. This file owns where the values come from,
 * which is the execution store and nothing else. Two reasons for the split:
 *
 *  * the top bar renders in every window, including ones with no engine attached
 *    yet, so the presentational component has to work from plain props;
 *  * spec §13's promise is that the readout tracks *execution*, so it must be
 *    fed by the same events that decide staleness. A second feed would be a
 *    second source of truth for the one number the whole product is about.
 *
 * The readout also carries the interactive/clean distinction (spec §15). It is
 * the one piece of chrome guaranteed to be on screen, so it is where a run whose
 * results must not be mistaken for the live session announces itself.
 */

import { type JSX, Show } from "solid-js";
import { runCommand } from "../keys/registry";
import { cleanRunActive, readout, runState, staleCount } from "../state/exec";
import { latestResult } from "../state/results";
import { Button, type StateReadout as StateReadoutValue, StateReadoutView } from "../ui";
import { CleanChip } from "./CleanChip";

import "./exec.css";

/** `E41`, `D17` — the two ids spec §13 puts on screen, in its own spelling. */
export function execText(exec: number | undefined): string | undefined {
  return exec === undefined ? undefined : `E${String(exec)}`;
}

export function datasetText(dataset: number | undefined): string | undefined {
  return dataset === undefined ? undefined : `D${String(dataset)}`;
}

export function resultText(result: number | undefined): string | undefined {
  return result === undefined ? undefined : `R${String(result)}`;
}

/**
 * Spec §13's sentence, for the accessible name and the tooltip.
 *
 * §13 writes it as `Execution 41 / Dataset state: D17 / Code hash: … / Result:
 * R41`. Three of the four are here; **the code hash is deliberately absent**
 * rather than approximated. It is 32 hex characters per execution, this store
 * keys hashes by block rather than by execution, and a truncated hash in a
 * status line is a value people would try to compare. Where the hash is actually
 * needed — the block-mismatch path of 06 §5.5 — it is on the wire, not on a bar.
 */
export function readoutSentence(value: StateReadoutValue, result: string | undefined): string {
  const parts: string[] = [];
  if (value.exec !== undefined) parts.push(`Execution ${value.exec.slice(1)}`);
  if (value.dataset !== undefined) parts.push(`Dataset state: ${value.dataset}`);
  if (result !== undefined) parts.push(`Result: ${result}`);
  if (value.obs !== undefined) parts.push(`${value.obs.toLocaleString("en-US")} observations`);
  return parts.length === 0 ? "No execution yet" : parts.join(" / ");
}

export interface StateReadoutProps {
  /** Overrides the store, for previews and for a window with no session. */
  value?: StateReadoutValue;
  onAction?: (command: string) => void;
}

/**
 * The connected readout: `CLEAN?  E41 · D17 · 74 obs · 12 vars  ⟲ 3 stale`.
 *
 * The chip comes FIRST, before the ids. A clean run's `E` and `D` ids are
 * perfectly ordinary-looking numbers; if the qualifier trailed them, the eye
 * would have read the numbers before it read the qualifier.
 */
export function StateReadout(props: StateReadoutProps): JSX.Element {
  const value = (): StateReadoutValue => {
    if (props.value !== undefined) return props.value;
    const r = readout();
    const out: StateReadoutValue = {};
    const exec = execText(r.exec);
    const dataset = datasetText(r.dataset);
    if (exec !== undefined) out.exec = exec;
    if (dataset !== undefined) out.dataset = dataset;
    if (r.obs !== undefined) out.obs = r.obs;
    if (r.vars !== undefined) out.vars = r.vars;
    return out;
  };

  const result = (): string | undefined => resultText(latestResult());

  return (
    <div
      class="exec-readout"
      data-exec-readout
      data-mode={cleanRunActive() ? "clean" : "interactive"}
      // Also a clean SCOPE, so the readout's own glyphs go neutral with the
      // chip. The rest of the window is scoped by the host wrapping its editor
      // and card surfaces in `CleanScope`.
      data-clean={cleanRunActive() ? "" : undefined}
    >
      <CleanChip />
      <span class="exec-readout__ids" title={readoutSentence(value(), result())}>
        <StateReadoutView readout={value()} />
        {/* §13 names the Result id alongside E and D. It lives in W14's result
            store, so it is read from there rather than tracked twice. */}
        <Show when={result()}>
          {(id) => (
            <>
              <span class="exec-readout__sep" aria-hidden="true">
                ·
              </span>
              <span data-exec-result>{id()}</span>
            </>
          )}
        </Show>
      </span>
      <StaleCountButton {...(props.onAction === undefined ? {} : { onAction: props.onAction })} />
    </div>
  );
}

export interface StaleCountButtonProps {
  count?: number;
  onAction?: (command: string) => void;
}

/**
 * `⟲ 3 stale` — 06 §5.3, and the connected form of `TopBar`'s `staleCount` prop.
 *
 * The count is `staleCount()`, an O(1) signal the store maintains incrementally.
 * It is emphatically NOT recomputed here by walking the document: the top bar
 * repaints on every keystroke that changes a block's hash, and a sweep on that
 * path is a long task on the one interaction 06 §15.1 budgets at 16 ms.
 */
export function StaleCountButton(props: StaleCountButtonProps): JSX.Element {
  const n = (): number => props.count ?? staleCount();
  return (
    // The wrapper is always in the DOM, and it is the ONE live region the
    // execution-state UI has. A live region only announces changes to content it
    // already contains, so putting `aria-live` on the button itself would stay
    // silent on the transition that matters — nothing to three. The individual
    // block strips are `aria-live="off"` for the mirror-image reason: forty
    // announcements for one edit is a wall, not information.
    <span class="exec-stale-live" aria-live="polite">
      <Show when={n() > 0}>
        <Button
          variant="quiet"
          icon="rerun"
          class="exec-stale-count"
          data-exec-stale-count={String(n())}
          data-exec-action="run.allStale"
          // Spec §13: never auto-rerun. Offering the verb is the whole feature;
          // taking it is the user's.
          title="Run all stale blocks, in document order"
          onClick={() => {
            if (props.onAction === undefined) runCommand("run.allStale");
            else props.onAction("run.allStale");
          }}
        >
          {`${String(n())} stale`}
        </Button>
      </Show>
    </span>
  );
}

/**
 * "Interactive" / "Clean", spelled out.
 *
 * The chip says CLEAN only while a clean run is in flight. This says which mode
 * the *session* is in at rest, which is the other half of spec §15's "clearly
 * distinguish": after a clean run finishes, the live session is still the
 * interactive one, and nothing on screen should suggest the clean environment
 * persisted.
 */
export function RunModeLabel(props: { mode?: "interactive" | "clean" }): JSX.Element {
  const mode = (): "interactive" | "clean" => props.mode ?? runState().kind;
  return (
    <span class="exec-mode t-micro" data-exec-mode={mode()}>
      {mode() === "clean" ? "Clean" : "Interactive"}
    </span>
  );
}
