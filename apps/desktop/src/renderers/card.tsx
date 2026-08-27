/**
 * The card shell — 06 §4.6 "Card anatomy — identical for every renderer".
 *
 * ```
 * ▌ ✓  summarize income age education           E41 · D17 · 0.08s   ⋯
 * ▌    ─────────────────────────────────────────────────────────────
 * ▌     <body: whatever the typed renderer drew>
 * ▌    ─────────────────────────────────────────────────────────────
 * ▌    Distribution   Missingness   Copy   Raw ▸
 * ```
 *
 * Six slots, one order, no renderer allowed to vary it: rail, glyph, echoed
 * command, state readout, body, action row ending in `Raw ▸` (06 §14.8 rule 2).
 * `renderers.test.tsx` asserts that document order over every `ResultPayload`
 * variant including `Unknown`, which is what makes "identical anatomy" a fact
 * rather than a convention twenty files are each expected to remember.
 *
 * Three behaviours the shell owns because they must not be re-decided per
 * renderer:
 *
 *  * **No animation on appearance** (06 §14.6). A card is painted at its final
 *    height from `layout_hint.est_px` and never fades, slides or grows in. The
 *    only motion in this file is the 120 ms collapse height and the running
 *    hairline, and both stop under `prefers-reduced-motion`.
 *  * **Running is a 1 px hairline advancing along the rail, never a spinner.**
 *    The rail is already the strongest system element in the product — the
 *    gutter glyph, the hairline and the rail are one colour for one block — so
 *    progress belongs on it, not in a rotating disc borrowed from a web app.
 *  * **Stale never re-runs itself** (spec §13). Stale dashes the rail, drops the
 *    BODY to .62 while the header stays at full opacity, and states which
 *    upstream block did it. The shell has no code path that can issue a run.
 */

import { type JSX, Show } from "solid-js";
import type { BlockStatusState } from "../ipc/hand";
import { StateGlyph } from "../ui";
import { ActionRow } from "./actions";
import { readout } from "./readout";
import type { CardActionView, CardUiState, ResultEnvelopeView } from "./types";

import "./card.css";

/**
 * The card's state, derived from the envelope and the host's UI state. Cards
 * show a block state (spec §12), not a "result kind": 06 §14.8 rule 3 is that
 * colour never encodes what sort of result this is.
 */
export function cardState(envelope: ResultEnvelopeView, ui: CardUiState): BlockStatusState {
  if (ui.running === true) return "running";
  if (envelope.rc !== 0) return "failed";
  if (ui.stale !== undefined) return "stale";
  return "current";
}

/**
 * Middle ellipsis for the echoed command (06 §4.6). A macro-expanded `cmdline`
 * can be thousands of characters; the head and the tail are the informative
 * ends, and the whole string is on `title` and in the accessible name.
 *
 * A character budget rather than a measured width on purpose: measuring is a
 * layout read on the mount path, and this runs once per card per revision.
 */
export function middleEllipsis(text: string, budget = 120): string {
  if (text.length <= budget) return text;
  const head = Math.ceil((budget - 1) / 2);
  const tail = budget - 1 - head;
  return `${text.slice(0, head)}…${text.slice(text.length - tail)}`;
}

/** The `aria-live` sentence a host announces on completion (06 §17). */
export function announcement(envelope: ResultEnvelopeView): string {
  const verb = envelope.rc === 0 ? "finished" : `failed, r(${String(envelope.rc)})`;
  const name = envelope.cmdline.split(/\s+/, 1)[0] ?? "command";
  return `${name} ${verb}, ${readout(envelope.exec, envelope.dataset_state, envelope.duration_us).duration}`;
}

export interface CardProps {
  envelope: ResultEnvelopeView;
  ui?: CardUiState;
  /** The typed renderer's output. */
  children: JSX.Element;
  onAction?: (action: CardActionView) => void;
  /** `⋯` — the block menu. The host owns its contents (previous results, etc.). */
  onMenu?: () => void;
  /** Which action is currently disclosed, for `aria-expanded` on its button. */
  expanded?: CardActionView["action"];
}

export function Card(props: CardProps): JSX.Element {
  const ui = (): CardUiState => props.ui ?? {};
  const state = (): BlockStatusState => cardState(props.envelope, ui());
  const meta = (): ReturnType<typeof readout> =>
    readout(props.envelope.exec, props.envelope.dataset_state, props.envelope.duration_us);

  return (
    <article
      class="card"
      data-card
      data-state={state()}
      data-stale={ui().stale === undefined ? undefined : ""}
      data-collapsed={ui().collapsed === true ? "" : undefined}
      tabindex="0"
      aria-label={`Result for ${props.envelope.cmdline}`}
      // 06 §4.6: the final height is known before the body is laid out, which is
      // what keeps scroll anchoring sane on a webview with no `overflow-anchor`.
      style={{ "--card-est-px": `${String(props.envelope.layout_hint.est_px)}px` }}
    >
      <span class="card__rail" data-card-rail aria-hidden="true">
        <Show when={ui().running === true}>
          <span
            class="card__hairline"
            data-running-hairline
            style={{
              height:
                ui().progress === undefined
                  ? undefined
                  : `${String(Math.round((ui().progress ?? 0) * 100))}%`,
            }}
            data-indeterminate={ui().progress === undefined ? "" : undefined}
          />
        </Show>
      </span>

      <header class="card__header">
        <span class="card__glyph" data-card-glyph>
          <StateGlyph state={state()} detail={ui().stale?.because} />
        </span>
        <code class="card__cmd" data-card-cmd title={props.envelope.cmdline}>
          {middleEllipsis(props.envelope.cmdline)}
        </code>
        <span class="card__readout" data-card-readout aria-label={meta().label}>
          <span>{meta().exec}</span>
          <span class="card__dot" aria-hidden="true">
            ·
          </span>
          <span>{meta().dataset}</span>
          <span class="card__dot" aria-hidden="true">
            ·
          </span>
          <span>{meta().duration}</span>
        </span>
        <button type="button" class="card__menu" aria-label="Block menu" onClick={props.onMenu}>
          ⋯
        </button>
      </header>

      {/* Spec §13: never auto-rerun. The strip states the specific upstream
          block and offers nothing that runs on its own. */}
      <Show when={ui().stale}>
        {(reason) => (
          <p class="card__stale" data-card-stale>
            <span class="card__stale-because">{reason().because}</span>
            <span class="card__dot" aria-hidden="true">
              ·
            </span>
            <span class="card__stale-upstream">{reason().upstream}</span>
          </p>
        )}
      </Show>

      <div class="card__body" data-card-body hidden={ui().collapsed === true}>
        {props.children}
      </div>

      <ActionRow
        actions={props.envelope.actions}
        onAction={props.onAction}
        expanded={props.expanded}
      />
    </article>
  );
}
