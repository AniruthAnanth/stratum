/**
 * The quick-action row — spec §4, CONTRACTS §5.1 (A22).
 *
 * **The action row is data, not markup.** The row is `envelope.actions`, computed
 * in Rust from `stratum_effects::CommandRegistry` — i.e. from what this build
 * actually implements — and rendered here in the order it arrives. No renderer
 * may add an item, and this is the only file in `src/renderers/` that knows an
 * action's human label at all; `contract.test.ts` greps the other twenty for
 * those strings and fails on a hit.
 *
 * That indirection is the whole point of A22. The pre-audit design drew
 * "Run margins" and "Coefficient plot" on every `regress` card while `margins`
 * was out of Pass-1 scope, so two of §4's eight quick actions would have rendered
 * and then returned exit-10 "unsupported". A promise that fails on click is worse
 * than an absent button.
 *
 * The single exception, and it is a guarantee rather than an invention:
 * **`Raw ▸` is always present and always last** (spec §17, §5.1's own comment).
 * It is not capability-gated — the classic text is in the envelope's `raw.head`
 * and the rest is one asset fetch away — so there is no build in which it can
 * fail on click. The row therefore drops any `raw_output` the engine sent and
 * appends exactly one of its own at the end. When the engine omitted it,
 * `rawOutputRepairs()` counts the repair so an engine bug is visible in a test
 * rather than invisible behind a correct-looking card.
 */

import { For, type JSX } from "solid-js";
import type { CardActionTag, CardActionView } from "./types";

/**
 * The one label table. Keyed by the wire tag, so a variant added to
 * `CardAction` in Rust is a TypeScript error here and nowhere else.
 */
const ACTION_LABELS: Readonly<Record<CardActionTag, string>> = {
  raw_output: "Raw ▸",
  copy_table: "Copy",
  export: "Export ▸",
  hide_output: "Hide output",
  plot_coefficients: "Plot coefficients",
  run_margins: "Run margins",
  compare_model: "Compare ▸",
  diagnostics: "Diagnostics ▸",
  ai_explain: "Explain",
  ai_check_model: "Check model",
  ai_suggest_next: "Suggest next step",
};

/** Exported for the Results pane's overflow menu, which shows the same words. */
export function actionLabel(tag: CardActionTag): string {
  return ACTION_LABELS[tag];
}

/** The mandatory action, in the position §17 requires. */
export const RAW_ACTION: CardActionView = { action: "raw_output" };

let repairs = 0;

/**
 * How many envelopes arrived without the `RawOutput` §5.1 calls mandatory.
 * Zero against a conforming engine; the mock's three envelopes all carry it.
 */
export function rawOutputRepairs(): number {
  return repairs;
}

export function resetRawOutputRepairs(): void {
  repairs = 0;
}

/**
 * `envelope.actions` in wire order, with `Raw ▸` moved to last.
 *
 * Pure and exported so the invariant can be asserted without a DOM: every
 * payload variant, every action list including the empty one, ends in
 * `raw_output` exactly once.
 */
export function orderedActions(actions: readonly CardActionView[]): readonly CardActionView[] {
  const rest = actions.filter((a) => a.action !== "raw_output");
  if (rest.length === actions.length) repairs += 1;
  return [...rest, RAW_ACTION];
}

export interface ActionRowProps {
  actions: readonly CardActionView[];
  /** The host decides what an action does; the renderer only reports the click. */
  onAction?: (action: CardActionView) => void;
  /** Which action is currently "open" (the raw disclosure), for `aria-expanded`. */
  expanded?: CardActionTag;
}

export function ActionRow(props: ActionRowProps): JSX.Element {
  return (
    <div class="card__actions" data-card-actions role="toolbar" aria-label="Result actions">
      <For each={orderedActions(props.actions)}>
        {(action) => (
          <button
            type="button"
            class="card__action"
            data-action={action.action}
            aria-expanded={props.expanded === action.action ? "true" : undefined}
            onClick={() => props.onAction?.(action)}
          >
            {ACTION_LABELS[action.action]}
          </button>
        )}
      </For>
    </div>
  );
}
