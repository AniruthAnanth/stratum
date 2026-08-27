/**
 * A DOM for the pre-host bridge — plan W25.
 *
 * # Why this file exists
 *
 * `bridge.ts` answers the harness's `snapshot` out of the application's own
 * state. Until repair round 3 it could not answer three of those fields at all
 * and named the units that owed them. Two of those units — W13's editor and
 * W14's result renderers — landed in wave 1, so the claim went stale: the
 * *product* had the answer and the bridge was still reporting it blocked. The
 * only thing actually missing was a `document`, because the tier-1 host is a
 * node process (`serve.test.ts` is `@vitest-environment node`, for the realm
 * reason its header gives).
 *
 * jsdom is already a dev dependency — it is the environment every other suite in
 * this frontend runs under — so the DOM the editor and the cards need is one
 * import away. Installing it *here*, explicitly, rather than by switching
 * `serve.test.ts` to the jsdom environment, keeps two properties that file
 * depends on:
 *
 *   * `TextEncoder`, `Uint8Array` and `WebAssembly` stay node's own, so the wasm
 *     segmenter keeps working. jsdom's globals come from a second realm and
 *     `wasm-bindgen`'s `instanceof` checks fail across it — that is what the
 *     header of `serve.test.ts` is about, and it is still true;
 *   * only the names node does not already define are installed, so nothing
 *     node provides is shadowed.
 *
 * # Why the caller must await this before importing the editor
 *
 * `@codemirror/view` and `solid-js/web` both touch `window` at MODULE
 * EVALUATION time — `solid-js/web`'s `delegateEvents` defaults its argument to
 * `window.document`, and importing `src/ui/index.ts` in a bare node realm
 * throws `ReferenceError: window is not defined` before a single test runs.
 * Static imports are hoisted above every statement in a module, and biome's
 * `organizeImports` sorts them, so "import the shim first" is not something a
 * source file can express. `createBridge` therefore awaits {@link ensureDom}
 * and then reaches the editor and renderer modules through `await import(...)`.
 *
 * In a real webview — tier 2, and the packaged host once W17 lands — there is
 * already a `document` and this is a no-op that returns it.
 */

/** Where the DOM the bridge is using came from. Reported in the host name. */
export type DomKind = "native" | "jsdom";

/** The DOM the bridge mounted into. */
export interface InstalledDom {
  readonly kind: DomKind;
  readonly document: Document;
}

/**
 * jsdom's public shape, to the extent this file uses it.
 *
 * Hand-written because `@types/jsdom` is not a dependency of this app and
 * `apps/desktop/package.json` is W12's file (R0). The specifier is built at run
 * time so `tsc --noEmit` does not try to resolve a module it has no types for;
 * the cast below is the type, and it is checked by the fact that a wrong one
 * fails every bridge test in `serve.test.ts` immediately.
 */
interface JsdomModule {
  JSDOM: new (
    html: string,
    options: { pretendToBeVisual: boolean; url: string },
  ) => { window: Record<string, unknown> };
}

/**
 * A `Range.getClientRects` that reports no rectangles.
 *
 * jsdom implements no layout: `Range` carries neither `getClientRects` nor
 * `getBoundingClientRect`, and CodeMirror's `measureTextSize` calls the first on
 * the animation frame after a view is constructed. W13's own editor suite never
 * sees this, because under the *vitest* jsdom environment the window is torn
 * down when the test file ends and that frame never fires; the window this
 * module builds outlives the test that asked for it, so it does.
 *
 * An empty list is the honest answer rather than a convenient one — jsdom
 * reports every rect as zero either way, which is the same reasoning
 * `platform/shims.ts` gives for its inert `ResizeObserver`. Fabricating
 * plausible geometry would make CodeMirror lay out a viewport that does not
 * exist, and nothing tier 1 asserts is about pixels.
 */
function installRangeRects(w: Record<string, unknown>): void {
  const range = w["Range"] as { prototype: Record<string, unknown> } | undefined;
  if (range === undefined) return;
  const empty = Object.assign([] as unknown[], { item: () => null });
  if (typeof range.prototype["getClientRects"] !== "function") {
    range.prototype["getClientRects"] = () => empty;
  }
  if (typeof range.prototype["getBoundingClientRect"] !== "function") {
    range.prototype["getBoundingClientRect"] = () => ({
      x: 0,
      y: 0,
      top: 0,
      left: 0,
      right: 0,
      bottom: 0,
      width: 0,
      height: 0,
    });
  }
}

let installed: InstalledDom | null = null;

/**
 * Make sure `globalThis.document` exists, installing jsdom if it does not.
 *
 * Idempotent: the second call returns the first call's answer rather than
 * building a second window, so two bridges in one process share one document
 * and `document.body` means the same thing to both.
 */
export async function ensureDom(): Promise<InstalledDom> {
  if (installed !== null) return installed;

  const g = globalThis as unknown as Record<string, unknown>;
  if (g["document"] !== undefined) {
    installed = { kind: "native", document: g["document"] as Document };
    return installed;
  }

  // A computed specifier: see JsdomModule above for why it is not a literal.
  const specifier = "jsdom";
  const { JSDOM } = (await import(/* @vite-ignore */ specifier)) as JsdomModule;
  const dom = new JSDOM("<!doctype html><html><body></body></html>", {
    // CodeMirror asks for `requestAnimationFrame` and `performance` the moment
    // a view is constructed; without this jsdom omits both.
    pretendToBeVisual: true,
    // `document.baseURI` has to be something the asset helpers can resolve
    // against. `about:blank` makes `new URL(rel, base)` throw.
    url: "http://localhost/",
  });
  const w = dom.window;

  // Only what node is missing. `TextEncoder`, `Uint8Array`, `WebAssembly`,
  // `performance` and `fetch` all already exist here and stay node's — the wasm
  // segmenter is instantiated from node `Uint8Array`s and must keep matching
  // wasm-bindgen's `instanceof` checks.
  for (const key of Object.getOwnPropertyNames(w)) {
    if (g[key] !== undefined) continue;
    try {
      g[key] = w[key];
    } catch {
      // A handful of jsdom globals are accessor-only on the window and refuse
      // to be re-read. None of them is one the editor or a card touches.
    }
  }
  // These two are defined on the node global as `undefined`-valued getters in
  // some runtimes rather than being absent, so they are assigned unconditionally.
  g["window"] = w;
  g["document"] = w["document"];

  // W12's capability shims: `ResizeObserver`, `matchMedia`, `scheduler.postTask`.
  // dockview and the editor's scroll compensation both ask for them, and the
  // shims install only what is missing, exactly as under the jsdom environment.
  await import("../platform/shims.ts");
  installRangeRects(w);

  installed = { kind: "jsdom", document: g["document"] as Document };
  return installed;
}

/** Test seam: forget the installed DOM without tearing the globals down. */
export function resetDomMemo(): void {
  installed = null;
}
