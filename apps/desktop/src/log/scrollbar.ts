/**
 * The synthetic scrollbar — 06 §15.2's "the 33 M-pixel problem".
 *
 * > 5 M lines × 18 px = 90 M px, and browsers clamp element height around
 * > 33.5 M px. So the log (and the data grid, §15.3) do **not** use a tall
 * > spacer div. Both use a shared synthetic scrollbar: our own track and thumb,
 * > position stored as an `f64` row index, wheel/trackpad/keyboard/drag all
 * > mapped to row deltas, with native `overscroll-behavior: contain` on the
 * > viewport. One component, two consumers.
 *
 * This module is the model half — no DOM, no framework, no pixels except the
 * ones the caller hands in. `log/view.tsx` is the log's consumer; W18's
 * `grid/scrollbar.ts` is the data editor's, and it imports from here rather than
 * re-deriving the arithmetic, because two implementations of "which row is the
 * thumb over" is exactly how a grid and a log come to disagree about where the
 * user is.
 *
 * # Why the position is a row index and not a pixel offset
 *
 * A pixel offset over 5 M lines is a number that only exists if a 90 M px
 * element exists, and it does not. The position here is an `f64` **row index**:
 * `1_234_567.5` is "halfway down line 1,234,567". Every input — wheel deltas,
 * a thumb drag, PageDown, a jump to a search hit — is converted to a row delta
 * before it touches state, so there is exactly one representation of "where am
 * I" and it is one the log can index with.
 *
 * # Why the geometry is computed and never stored
 *
 * {@link thumb} is a pure function of `(total, viewport, position, trackPx)`.
 * A stored thumb rectangle is a cache of four numbers that change on every
 * frame, and a stale one is a thumb that lies about the position — which, on a
 * 5 M-line log, is indistinguishable from a scroll bug.
 */

/** The height at which browsers clamp an element, near enough. 06 §15.2. */
export const ELEMENT_HEIGHT_CAP_PX = 33_554_400;

/** 06 §15.2's default cap on the Rust-side ring. */
export const DEFAULT_LOG_CAP_LINES = 5_000_000;

/** Below this the thumb stops shrinking, or it becomes impossible to grab. */
export const MIN_THUMB_PX = 24;

export interface ScrollMetrics {
  /** Total rows in the whole buffer, not just the resident window. */
  readonly total: number;
  /** How many rows fit in the viewport. Fractional is fine and usual. */
  readonly viewport: number;
  /** Top row, as an `f64` index. `0` is the first line. */
  readonly position: number;
}

export interface ThumbGeometry {
  /** Thumb offset from the top of the track, in CSS px. */
  readonly offset: number;
  /** Thumb length, in CSS px. */
  readonly size: number;
  /** True when everything fits and the track should not be drawn at all. */
  readonly hidden: boolean;
}

/** The largest row index the viewport may sit at without showing past the end. */
export function maxPosition(m: Pick<ScrollMetrics, "total" | "viewport">): number {
  return Math.max(0, m.total - m.viewport);
}

/** Clamps a candidate position into `[0, maxPosition]`. Never returns NaN. */
export function clampPosition(m: Pick<ScrollMetrics, "total" | "viewport">, next: number): number {
  if (!Number.isFinite(next)) return 0;
  return Math.min(maxPosition(m), Math.max(0, next));
}

/**
 * Thumb offset and length for a track of `trackPx`.
 *
 * The thumb has a floor of {@link MIN_THUMB_PX}, which means the travel
 * available to it is `trackPx - size` rather than `trackPx` — using the full
 * track with a floored thumb is the bug where the last 40 rows of a 5 M-line
 * log are unreachable because the thumb hits the bottom early.
 */
export function thumb(m: ScrollMetrics, trackPx: number): ThumbGeometry {
  const total = Math.max(0, m.total);
  const viewport = Math.max(0, m.viewport);
  if (trackPx <= 0 || total <= 0 || viewport >= total) {
    return { offset: 0, size: trackPx, hidden: true };
  }
  const size = Math.max(MIN_THUMB_PX, Math.min(trackPx, (viewport / total) * trackPx));
  const travel = Math.max(0, trackPx - size);
  const max = maxPosition({ total, viewport });
  const fraction = max <= 0 ? 0 : clampPosition({ total, viewport }, m.position) / max;
  return { offset: fraction * travel, size, hidden: false };
}

/** The inverse of {@link thumb}: a thumb offset in px back to a row index. */
export function positionForThumbOffset(
  m: Pick<ScrollMetrics, "total" | "viewport">,
  trackPx: number,
  offsetPx: number,
): number {
  const size = thumb({ ...m, position: 0 }, trackPx).size;
  const travel = Math.max(0, trackPx - size);
  if (travel <= 0) return 0;
  return clampPosition(m, (offsetPx / travel) * maxPosition(m));
}

/**
 * A `wheel` event's delta, in rows.
 *
 * `deltaMode` is the whole reason this is a function. Firefox reports
 * `DOM_DELTA_LINE` (1) for a mouse wheel and `DOM_DELTA_PIXEL` (0) for a
 * trackpad; WebKit reports pixels for both; `DOM_DELTA_PAGE` (2) exists and is
 * rare but real. Treating a line delta as pixels scrolls three rows instead of
 * three hundred, which reads as a dead wheel.
 */
export function rowsForWheel(
  event: Pick<WheelEvent, "deltaY" | "deltaMode">,
  lineHeightPx: number,
  viewportRows: number,
): number {
  switch (event.deltaMode) {
    case 1:
      return event.deltaY;
    case 2:
      return event.deltaY * viewportRows;
    default:
      return lineHeightPx <= 0 ? 0 : event.deltaY / lineHeightPx;
  }
}

/** The keyboard verbs a scrollable surface owes, as row deltas. */
export type ScrollKey = "up" | "down" | "pageUp" | "pageDown" | "home" | "end";

/**
 * Applies a keyboard verb. Page steps keep one row of overlap, which is what
 * every terminal and pager does and what makes a paged read continuous rather
 * than a slideshow.
 */
export function applyKey(m: ScrollMetrics, key: ScrollKey): number {
  const page = Math.max(1, Math.floor(m.viewport) - 1);
  switch (key) {
    case "up":
      return clampPosition(m, m.position - 1);
    case "down":
      return clampPosition(m, m.position + 1);
    case "pageUp":
      return clampPosition(m, m.position - page);
    case "pageDown":
      return clampPosition(m, m.position + page);
    case "home":
      return 0;
    case "end":
      return maxPosition(m);
  }
}

/**
 * Would a spacer-div virtualiser survive this many rows? Exported so the claim
 * in 06 §15.2 is a test rather than a comment: `log.test.ts` asserts that the
 * default 5 M-line cap at the shipped line height does NOT fit, which is the
 * reason this module exists.
 */
export function fitsInASpacerDiv(rows: number, lineHeightPx: number): boolean {
  return rows * lineHeightPx <= ELEMENT_HEIGHT_CAP_PX;
}
