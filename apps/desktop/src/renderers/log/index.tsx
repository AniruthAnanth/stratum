/**
 * `ResultPayload::Log` — styled runs, 06 §9.2 and CONTRACTS §5.2 (A12).
 *
 * **The frontend never parses SMCL.** Runs arrive as `(text, style)` pairs from
 * one of two Rust producers — `stratum_runtime::smcl` for user output, and
 * `classic_text(linesize)` for every built-in statistical table — and this
 * renderer maps `StyleId` to a class and nothing else. That split is why the CLI
 * text mode, the log file and this pane can be byte-identical: they all flatten
 * the same runs through `stratum_proto::styled::to_plain`.
 *
 * Consecutive runs are emitted as sibling spans inside one `white-space: pre`
 * block, so a run boundary in the middle of a line does not become a line break
 * and copying the selection yields the original bytes.
 */

import { For, type JSX } from "solid-js";
import type { LogPayloadView, StyleIdView } from "../types";

import "./log.css";

/** `StyleId` → class. `Link { target_index }` is an object, every other is a tag. */
export function styleClass(style: StyleIdView): string {
  return typeof style === "string" ? `smcl--${style}` : "smcl--link";
}

export function linkTarget(style: StyleIdView): number | undefined {
  return typeof style === "string" ? undefined : style.link.target_index;
}

export interface LogCardProps {
  payload: LogPayloadView;
}

export function LogCard(props: LogCardProps): JSX.Element {
  return (
    <pre class="smcl" data-log>
      <For each={props.payload.runs}>
        {(run) => (
          <span class={styleClass(run.style)} data-link-target={linkTarget(run.style)}>
            {run.text}
          </span>
        )}
      </For>
    </pre>
  );
}
