/**
 * Graphs — spec §18, 06 §6.7.
 *
 * "Stack results, don't spawn windows": the graph is inline in the card, capped
 * at min(pane width, 720 px), at its intrinsic aspect ratio. **Open in window is
 * opt-in** and lives in the action row, never here.
 *
 * Bytes are never inline in the envelope (ARCHITECTURE C23) — a 1.5 MB SVG in a
 * MessagePack event blows the 16 ms coalescing budget for every subscribed
 * window — so the payload carries an `AssetRef` and this renderer fetches it.
 * Two consequences the code makes explicit:
 *
 *  * The `aspect-ratio` box is laid out from `intrinsic_pt` on the FIRST paint,
 *    before the fetch resolves. The card therefore never changes height when the
 *    image lands, which is what keeps scroll anchoring correct in a webview with
 *    no `overflow-anchor` (06 §4.6).
 *  * The fetch goes through `bridge().fetchAsset`, because every
 *    `stratum-asset://` request must carry `X-Stratum-Token` (§10.2) and an
 *    `<img src>` cannot. That is also why the SVG is injected rather than
 *    referenced: `script-src 'self'` makes injected markup inert, and the bytes
 *    came from our own graphics crate over an authenticated scheme.
 */

import { type JSX, Show, createSignal, onCleanup, onMount } from "solid-js";
import { bridge } from "../../platform/bridge";
import { assetUrl } from "../asset";
import type { GraphPayloadView } from "../types";

import "./graph.css";

/** 06 §6.7: capped at min(pane width, 720 px). */
export const MAX_GRAPH_PT = 720;

export interface GraphCardProps {
  payload: GraphPayloadView;
}

export function GraphCard(props: GraphCardProps): JSX.Element {
  const [svg, setSvg] = createSignal<string | undefined>(undefined);
  const [failed, setFailed] = createSignal(false);
  const controller = new AbortController();

  onMount(() => {
    void (async () => {
      try {
        const response = await bridge().fetchAsset(assetUrl(props.payload.asset.path), {
          signal: controller.signal,
        });
        const mime = response.headers.get("content-type") ?? props.payload.asset.mime;
        if (!mime.startsWith("image/svg+xml")) {
          setFailed(true);
          return;
        }
        setSvg(await response.text());
      } catch {
        setFailed(true);
      }
    })();
  });

  onCleanup(() => controller.abort());

  const [w, h] = [props.payload.intrinsic_pt[0], props.payload.intrinsic_pt[1]];

  return (
    <figure
      class="graph"
      data-graph={props.payload.name}
      style={{
        "aspect-ratio": `${String(w)} / ${String(h)}`,
        "max-width": `${String(Math.min(w, MAX_GRAPH_PT))}px`,
      }}
    >
      <Show
        when={svg()}
        fallback={
          <div class="graph__placeholder" data-graph-placeholder>
            <Show when={failed()}>
              <span class="graph__failed">{`${props.payload.name} could not be loaded`}</span>
            </Show>
          </div>
        }
      >
        {(markup) => (
          // Injected rather than referenced: the bytes come from our own
          // graphics crate over the authenticated asset scheme, and CSP
          // `script-src 'self'` (§10.2) makes anything in them inert.
          <div
            class="graph__canvas"
            role="img"
            aria-label={props.payload.source_cmd}
            innerHTML={markup()}
          />
        )}
      </Show>
      <figcaption class="graph__caption">
        <span class="graph__name">{props.payload.name}</span>
        <span class="graph__cmd">{props.payload.source_cmd}</span>
      </figcaption>
    </figure>
  );
}
