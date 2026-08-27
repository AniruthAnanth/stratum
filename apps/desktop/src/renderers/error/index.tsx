/**
 * Errors — 06 §6.6.
 *
 * "The error card is the same card with a red rail." That is not a slogan: the
 * rail, the glyph, the echoed command, the state readout and the action row all
 * come from the shared shell, and this file draws only the body. An error is a
 * result, not a modal.
 *
 * Deterministic suggestions render as chips. `Diagnostic.offending_token` is the
 * field spec §21 actually needs — without it "Did you mean 'income'?" degrades to
 * regex-scraping English prose — so it is shown as itself rather than dug back
 * out of `message`.
 *
 * What this renderer deliberately does NOT do is 06 §6.6's "echoes the submitted
 * line with the culprit range underlined". `Diagnostic.span` is a byte range in
 * the ORIGINAL source, composed back through the SpanMap; underlining it needs
 * the document, which the editor has and a renderer does not. That underline
 * belongs to W13's block widget, and drawing an approximation here — by
 * searching the echoed `cmdline` for the token — would put the marker under the
 * wrong occurrence the first time a variable appeared twice.
 *
 * AI actions (`Explain`, `Fix`) are appended by the AI module into the same
 * action row (§17.4), never as a separate panel — which is automatic here,
 * because the action row is `envelope.actions` and nothing else.
 */

import { For, type JSX, Show } from "solid-js";
import type { ErrorPayloadView } from "../types";

import "./error.css";

export interface ErrorCardProps {
  payload: ErrorPayloadView;
  /** A deterministic suggestion was accepted. The host applies the edits. */
  onApply?: (index: number) => void;
  /** `r(199);` is a link to help — the host owns navigation. */
  onHelp?: (rc: number) => void;
}

export function ErrorCard(props: ErrorCardProps): JSX.Element {
  return (
    <div class="err" data-error data-severity={props.payload.severity}>
      <p class="err__message" data-error-message>
        {props.payload.message}
      </p>

      <p class="err__codes">
        <Show when={props.payload.stata_rc}>
          {(rc) => (
            <button
              type="button"
              class="err__rc"
              data-error-rc={rc()}
              onClick={() => props.onHelp?.(rc())}
            >
              {`r(${String(rc())});`}
            </button>
          )}
        </Show>
        <span class="err__code" data-error-code>
          {props.payload.code}
        </span>
        <Show when={props.payload.offending_token}>
          {(token) => (
            <code class="err__token" data-error-token>
              {token()}
            </code>
          )}
        </Show>
      </p>

      <Show when={props.payload.suggestions.length > 0}>
        <div class="err__suggestions">
          <For each={props.payload.suggestions}>
            {(suggestion, i) => (
              <button
                type="button"
                class="err__suggestion"
                data-error-suggestion={suggestion.kind}
                disabled={suggestion.edits.length === 0}
                onClick={() => props.onApply?.(i())}
              >
                {suggestion.label}
              </button>
            )}
          </For>
        </div>
      </Show>

      <For each={props.payload.notes}>{(note) => <p class="card__note">{note}</p>}</For>
    </div>
  );
}
