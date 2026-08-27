/**
 * The synthetic scrollbar's VIEW — 06 §15.2, shared with the log window.
 *
 * **Why not a native one.** "5 M lines × 18 px = 90 M px, and browsers clamp
 * element height around 33.5 M px. So the log (and the data grid, §15.3) do
 * **not** use a tall spacer div. Both use a shared synthetic scrollbar: our own
 * track and thumb, position stored as an `f64` row index, wheel/trackpad/
 * keyboard/drag all mapped to row deltas."
 *
 * 10 M rows × 22 px is 220 M px, six times the clamp. A native scrollbar over a
 * spacer that tall does not merely scroll badly — the browser silently rescales
 * it, so row 9 000 000 and row 9 400 000 are the same thumb pixel and there is no
 * way to reach a specific observation. An `f64` row index has 53 bits of
 * mantissa: exact at 10 M rows, exact at 10 B.
 *
 * **"One component, two consumers" is an import, not a resemblance.** Every
 * number in this file comes from `log/scrollbar.ts` — `thumb`,
 * `positionForThumbOffset`, `clampPosition`, `maxPosition`, `rowsForWheel` and
 * `MIN_THUMB_PX`. W16's module says why in its own header: "two implementations
 * of 'which row is the thumb over' is exactly how a grid and a log come to
 * disagree about where the user is." So this file contributes the DOM half — a
 * track, a thumb, pointer capture, the ARIA numbers — and contributes no
 * arithmetic at all. `scrollbar.test.ts` asserts that by comparing every
 * rendered thumb against `thumb()` directly, at 10 M rows.
 *
 * The class knows nothing about rows: the Data Editor passes row indices for the
 * vertical bar and pixels for the horizontal one, and the model is the same one
 * the log uses for lines.
 */

import {
  clampPosition,
  maxPosition,
  positionForThumbOffset,
  rowsForWheel,
  thumb,
} from "../log/scrollbar";

/**
 * Re-exported, never re-declared. A second `MIN_THUMB_PX = 24` in this file — or
 * a second `thumb()` — is the first step of the drift W16's header describes, so
 * a consumer of the grid's scrollbar reaches the log's arithmetic through here
 * and there is exactly one copy of it in the app.
 */
export {
  MIN_THUMB_PX,
  clampPosition,
  maxPosition,
  positionForThumbOffset,
  rowsForWheel,
  thumb,
} from "../log/scrollbar";
export type { ScrollMetrics } from "../log/scrollbar";

export interface ScrollbarOptions {
  orientation: "vertical" | "horizontal";
  /** Called with the new position in the caller's own units (rows, or px). */
  onScroll: (position: number) => void;
  /** For `aria-controls`; a scrollbar with no controlled element is unusable. */
  controls?: string;
  label?: string;
  doc?: Document;
}

export class SyntheticScrollbar {
  readonly element: HTMLElement;
  private readonly thumbEl: HTMLElement;
  private readonly options: ScrollbarOptions;
  /**
   * The model, mutated in place. `total` may be 10 M; neither it nor `viewport`
   * is ever iterated or turned into a pixel count. Structurally a
   * {@link ScrollMetrics}, so it is passed to W16's pure functions as-is and no
   * frame allocates an argument object.
   */
  private readonly m = { total: 1, viewport: 1, position: 0 };
  private trackPx = 0;
  private dragFrom: { pointer: number; offset: number } | undefined;
  private readonly listeners: (() => void)[] = [];

  constructor(options: ScrollbarOptions) {
    this.options = options;
    const doc = options.doc ?? document;
    const vertical = options.orientation === "vertical";

    this.element = doc.createElement("div");
    this.element.className = `grid-scrollbar grid-scrollbar--${options.orientation}`;
    this.element.setAttribute("role", "scrollbar");
    this.element.setAttribute("aria-orientation", options.orientation);
    this.element.setAttribute("aria-label", options.label ?? (vertical ? "Rows" : "Columns"));
    if (options.controls !== undefined)
      this.element.setAttribute("aria-controls", options.controls);
    this.element.tabIndex = -1;

    this.thumbEl = doc.createElement("div");
    this.thumbEl.className = "grid-scrollbar__thumb";
    this.element.appendChild(this.thumbEl);

    const onPointerDown = (event: PointerEvent): void => {
      if (event.target === this.thumbEl) {
        this.dragFrom = {
          pointer: vertical ? event.clientY : event.clientX,
          // The offset the drag starts from, from the shared geometry — so a
          // drag and the paint that follows it cannot disagree by a pixel.
          offset: thumb(this.m, this.trackPx).offset,
        };
        this.thumbEl.setPointerCapture(event.pointerId);
        event.preventDefault();
        return;
      }
      // A click on the track pages towards the pointer, as every platform does.
      const rect = this.element.getBoundingClientRect();
      const at = vertical ? event.clientY - rect.top : event.clientX - rect.left;
      const geometry = thumb(this.m, this.trackPx);
      this.emit(this.m.position + (at < geometry.offset ? -this.m.viewport : this.m.viewport));
    };

    const onPointerMove = (event: PointerEvent): void => {
      const from = this.dragFrom;
      if (from === undefined) return;
      const moved = (vertical ? event.clientY : event.clientX) - from.pointer;
      // `positionForThumbOffset` is the inverse of `thumb`, and using it rather
      // than a locally derived px-per-row is what keeps the two exact inverses.
      this.emit(positionForThumbOffset(this.m, this.trackPx, from.offset + moved));
    };

    const onPointerUp = (event: PointerEvent): void => {
      if (this.dragFrom === undefined) return;
      this.dragFrom = undefined;
      if (this.thumbEl.hasPointerCapture(event.pointerId)) {
        this.thumbEl.releasePointerCapture(event.pointerId);
      }
    };

    this.element.addEventListener("pointerdown", onPointerDown);
    this.element.addEventListener("pointermove", onPointerMove);
    this.element.addEventListener("pointerup", onPointerUp);
    this.element.addEventListener("pointercancel", onPointerUp);
    this.listeners.push(
      () => this.element.removeEventListener("pointerdown", onPointerDown),
      () => this.element.removeEventListener("pointermove", onPointerMove),
      () => this.element.removeEventListener("pointerup", onPointerUp),
      () => this.element.removeEventListener("pointercancel", onPointerUp),
    );
  }

  /** The largest legal position, from W16's `maxPosition` and from nowhere else. */
  get max(): number {
    return maxPosition(this.m);
  }

  get position(): number {
    return this.m.position;
  }

  /** The live geometry. Exported for the drift test, and for nothing else. */
  get geometry(): { offset: number; size: number; hidden: boolean } {
    return thumb(this.m, this.trackPx);
  }

  /**
   * Re-states the model.
   *
   * There is deliberately no `max` parameter: the caller passing its own maximum
   * alongside `total` and `visible` is the second implementation this file exists
   * not to have. `max` is `maxPosition(total, viewport)`, always.
   */
  update(position: number, visible: number, total: number, trackPx: number): void {
    this.m.total = Math.max(0, total);
    this.m.viewport = Math.max(0, visible);
    this.m.position = clampPosition(this.m, position);
    this.trackPx = Math.max(0, trackPx);
    this.render();
  }

  private render(): void {
    const geometry = thumb(this.m, this.trackPx);
    if (this.options.orientation === "vertical") {
      this.thumbEl.style.height = `${geometry.size}px`;
      this.thumbEl.style.transform = `translateY(${geometry.offset}px)`;
    } else {
      this.thumbEl.style.width = `${geometry.size}px`;
      this.thumbEl.style.transform = `translateX(${geometry.offset}px)`;
    }
    this.element.style.visibility = geometry.hidden ? "hidden" : "";
    // Screen readers get the TRUE numbers, not a 0–100 percentage: "row 8 400 000
    // of 10 000 000" is the fact, and rounding it to 84 % throws away exactly the
    // information a 10 M-row grid exists to give.
    this.element.setAttribute("aria-valuenow", String(Math.round(this.m.position)));
    this.element.setAttribute("aria-valuemin", "0");
    this.element.setAttribute("aria-valuemax", String(Math.round(maxPosition(this.m))));
  }

  private emit(next: number): void {
    const clamped = clampPosition(this.m, next);
    if (clamped === this.m.position) return;
    this.m.position = clamped;
    this.render();
    this.options.onScroll(clamped);
  }

  dispose(): void {
    for (const off of this.listeners) off();
    this.element.remove();
  }
}

// ---------------------------------------------------------------------------
// Wheel and trackpad
// ---------------------------------------------------------------------------

/** The subset of `WheelEvent` the mapping reads. */
export interface WheelLike {
  deltaX: number;
  deltaY: number;
  /** 0 = pixel, 1 = line, 2 = page. */
  deltaMode: number;
}

/**
 * Wheel/trackpad delta → a row delta and a pixel delta.
 *
 * The vertical half is `rowsForWheel` verbatim: `deltaMode` handling is not
 * decoration (Firefox reports LINE for a real wheel and PIXEL for a trackpad,
 * and treating a line delta as pixels moves three rows instead of three hundred)
 * and the log already solved it. The horizontal half is the grid's own, because
 * the log does not scroll sideways in rows — `deltaX` is pixels of column, and a
 * LINE delta there is one row height of them.
 */
export function wheelToScroll(
  event: WheelLike,
  rowHeight: number,
  visibleRows: number,
): { rows: number; x: number } {
  const rows = rowsForWheel(event, rowHeight, visibleRows);
  switch (event.deltaMode) {
    case 1:
      return { rows, x: event.deltaX * rowHeight };
    case 2:
      return { rows, x: event.deltaX * visibleRows * rowHeight };
    default:
      return { rows, x: event.deltaX };
  }
}
