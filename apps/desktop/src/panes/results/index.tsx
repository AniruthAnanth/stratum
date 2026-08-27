/**
 * The Results pane — spec §4 (`InlineResults: Off`), §17, §18; 06 §6.1, §9.2.
 *
 * **Everything goes to the scrollback, always.** Inline cards are a presentation
 * layer; this pane holds the same results whether or not a card was ever drawn.
 * Turning inline results off loses nothing, turning them on later loses nothing,
 * and a detached card's output still exists — which is only true because this
 * pane is never fed by the editor.
 *
 * Two presentations, one source:
 *
 *  * **Cards** — the same [`ResultCard`] the editor mounts, so a `regress` in the
 *    Results pane and a `regress` under the code are the same object.
 *  * **Classic** — 06 §9.2's append-only scrollback: the echoed `. command` line
 *    in command ink, then the classic output, `white-space: pre`, no wrapping.
 *    The bytes are `envelope.raw.head` and nothing touches them.
 *
 * The classic body is deliberately a thin `<pre>` over the envelope. W16 owns
 * `src/log/{window,selection,find}.ts` — the Fenwick-indexed 5 M-line window, the
 * cross-boundary selection model and the synthetic scrollbar — and this pane is
 * where that lands. Rendering the resident envelopes directly until then is
 * honest about what exists; building a second, worse virtualiser here would not
 * be, and W16's acceptance would then have to delete it.
 */

import { For, type JSX, Show, createMemo, createSignal } from "solid-js";
import type { ResultId } from "../../ipc/hand";
import { ResultCard, type ResultCardHandlers, announcement } from "../../renderers";
import type { CardUiState, ResultEnvelopeView } from "../../renderers";
import { PaneHeader, Segmented } from "../../ui";

import "./results.css";

export type ResultsMode = "cards" | "classic";

export interface ResultsPaneProps extends ResultCardHandlers {
  /** Oldest first. The resident window; `state/results.ts` owns the cap. */
  envelopes: readonly ResultEnvelopeView[];
  mode?: ResultsMode;
  onModeChange?: (mode: ResultsMode) => void;
  /** Per-result UI state (collapsed, stale, running), keyed by `ResultId`. */
  ui?: (id: ResultId) => CardUiState | undefined;
}

const MODES = [
  { value: "cards" as const, label: "Cards" },
  { value: "classic" as const, label: "Classic" },
];

export function ResultsPane(props: ResultsPaneProps): JSX.Element {
  const [uncontrolled, setUncontrolled] = createSignal<ResultsMode>("cards");
  const mode = (): ResultsMode => props.mode ?? uncontrolled();

  const latest = createMemo((): ResultEnvelopeView | undefined => props.envelopes.at(-1));

  return (
    <section class="results" data-pane="results" data-mode={mode()}>
      <PaneHeader
        title="Results"
        actions={
          <Segmented
            options={MODES}
            value={mode()}
            onChange={(next) => {
              setUncontrolled(next);
              props.onModeChange?.(next);
            }}
            label="Results presentation"
          />
        }
      />

      {/* 06 §17: completion is announced politely, a failure assertively. One
          live region for the pane — one per card would announce a replayed
          scrollback forty times on open. */}
      <p
        class="results__live"
        data-results-live
        aria-live={latest()?.rc === 0 ? "polite" : "assertive"}
      >
        <Show when={latest()}>{(envelope) => announcement(envelope())}</Show>
      </p>

      <div class="results__scroll" data-results-scroll>
        <Show
          when={props.envelopes.length > 0}
          fallback={<p class="results__empty">No results yet.</p>}
        >
          <Show
            when={mode() === "cards"}
            fallback={
              <pre class="results__classic" data-results-classic>
                <For each={props.envelopes}>
                  {(envelope) => (
                    <>
                      <span class="results__echo">{`. ${envelope.cmdline}\n`}</span>
                      <span data-results-raw={String(envelope.result)}>{envelope.raw.head}</span>
                    </>
                  )}
                </For>
              </pre>
            }
          >
            <For each={props.envelopes}>
              {(envelope) => (
                <ResultCard
                  envelope={envelope}
                  ui={props.ui?.(envelope.result)}
                  onAction={props.onAction}
                  onMenu={props.onMenu}
                  onSelectVar={props.onSelectVar}
                  onOpenViewer={props.onOpenViewer}
                  onApplySuggestion={props.onApplySuggestion}
                  onHelp={props.onHelp}
                />
              )}
            </For>
          </Show>
        </Show>
      </div>
    </section>
  );
}
