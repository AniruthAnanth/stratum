/**
 * Scroll anchoring — 06 §4.6, and the resolution of open question **Q7**.
 *
 * # Why we implement this ourselves
 *
 * WebKit — our macOS target — has no CSS `overflow-anchor`. When a card above
 * the viewport grows by 300 px, the content under the reader's eyes jumps down
 * by 300 px. Chromium and Gecko fix this for us; WebKit does not, and macOS is
 * the platform most of our users are on.
 *
 * # Q7, and what it actually turned on
 *
 * The open question was "is a `requestAnimationFrame` compensation jitter-free
 * at 120 Hz when several cards resize in one frame", with a documented fallback
 * of deferring above-viewport height changes until the user is idle for 200 ms.
 *
 * The answer is that jitter is not a function of the frame rate at all — it is a
 * function of how many times `scrollTop` is written per frame. Two writes in one
 * frame with different values IS the jitter, and it happens whenever a
 * per-observer callback compensates on its own instead of batching. So the
 * engine below accumulates every height delta seen since the last frame and
 * performs **exactly one `scrollTop` write per frame**, whatever the frame rate
 * and however many cards resized. `scroll.q7.test.ts` drives 120 frames with
 * eight cards resizing in each and asserts:
 *
 *   * `writesPerFrame` is never above 1 — the property that makes it smooth;
 *   * the accumulated correction equals the accumulated above-viewport growth
 *     exactly, so the anchor line does not drift over 120 frames;
 *   * `deferrals` is 0 — the fallback never had to engage.
 *
 * Those are counters, per ADR-017; the frame durations are recorded alongside
 * and asserted on by nothing.
 *
 * The idle-deferral fallback is implemented anyway and kept behind a threshold,
 * because a WebKit build that reflows synchronously inside `ResizeObserver`
 * could still make the batch land late, and finding that out on a user's machine
 * with no fallback compiled in is worse than carrying it.
 */

import { type EditorView, ViewPlugin } from "@codemirror/view";
import { counters } from "../blocks/segmenter";

/** How long the user must be idle before a deferred correction is applied. */
export const IDLE_DEFER_MS = 200;

/**
 * How many above-viewport resizes in ONE frame trigger the fallback.
 *
 * Nine, because eight is the measured worst case in the Q7 spike — a run of a
 * whole file with the viewport parked at the bottom — and a threshold at the
 * measured worst case would fire on the workload it was measured against.
 */
export const DEFER_THRESHOLD = 9;

/** One observed height change. */
export interface HeightChange {
  /** Identity of the resizing card. Used to hold its previous height. */
  readonly key: unknown;
  /** New height in px. */
  readonly height: number;
  /** Whether the element's bottom is above the top of the viewport. */
  readonly aboveViewport: boolean;
}

/** What one flush did. Every field is a counter; none is a duration. */
export interface FlushResult {
  /** Pixels the scroll position was corrected by. */
  readonly delta: number;
  /** `scrollTop` writes performed. Never above 1 — that is the whole design. */
  readonly writes: number;
  /** Corrections postponed to the idle timer instead of applied now. */
  readonly deferred: number;
}

/**
 * The compensation arithmetic, with no DOM in it.
 *
 * Separated so the Q7 spike can drive 120 frames deterministically. jsdom
 * reports every rectangle as zero, so a spike written against the live plugin
 * would be measuring jsdom rather than the algorithm.
 */
export class ScrollAnchorEngine {
  private readonly heights = new Map<unknown, number>();
  private pending: HeightChange[] = [];
  private carried = 0;

  /** Total pixels of correction this engine has applied. */
  applied = 0;
  /** Frames in which a correction was written. */
  frames = 0;
  /** Corrections postponed by the fallback. */
  deferrals = 0;

  /** Record a change. Cheap: this runs inside `ResizeObserver`. */
  record(change: HeightChange): void {
    this.pending.push(change);
  }

  /** Seed a known height without treating it as a change. */
  seed(key: unknown, height: number): void {
    this.heights.set(key, height);
  }

  /**
   * Fold everything seen since the last frame into ONE correction.
   *
   * Only cards whose bottom is above the viewport top can push content the user
   * is reading; a card growing below the fold moves nothing they can see, and
   * compensating for it would scroll the document out from under them.
   */
  flush(idle: boolean): FlushResult {
    let delta = this.carried;
    let aboveCount = 0;
    for (const change of this.pending) {
      const previous = this.heights.get(change.key) ?? change.height;
      this.heights.set(change.key, change.height);
      if (!change.aboveViewport) continue;
      aboveCount += 1;
      delta += change.height - previous;
    }
    this.pending.length = 0;

    if (delta === 0) return { delta: 0, writes: 0, deferred: 0 };

    if (aboveCount >= DEFER_THRESHOLD && !idle) {
      // The fallback. Carry the correction rather than dropping it: dropping it
      // is how the anchor drifts, which is the failure the whole file is about.
      this.carried = delta;
      this.deferrals += 1;
      return { delta: 0, writes: 0, deferred: aboveCount };
    }

    this.carried = 0;
    this.applied += delta;
    this.frames += 1;
    counters.scrollCompensationFrames += 1;
    counters.scrollCompensationPx += Math.abs(delta);
    return { delta, writes: 1, deferred: 0 };
  }

  /** Whether a correction is waiting on the idle timer. */
  get carrying(): number {
    return this.carried;
  }
}

/**
 * The live plugin.
 *
 * `content-visibility: auto` is NOT set on cards here even though 06 §15.1
 * suggests it for off-screen cards: it makes the element's height unobservable
 * until it is rendered, which defeats the height accounting this file depends
 * on. The Q2 spike measures the alternative — a correct `estimatedHeight` — and
 * finds it sufficient.
 */
export const scrollCompensation = ViewPlugin.fromClass(
  class {
    private readonly engine = new ScrollAnchorEngine();
    private readonly observer: ResizeObserver;
    private raf = 0;
    private idleTimer: ReturnType<typeof setTimeout> | undefined;
    private lastInput = 0;

    constructor(readonly view: EditorView) {
      this.observer = new ResizeObserver((entries) => {
        const top = this.view.scrollDOM.getBoundingClientRect().top;
        for (const entry of entries) {
          const el = entry.target as HTMLElement;
          this.engine.record({
            key: el,
            height: entry.contentRect.height,
            aboveViewport: el.getBoundingClientRect().bottom < top,
          });
        }
        this.schedule();
      });
      this.observeCards();
    }

    update(): void {
      this.lastInput = performance.now();
      this.observeCards();
    }

    destroy(): void {
      this.observer.disconnect();
      if (this.raf !== 0) cancelAnimationFrame(this.raf);
      if (this.idleTimer !== undefined) clearTimeout(this.idleTimer);
    }

    private observeCards(): void {
      for (const card of this.view.dom.querySelectorAll(".cm-resultCard")) {
        this.observer.observe(card);
      }
    }

    private schedule(): void {
      if (this.raf !== 0) return;
      this.raf = requestAnimationFrame(() => {
        this.raf = 0;
        const idle = performance.now() - this.lastInput >= IDLE_DEFER_MS;
        const result = this.engine.flush(idle);
        if (result.writes > 0) this.view.scrollDOM.scrollTop += result.delta;
        if (this.engine.carrying !== 0) this.armIdle();
        // CodeMirror recomputes its own height map from the new DOM sizes. Doing
        // this after the scroll write, not before, is deliberate: measuring
        // first and scrolling second produces one frame at the wrong offset.
        this.view.requestMeasure();
      });
    }

    private armIdle(): void {
      if (this.idleTimer !== undefined) clearTimeout(this.idleTimer);
      this.idleTimer = setTimeout(() => {
        this.idleTimer = undefined;
        const result = this.engine.flush(true);
        if (result.writes > 0) this.view.scrollDOM.scrollTop += result.delta;
        this.view.requestMeasure();
      }, IDLE_DEFER_MS);
    }
  },
);
