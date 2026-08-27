/**
 * "One component, two consumers" — 06 §15.2, and W16's acceptance bullet.
 *
 * `log/scrollbar.ts` states the contract in its own header: W18's
 * `grid/scrollbar.ts` "imports from here rather than re-deriving the arithmetic,
 * because two implementations of 'which row is the thumb over' is exactly how a
 * grid and a log come to disagree about where the user is."
 *
 * Two implementations would not fail either unit's tests — each would test its
 * own copy — so this file asserts the sharing itself, twice over:
 *
 *  1. **By construction.** The module's source imports the log's arithmetic and
 *     declares none of its own.
 *  2. **By behaviour.** Every thumb the grid's bar renders is compared to
 *     `thumb()` called directly, at 10 M rows, at both ends of the track.
 *
 * The second is the one that survives a refactor; the first is the one that
 * catches the refactor that reintroduces the copy.
 */

import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { beforeEach, describe, expect, test } from "vitest";
import * as log from "../log/scrollbar";
import { MIN_THUMB_PX, SyntheticScrollbar, type WheelLike, wheelToScroll } from "./scrollbar";

/** 10 M observations, a 20-row viewport, a 400 px track. */
const TOTAL = 10_000_000;
const VIEWPORT = 20;
const TRACK = 400;

function bar(onScroll: (position: number) => void = () => {}): SyntheticScrollbar {
  return new SyntheticScrollbar({ orientation: "vertical", doc: document, onScroll });
}

beforeEach(() => {
  // jsdom implements neither pointer capture method. The scrollbar's drag path
  // is the thing under test, not the capture, so they are stubbed rather than
  // routed around — routing around them would be testing a different code path.
  const proto = Element.prototype as unknown as Record<string, unknown>;
  proto.setPointerCapture ??= () => {};
  proto.releasePointerCapture ??= () => {};
  proto.hasPointerCapture ??= () => false;
});

describe("the arithmetic is imported, not re-derived", () => {
  test("the module imports the log's scrollbar and declares no geometry of its own", () => {
    const here = dirname(fileURLToPath(import.meta.url));
    const source = readFileSync(resolve(here, "scrollbar.ts"), "utf8");

    expect(source).toMatch(/from "\.\.\/log\/scrollbar"/);
    // A second `MIN_THUMB_PX = 24`, or a second thumb formula, is the drift.
    expect(source).not.toMatch(/const MIN_THUMB_PX\s*=/);
    expect(source).not.toMatch(/function thumb\s*\(/);
    expect(source).not.toMatch(/function (positionForThumbOffset|clampPosition|maxPosition)\s*\(/);
    expect(MIN_THUMB_PX).toBe(log.MIN_THUMB_PX);
  });

  test("every rendered thumb equals thumb() called directly, at 10 M rows", () => {
    const scrollbar = bar();
    for (const position of [0, 1, 999, 5_000_000, TOTAL - VIEWPORT - 1, TOTAL - VIEWPORT]) {
      scrollbar.update(position, VIEWPORT, TOTAL, TRACK);
      const expected = log.thumb({ total: TOTAL, viewport: VIEWPORT, position }, TRACK);
      expect(scrollbar.geometry).toEqual(expected);
      expect(scrollbar.element.querySelector(".grid-scrollbar__thumb")).not.toBeNull();
      const thumbEl = scrollbar.element.firstElementChild as HTMLElement;
      expect(thumbEl.style.height).toBe(`${expected.size}px`);
      expect(thumbEl.style.transform).toBe(`translateY(${expected.offset}px)`);
    }
  });

  test("the thumb floors at MIN_THUMB_PX, and the last observation stays reachable", () => {
    const scrollbar = bar();
    scrollbar.update(0, VIEWPORT, TOTAL, TRACK);
    // 20 / 10 000 000 × 400 px is 0.0008 px. Without the floor the thumb would
    // be ungrabbable; with the floor, the travel is `track - size` and not
    // `track`, or the last 40 rows would be unreachable.
    expect(scrollbar.geometry.size).toBe(MIN_THUMB_PX);
    expect(scrollbar.max).toBe(TOTAL - VIEWPORT);

    scrollbar.update(TOTAL - VIEWPORT, VIEWPORT, TOTAL, TRACK);
    expect(scrollbar.geometry.offset).toBeCloseTo(TRACK - MIN_THUMB_PX, 10);
    // The inverse agrees: the bottom of the travel is the last legal position.
    expect(
      log.positionForThumbOffset({ total: TOTAL, viewport: VIEWPORT }, TRACK, TRACK - MIN_THUMB_PX),
    ).toBe(TOTAL - VIEWPORT);
  });

  test("the maximum comes from maxPosition and is never passed in", () => {
    const scrollbar = bar();
    scrollbar.update(0, VIEWPORT, TOTAL, TRACK);
    expect(scrollbar.max).toBe(log.maxPosition({ total: TOTAL, viewport: VIEWPORT }));
    // Everything fits: the model says so and the bar hides itself.
    scrollbar.update(0, 100, 40, TRACK);
    expect(scrollbar.max).toBe(0);
    expect(scrollbar.geometry.hidden).toBe(true);
    expect(scrollbar.element.style.visibility).toBe("hidden");
  });
});

describe("the position is clamped, and announced as a row number", () => {
  test("out-of-range positions clamp exactly as clampPosition does", () => {
    const scrollbar = bar();
    scrollbar.update(-5, VIEWPORT, TOTAL, TRACK);
    expect(scrollbar.position).toBe(0);
    scrollbar.update(Number.POSITIVE_INFINITY, VIEWPORT, TOTAL, TRACK);
    expect(scrollbar.position).toBe(0); // `clampPosition` refuses non-finite input
    scrollbar.update(TOTAL * 2, VIEWPORT, TOTAL, TRACK);
    expect(scrollbar.position).toBe(TOTAL - VIEWPORT);
  });

  test("a screen reader is told row 8 400 000 of 10 000 000, not 84 %", () => {
    const scrollbar = bar();
    scrollbar.update(8_400_000, VIEWPORT, TOTAL, TRACK);
    expect(scrollbar.element.getAttribute("role")).toBe("scrollbar");
    expect(scrollbar.element.getAttribute("aria-orientation")).toBe("vertical");
    expect(scrollbar.element.getAttribute("aria-valuenow")).toBe("8400000");
    expect(scrollbar.element.getAttribute("aria-valuemin")).toBe("0");
    expect(scrollbar.element.getAttribute("aria-valuemax")).toBe(String(TOTAL - VIEWPORT));
  });
});

describe("input", () => {
  test("a click on the track pages by one viewport, towards the pointer", () => {
    const seen: number[] = [];
    const scrollbar = bar((p) => seen.push(p));
    scrollbar.update(1_000_000, VIEWPORT, TOTAL, TRACK);
    // jsdom reports every rect as zero, so a click at y = 300 is 300 px down the
    // track, which is below a thumb sitting at ~40 px.
    scrollbar.element.dispatchEvent(new MouseEvent("pointerdown", { clientY: 300 }));
    expect(seen).toEqual([1_000_000 + VIEWPORT]);
  });

  test("dragging the thumb runs through positionForThumbOffset", () => {
    const seen: number[] = [];
    const scrollbar = bar((p) => seen.push(p));
    scrollbar.update(0, VIEWPORT, TOTAL, TRACK);
    const thumbEl = scrollbar.element.firstElementChild as HTMLElement;

    thumbEl.dispatchEvent(new MouseEvent("pointerdown", { clientY: 0, bubbles: true }));
    thumbEl.dispatchEvent(new MouseEvent("pointermove", { clientY: 100, bubbles: true }));

    const expected = log.positionForThumbOffset({ total: TOTAL, viewport: VIEWPORT }, TRACK, 100);
    expect(seen).toEqual([expected]);
    // 100 px of a 376 px travel over ten million rows is a specific observation,
    // and it is the same one the log would scroll to.
    expect(expected).toBeGreaterThan(2_600_000);
    expect(scrollbar.position).toBe(expected);
  });

  test("a wheel delta is the log's own mapping, in all three deltaModes", () => {
    const rowHeight = 22;
    const visible = 20;
    for (const mode of [0, 1, 2]) {
      const event: WheelLike = { deltaX: 30, deltaY: 90, deltaMode: mode };
      expect(wheelToScroll(event, rowHeight, visible).rows).toBe(
        log.rowsForWheel(event, rowHeight, visible),
      );
    }
    // A LINE delta is rows outright; a PIXEL delta is divided by the row height.
    // Treating one as the other moves three rows instead of three hundred.
    expect(wheelToScroll({ deltaX: 0, deltaY: 3, deltaMode: 1 }, rowHeight, visible).rows).toBe(3);
    expect(wheelToScroll({ deltaX: 0, deltaY: 66, deltaMode: 0 }, rowHeight, visible).rows).toBe(3);
    expect(wheelToScroll({ deltaX: 0, deltaY: 1, deltaMode: 2 }, rowHeight, visible).rows).toBe(20);
    // The horizontal half is pixels of column, which the log has no analogue for.
    expect(wheelToScroll({ deltaX: 12, deltaY: 0, deltaMode: 0 }, rowHeight, visible).x).toBe(12);
    expect(wheelToScroll({ deltaX: 2, deltaY: 0, deltaMode: 1 }, rowHeight, visible).x).toBe(44);
  });
});

describe("06 §15.2's premise, restated for the grid", () => {
  test("10 M rows at 22 px do not fit in an element, which is why this exists", () => {
    // The log asserts this for 5 M lines at 18 px; the grid's numbers are worse.
    expect(log.fitsInASpacerDiv(10_000_000, 22)).toBe(false);
    expect(10_000_000 * 22).toBeGreaterThan(log.ELEMENT_HEIGHT_CAP_PX);
  });
});
