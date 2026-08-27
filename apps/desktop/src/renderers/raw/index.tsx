/**
 * `Raw ▸` — the classic output, byte for byte (spec §17).
 *
 * "Every result exposes View raw/classic output. Compatibility is never hidden
 * behind the richer UI." So this renderer has exactly one job and it is a
 * negative one: **change nothing**. `raw.head` is written into a `white-space:
 * pre` element as a single text node. No trimming, no `replace`, no tab
 * expansion, no re-wrapping, no "smart" quotes — the bytes that reach the DOM are
 * the bytes StataMP 18.5 printed, which is what lets `fixture.test.ts` compare
 * the rendered text against `tests/golden/stata18/core_surface.log` line by line.
 *
 * `head` is the first ≤ 8 KB cut at a line boundary and covers ~99 % of results,
 * so the disclosure paints from memory with no await. Above that the full text
 * is an asset fetch (§5.1); the head is shown IMMEDIATELY and the remainder is
 * appended when it arrives, because a spinner in place of output the frontend
 * already holds would be a self-inflicted stall.
 *
 * This is also the renderer for `ResultPayload::Unknown` — §5.2: "Renders
 * through the raw renderer. No apology, no empty state."
 */

import { type JSX, Show, createSignal } from "solid-js";
import { bridge } from "../../platform/bridge";
import { assetUrl } from "../asset";
import type { RawRefView } from "../types";

import "./raw.css";

export interface RawViewProps {
  raw: RawRefView;
  /** Rendered inline in the card body rather than as a disclosure. */
  inline?: boolean;
}

type Rest = { state: "idle" } | { state: "loading" } | { state: "done"; text: string };

export function RawView(props: RawViewProps): JSX.Element {
  const [rest, setRest] = createSignal<Rest>({ state: "idle" });

  const loadRest = async (): Promise<void> => {
    if (rest().state !== "idle") return;
    setRest({ state: "loading" });
    try {
      const response = await bridge().fetchAsset(assetUrl(props.raw.asset.path));
      const text = await response.text();
      // The asset is the WHOLE output, head included, so it replaces rather than
      // appends: concatenating would duplicate the first 8 KB.
      setRest({ state: "done", text });
    } catch {
      // A failed fetch leaves the head on screen. Losing the tail is bad; losing
      // the part we already had because the tail failed would be worse.
      setRest({ state: "idle" });
    }
  };

  const body = (): string => {
    const r = rest();
    return r.state === "done" ? r.text : props.raw.head;
  };

  return (
    <div class="raw" data-raw data-inline={props.inline === true ? "" : undefined}>
      <pre class="raw__text" data-raw-text>
        {body()}
      </pre>
      <Show when={props.raw.truncated && rest().state !== "done"}>
        <p class="raw__more">
          <span data-raw-truncated>
            {`Showing ${String(props.raw.head.length)} of ${String(props.raw.bytes)} bytes`}
          </span>
          <button
            type="button"
            class="raw__load"
            disabled={rest().state === "loading"}
            onClick={() => {
              void loadRest();
            }}
          >
            Load full output
          </button>
        </p>
      </Show>
    </div>
  );
}
