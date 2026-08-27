/**
 * The packaged-host half of the tier-1 e2e bridge — plan W17, ADR-011.
 *
 * When `stratum-desktop` is built `--features e2e` and launched by the harness
 * (`STRATUM_E2E` set), the host's control channel (`src-tauri/src/e2e_cmds.rs`)
 * forwards each harness request to this webview as an emitted event and waits
 * for the answer through the `e2e_reply` command:
 *
 * ```text
 *   harness ──socket──> Control ──emit "stratum://e2e-request"──> HERE
 *           <──socket──         <──invoke e2e_reply──────────────
 * ```
 *
 * The answers come from the SAME `createBridge` the pre-host node path uses
 * (`bridge.ts`): the real command registry, the real W12 stores, the real W13
 * editor and W14 cards — mounted against this window's native DOM instead of
 * jsdom. One bridge, two transports, which is the same property that keeps the
 * two tiers honest (see `protocol.ts`).
 *
 * This module is loaded ONLY by `boot/wire.tsx`, dynamically, and only when the
 * host actually emits an e2e request — an ordinary boot registers the listener
 * and never evaluates this file. The Rust half is fenced by the cargo feature
 * (`xtask e2e --check-fence`); this half is inert without it, because
 * `e2e_reply` does not exist to invoke.
 */

import { bridge as hostBridge } from "../platform/bridge";
import type { E2eBridge, Section } from "./protocol";

/**
 * The event the host emits. Keep in step with `REQUEST_EVENT` in
 * `src-tauri/src/e2e_cmds.rs` and with the literal in `boot/wire.tsx` (which
 * cannot import this module without bundling it); `wire.test.ts` pins all
 * three spellings to one string.
 */
export const REQUEST_EVENT = "stratum://e2e-request";

interface E2eRequest {
  id: number;
  op: string;
  payload: unknown;
}

let building: Promise<E2eBridge> | undefined;

async function buildBridge(): Promise<E2eBridge> {
  const [{ loadSegmenter }, { createBridge, installBridge }] = await Promise.all([
    import("../wasm/loader"),
    import("./bridge"),
  ]);
  const platform = hostBridge().platform();
  const built = await createBridge({ segmenter: await loadSegmenter(), platform });
  // Where tier 2's `executeScript` finds it — same global, same object.
  installBridge(built);
  return built;
}

function isRequest(v: unknown): v is E2eRequest {
  return (
    typeof v === "object" &&
    v !== null &&
    typeof (v as E2eRequest).id === "number" &&
    typeof (v as E2eRequest).op === "string"
  );
}

/**
 * Answer one emitted e2e request through `e2e_reply`.
 *
 * The heavy half — wasm, the bridge, its detached editor — is built once, on
 * the first request, and every later request awaits the same promise.
 */
export async function answerE2eRequest(raw: unknown): Promise<void> {
  if (!isRequest(raw)) {
    console.error("stratum e2e: malformed request", raw);
    return;
  }
  let ok = true;
  let payload: unknown;
  try {
    building ??= buildBridge();
    const e2e = await building;
    switch (raw.op) {
      case "capabilities":
        payload = e2e.capabilities();
        break;
      case "e2e_dispatch":
        payload = e2e.dispatch(raw.payload as never);
        break;
      case "e2e_snapshot":
        payload = e2e.snapshot(raw.payload as Section[]);
        break;
      default:
        ok = false;
        payload = `unknown e2e op: ${raw.op}`;
    }
  } catch (error) {
    ok = false;
    // Message AND stack: WebKit's `stack` does not begin with the message the
    // way V8's does, and a stack with no message names the where but not the
    // what.
    payload =
      error instanceof Error ? `${error.name}: ${error.message}\n${error.stack}` : String(error);
  }
  try {
    await hostBridge().invoke("e2e_reply", { id: raw.id, ok, payload });
  } catch (error) {
    // A reply that cannot be delivered is the host's timeout to report.
    console.error("stratum e2e: reply failed", error);
  }
}
