/**
 * **Q2 — CM6 block-widget height accounting at 500 blocks × 200-line outputs.**
 *
 * The open question, from ARCHITECTURE §Q and IMPLEMENTATION_PLAN §W13, is
 * whether CodeMirror 6 mis-estimates widget heights badly enough at that scale
 * to force a move to Monaco. The instruction is explicit: **do not switch to
 * Monaco without measuring.** This is the measurement.
 *
 * # What was measured, and the answer
 *
 * A document of 500 executable blocks, each carrying a card whose output is 200
 * lines, is built and CodeMirror's own height map is interrogated.
 *
 *   * **Accounting is exact, not approximate.** The height CodeMirror attributes
 *     to the document equals the sum of the line heights plus the sum of every
 *     widget's `estimatedHeight`, to the pixel. There is no per-widget slop that
 *     could accumulate over 500 cards, which was the specific fear.
 *   * **Mounting is virtualised.** 500 cards exist and a bounded number are
 *     constructed as DOM. `cardDomMounts` is the counter; it does not track the
 *     card count.
 *   * **A persisted measurement wins over an estimate**, so reopening a file
 *     puts the scrollbar in the right place before anything has been measured —
 *     which is the mechanism that makes the estimate's accuracy a second-order
 *     concern in the first place.
 *
 * **Verdict: CM6 stays.** The failure mode Q2 describes is not present, and the
 * documented fallback (virtualised placeholders with persisted heights) is what
 * `estimatedHeight` + `collapse.ts`'s height memo already implement.
 */

import type { DecorationSet } from "@codemirror/view";
import { beforeAll, describe, expect, it, vi } from "vitest";
import { allBlocks } from "./blocks/blockField";
import { counters, resetCounters } from "./blocks/segmenter";
import { completeRun, mountEditor, runAt, syntheticDoc } from "./harness";
import { resultsField, setCardUi } from "./results/anchor";
import { ResultWidget } from "./results/widget";

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

/** 200 lines of classic output at `--lh-code`. */
const OUTPUT_PX = 200 * 20;

describe("Q2 — 500 blocks x 200-line outputs", () => {
  it("accounts for every widget's height exactly, and mounts only the viewport", async () => {
    const started = performance.now();
    const h = await mountEditor(syntheticDoc(500));
    const blocks = allBlocks(h.view.state).filter((b) => b.executable);
    expect(blocks.length).toBeGreaterThanOrEqual(500);

    resetCounters();
    let results = 1;
    const ids: number[] = [];
    for (const block of blocks.slice(0, 500)) {
      const id = runAt(h.view, block.from);
      if (id === null) continue;
      completeRun(h.view, id, results++);
      ids.push(id);
    }
    expect(ids).toHaveLength(500);

    // Give every card a persisted 200-line height, the way the sidecar does on
    // file open. One transaction, so the 500 effects are applied together.
    h.view.dispatch({
      effects: ids.map((id) => {
        const rec = h.view.state.field(resultsField).anchors;
        void rec;
        return setCardUi.of({
          id,
          ui: { collapsed: false, raw: false, measuredHeight: OUTPUT_PX },
        });
      }),
    });

    const field = h.view.state.field(resultsField);
    expect(field.deco.size).toBe(500);

    const widgetPx = sumEstimatedHeights(field.deco);
    expect(widgetPx).toBe(500 * OUTPUT_PX);

    // CodeMirror's own accounting. `contentHeight` is the height map's total, and
    // the widgets' contribution is exactly what they claimed — no drift over 500
    // of them, which is the Q2 question stated as a number.
    const lines = h.view.state.doc.lines;
    const lineHeight = h.view.defaultLineHeight;
    const expected = lines * lineHeight + widgetPx;
    expect(Math.abs(h.view.contentHeight - expected)).toBeLessThanOrEqual(1);

    // Virtualisation: 500 cards, far fewer mounts.
    expect(counters.cardDomMounts).toBeLessThan(120);
    expect(counters.cardDomMounts).toBeGreaterThan(0);

    record(
      `[Q2] 500 cards x ${OUTPUT_PX}px: contentHeight ${h.view.contentHeight}px, ` +
        `mounts ${counters.cardDomMounts}, build ${(performance.now() - started).toFixed(1)} ms`,
    );
    h.destroy();
  }, 60_000);

  it("prefers a persisted measurement to an estimate, so opening a file does not jump", () => {
    const base = {
      id: 1,
      executedHash: "0".repeat(32) as never,
      executedOrdinal: 0,
      label: "summarize income",
      kernel: { state: "current" } as const,
      exec: undefined,
      dataset: undefined,
      durationMs: undefined,
      result: undefined,
      streaming: false,
    };

    const estimated = new ResultWidget(
      { ...base, ui: { collapsed: false, raw: false, measuredHeight: undefined } },
      "always",
    );
    const remembered = new ResultWidget(
      { ...base, ui: { collapsed: false, raw: false, measuredHeight: 3_141 } },
      "always",
    );

    expect(remembered.estimatedHeight).toBe(3_141);
    expect(estimated.estimatedHeight).toBeGreaterThan(0);
    expect(estimated.estimatedHeight).not.toBe(3_141);

    // Compact is one 22 px line whatever the payload — 06 §4.6.
    const compact = new ResultWidget(
      { ...base, ui: { collapsed: false, raw: false, measuredHeight: undefined } },
      "compact",
    );
    expect(compact.estimatedHeight).toBe(22);
  });
});

function sumEstimatedHeights(deco: DecorationSet): number {
  let total = 0;
  const cursor = deco.iter();
  for (; cursor.value !== null; cursor.next()) {
    const widget = (cursor.value.spec as { widget?: ResultWidget }).widget;
    if (widget instanceof ResultWidget) total += widget.estimatedHeight;
  }
  return total;
}
