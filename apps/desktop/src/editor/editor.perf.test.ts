/**
 * What a keystroke costs — asserted as COUNTERS, per ADR-017.
 *
 * The plan states this unit's budget as "keystroke → segmentation + gutter +
 * highlight ≤ 6 ms p95, same frame, no IPC". ADR-017 is binding and forbids
 * asserting a duration: the same unchanged tree in this repository benchmarked
 * 33 % apart an hour apart under nothing but machine load. So each clause of
 * that budget is implemented here as the counter that expresses it:
 *
 *   "segmentation"  → exactly one `resegment()` per changed transaction, and
 *                     zero on a selection-only one;
 *   "gutter"        → markers built for the VIEWPORT, a number that does not
 *                     move when the document grows sixtyfold;
 *   "highlight"     → tokens decoded and mark ranges built, likewise;
 *   "same frame"    → no full region decode, no decoration rebuild, no widget
 *                     construction anywhere on the path;
 *   "no IPC"        → `ipcCalls` is 0.
 *
 * Durations are RECORDED — printed below, never asserted — and were, on the
 * verification machine against the real wasm module: 1.0 ms at 500 blocks
 * (19 KB), 2.7 ms at 3 000 blocks (121 KB), 5.2 ms at 6 000 blocks (245 KB),
 * all under jsdom. Calibration: StataCorp's shipped ado library has a median
 * program of 2.0 KiB and a p99.9 of 127 KiB, so the middle row is already past
 * the corpus's tail and the last is twice it again.
 */

import { beforeAll, describe, expect, it, vi } from "vitest";
import { allBlocks } from "./blocks/blockField";
import { counters, resetCounters, snapshotCounters } from "./blocks/segmenter";
import { completeRun, editorBackends, mountEditor, runAt, syntheticDoc, typeChar } from "./harness";
import { updateRun } from "./results/anchor";
import { appendStreamingLog } from "./results/widget";

/**
 * Recorded, never asserted (ADR-017).
 *
 * `process.stdout` rather than `console.log`, matching the idiom in
 * `wasm/conformance.ts`: these lines are the measurement's output and belong on
 * the runner's stream, not in a log a linter is right to be suspicious of.
 */
function record(line: string): void {
  process.stdout.write(`${line}\n`);
}

beforeAll(() => {
  vi.spyOn(console, "warn").mockImplementation(() => {});
});

const backends = await editorBackends();

/**
 * Every check runs against every backend in the checkout.
 *
 * The editor is written against `StratumSegmenter` and cannot tell which module
 * is behind it (`wasm/types.ts`); this is the consumer that proves it, which
 * W11a's conformance suite explicitly leaves owed.
 */
describe.each(backends)("keystroke cost [$name]", (backend) => {
  it("does exactly one segmentation per changed transaction and none per selection", async () => {
    const h = await mountEditor(syntheticDoc(200), { segmenter: await backend.load() });

    resetCounters();
    typeChar(h.view, 40, "x");
    expect(counters.wasmResegments).toBe(1);
    expect(counters.wasmSplices).toBe(1);
    expect(counters.wasmSetDocs).toBe(0);

    resetCounters();
    h.view.dispatch({ selection: { anchor: 12 } });
    expect(counters.wasmResegments).toBe(0);
    expect(counters.wasmSplices).toBe(0);
    // A caret move re-renders nothing: the gutter memo, the highlight plugin and
    // the results field all short-circuit on "the outline did not move".
    expect(counters.gutterRebuilds).toBe(0);
    expect(counters.highlightRangesBuilt).toBe(0);
    expect(counters.resultDecoRebuilds).toBe(0);

    h.destroy();
  });

  it("touches the viewport, not the document — the counters do not move with size", async () => {
    const measure = async (blocks: number): Promise<ReturnType<typeof snapshotCounters>> => {
      const h = await mountEditor(syntheticDoc(blocks), { segmenter: await backend.load() });
      typeChar(h.view, 40, "x"); // warm every memo
      resetCounters();
      const started = performance.now();
      typeChar(h.view, 40, "y");
      const ms = performance.now() - started;
      const snapshot = snapshotCounters();
      // Recorded, never asserted (ADR-017).
      record(`[${backend.name}] ${blocks} blocks: ${ms.toFixed(3)} ms/keystroke`);
      h.destroy();
      return snapshot;
    };

    const small = await measure(50);
    const large = await measure(3000);

    // The document grew sixtyfold. Not one of these may move.
    expect(large.regionsDecoded).toBe(small.regionsDecoded);
    expect(large.regionLookups).toBe(small.regionLookups);
    expect(large.tokensDecoded).toBe(small.tokensDecoded);
    expect(large.highlightRangesBuilt).toBe(small.highlightRangesBuilt);
    expect(large.gutterMarkersConstructed).toBe(small.gutterMarkersConstructed);

    // And none of it is the O(document) path.
    expect(large.regionDecodePasses).toBe(0);
    expect(small.regionDecodePasses).toBe(0);
    // One token query per visible range; a viewport is one range.
    expect(large.tokenQueries).toBe(1);
    // Bounded absolutely, not just relatively: a viewport of ~30 blocks.
    expect(large.regionsDecoded).toBeLessThan(80);
  });

  it("makes no IPC call and no document write while typing", async () => {
    const h = await mountEditor(syntheticDoc(100), { segmenter: await backend.load() });
    resetCounters();
    for (let i = 0; i < 200; i++) typeChar(h.view, 40 + i, "z");
    expect(counters.ipcCalls).toBe(0);
    expect(counters.documentWrites).toBe(0);
    h.destroy();
  });
});

describe.each(backends)("cards do not rebuild while typing [$name]", (backend) => {
  it("maps 60 cards through 200 keystrokes without constructing a widget", async () => {
    const h = await mountEditor(syntheticDoc(80), { segmenter: await backend.load() });
    const blocks = allBlocks(h.view.state).filter((b) => b.executable);
    let results = 1;
    for (const block of blocks.slice(0, 60)) {
      const id = runAt(h.view, block.from);
      if (id !== null) completeRun(h.view, id, results++);
    }

    resetCounters();
    for (let i = 0; i < 200; i++) typeChar(h.view, 0, "*");

    // `deco.map(tr.changes)` is CodeMirror's own position mapping. Nothing is
    // rebuilt, nothing is constructed, and every card is still in the right
    // place — which is the entire argument for anchoring by offset.
    expect(counters.resultDecoRebuilds).toBe(0);
    expect(counters.cardWidgetsConstructed).toBe(0);
    expect(counters.regionDecodePasses).toBe(0);
    h.destroy();
  });
});

describe.each(backends)("hover dispatches nothing [$name]", (backend) => {
  it("writes about one attribute pair per block crossed, not per pixel", async () => {
    const h = await mountEditor(syntheticDoc(20), { segmenter: await backend.load() });
    const blocks = allBlocks(h.view.state).filter((b) => b.executable);
    expect(blocks.length).toBeGreaterThan(3);

    // jsdom reports every rectangle as zero, so `posAtCoords` cannot be driven
    // from coordinates. Substituting it is what isolates the handler's decision
    // — "did the block under the pointer change" — from a layout engine that is
    // not the subject of this test.
    const sweep = [0, 1, 2, 3].map((i) => (blocks[i] as { from: number }).from);
    let at = 0;
    h.view.posAtCoords = ((): number | null => sweep[at] ?? null) as typeof h.view.posAtCoords;

    const dom = h.view.dom;
    resetCounters();
    // 400 events, 100 per block, crossing three boundaries.
    for (let i = 0; i < 400; i++) {
      at = Math.floor(i / 100);
      dom.dispatchEvent(new MouseEvent("pointermove", { clientX: 30, clientY: 12, bubbles: true }));
    }
    // Four blocks entered, three left: at most one attribute write each. 400
    // events, seven writes — the pixel count is nowhere in the bound.
    expect(counters.hoverAttributeWrites).toBeGreaterThan(0);
    expect(counters.hoverAttributeWrites).toBeLessThanOrEqual(8);
    h.destroy();
  });
});

describe.each(backends)("streaming does not resize the card [$name]", (backend) => {
  it("appends 500 log chunks with no rebuild, no widget and no transaction", async () => {
    const h = await mountEditor(syntheticDoc(6), { segmenter: await backend.load() });
    const block = allBlocks(h.view.state).filter((b) => b.executable)[0];
    expect(block).toBeDefined();
    const id = runAt(h.view, (block as { from: number }).from);
    expect(id).not.toBeNull();
    h.view.dispatch({
      effects: updateRun.of({
        id: id as number,
        patch: { kernel: { state: "running" }, streaming: true },
      }),
    });

    const before = h.view.state.doc.toString();
    resetCounters();
    for (let i = 0; i < 500; i++) appendStreamingLog(h.view, id as number, `line ${i}\n`);

    // The card's height is fixed while running, so none of this can move the
    // document layout — which is why a 40-second bootstrap costs nothing.
    expect(counters.resultDecoRebuilds).toBe(0);
    expect(counters.cardWidgetsConstructed).toBe(0);
    expect(counters.scrollCompensationFrames).toBe(0);
    expect(h.view.state.doc.toString()).toBe(before);
    h.destroy();
  });
});
