/**
 * **Q7 — scroll-anchor jitter at 120 Hz with several cards resizing per frame.**
 *
 * WebKit, our macOS target, has no CSS `overflow-anchor`, so a card growing
 * above the viewport pushes the content under the reader's eyes. 06 §4.6
 * compensates in a `requestAnimationFrame`; the open question asked whether that
 * is jitter-free at 120 Hz when several cards resize in one frame, with a
 * documented fallback of deferring above-viewport changes until 200 ms idle.
 *
 * # What was measured, and the answer
 *
 * Jitter is not a function of frame rate. It is a function of how many times
 * `scrollTop` is written per frame with different values — two writes in one
 * frame IS the visible jump, and that is what a per-observer callback that
 * compensates on its own produces. So the counters are:
 *
 *   * `writes` per flush, over 120 frames with eight cards resizing in each:
 *     never above 1;
 *   * accumulated correction against accumulated above-viewport growth: equal,
 *     so the anchor line does not drift over 120 frames;
 *   * `deferrals`: 0 — the documented fallback never had to engage.
 *
 * **Verdict: batching resolves Q7.** The fallback stays compiled in, because a
 * WebKit build that reflows synchronously inside `ResizeObserver` could still
 * land the batch late, and the second test proves the fallback itself does not
 * lose pixels when it does engage.
 *
 * Frame durations are recorded and asserted on by nothing (ADR-017).
 */

import { describe, expect, it } from "vitest";
import { DEFER_THRESHOLD, ScrollAnchorEngine } from "./scrollAnchor";

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

/** Eight cards, the measured worst case: a whole-file run with the view at the bottom. */
const CARDS = 8;
const FRAMES = 120;

describe("Q7 — scroll anchoring at 120 Hz", () => {
  it("writes scrollTop at most once per frame and never drifts", () => {
    const engine = new ScrollAnchorEngine();
    const heights = new Array<number>(CARDS).fill(100);
    for (let card = 0; card < CARDS; card++) engine.seed(card, heights[card] as number);

    let expectedTotal = 0;
    let maxWrites = 0;
    const started = performance.now();

    for (let frame = 0; frame < FRAMES; frame++) {
      for (let card = 0; card < CARDS; card++) {
        // Every card grows, every frame, all of them above the viewport — the
        // worst case the question describes.
        const grew = (heights[card] as number) + 7;
        heights[card] = grew;
        expectedTotal += 7;
        engine.record({ key: card, height: grew, aboveViewport: true });
      }
      const result = engine.flush(false);
      maxWrites = Math.max(maxWrites, result.writes);
      // One write, carrying the whole frame's growth. Not eight.
      expect(result.writes).toBeLessThanOrEqual(1);
      expect(result.delta).toBe(CARDS * 7);
    }

    const ms = performance.now() - started;
    expect(maxWrites).toBe(1);
    expect(engine.frames).toBe(FRAMES);
    // No drift: the anchor line is exactly where it started, relative to content.
    expect(engine.applied).toBe(expectedTotal);
    expect(engine.deferrals).toBe(0);
    expect(engine.carrying).toBe(0);

    record(
      `[Q7] ${FRAMES} frames x ${CARDS} cards: ${engine.frames} writes, ` +
        `${engine.applied}px corrected, ${ms.toFixed(2)} ms total`,
    );
  });

  it("compensates for above-viewport growth only", () => {
    const engine = new ScrollAnchorEngine();
    engine.seed("above", 100);
    engine.seed("below", 100);
    engine.record({ key: "above", height: 400, aboveViewport: true });
    engine.record({ key: "below", height: 900, aboveViewport: false });

    const result = engine.flush(false);
    // A card growing below the fold moves nothing the reader can see;
    // compensating for it would scroll the document out from under them.
    expect(result.delta).toBe(300);
  });

  it("the fallback defers without losing pixels", () => {
    const engine = new ScrollAnchorEngine();
    const cards = DEFER_THRESHOLD + 3;
    for (let card = 0; card < cards; card++) engine.seed(card, 100);
    for (let card = 0; card < cards; card++) {
      engine.record({ key: card, height: 150, aboveViewport: true });
    }

    const busy = engine.flush(false);
    expect(busy.writes).toBe(0);
    expect(busy.deferred).toBe(cards);
    expect(engine.carrying).toBe(cards * 50);

    // 200 ms later, idle. The carried correction is applied in full: deferring is
    // a delay, never a discard — dropping it is exactly how the anchor drifts,
    // which is the failure the whole mechanism exists to prevent.
    const idle = engine.flush(true);
    expect(idle.writes).toBe(1);
    expect(idle.delta).toBe(cards * 50);
    expect(engine.carrying).toBe(0);
  });

  it("does nothing at all when no height changed", () => {
    const engine = new ScrollAnchorEngine();
    engine.seed("a", 100);
    engine.record({ key: "a", height: 100, aboveViewport: true });
    const result = engine.flush(false);
    expect(result).toEqual({ delta: 0, writes: 0, deferred: 0 });
    expect(engine.frames).toBe(0);
  });
});
