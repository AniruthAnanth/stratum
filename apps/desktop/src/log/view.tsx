/**
 * The scrollback view — 06 §9.2, §15.2.
 *
 * One component over {@link LogWindow}, {@link LogFind} and the selection model.
 * It is the only place in the product that draws the classic log, so the
 * Results pane, the Viewer and a detached log window are the same pixels; a
 * second `<pre>` somewhere else would be a second answer to "what does Stata
 * output look like here".
 *
 * Three things it does that an ordinary scroller does not, all forced by 06:
 *
 *  * **No spacer div.** The scrollbar is ours (`scrollbar.ts`); the rows live in
 *    a `translateY`-offset flow of at most `viewport + 1` elements. 5 M lines ×
 *    20 px is 100 M px and the browser clamps near 33.5 M, so a spacer would
 *    quietly stop scrolling two-thirds of the way down a long session.
 *  * **`white-space: pre`, no wrapping by default.** Horizontal scroll, like
 *    Stata. Wrapping is a toggle and it is off, which is also the O(1) height
 *    path.
 *  * **Copy goes through Rust.** `Mod+C` here calls `log_copy`, so a selection
 *    that runs past the rendered window copies in full and byte-identically.
 */

import {
  For,
  Index,
  type JSX,
  Show,
  createEffect,
  createMemo,
  createSignal,
  onCleanup,
  onMount,
} from "solid-js";
import type { LogFind } from "./find";
import { segmentLine } from "./find";
import { rowsForWheel, thumb } from "./scrollbar";
import {
  type LogPoint,
  type LogSelection,
  applySelectionToDom,
  autoScrollRows,
  collapsedAt,
  columnsOnLine,
  copySelection,
  extendTo,
  isEmpty,
  lineRange,
  pointAt,
  residentLineLength,
} from "./selection";
import { type LogLine, LogWindow, type LogWindowOptions, lineText } from "./window";

// The `.smcl--*` ink table lives with the card renderer; importing the sheet
// rather than restating it is what makes a `{res}` value the same colour in the
// Results pane and in the Viewer. See `styleClassOf` at the foot of this file.
import "../renderers/log/log.css";
import "./log.css";

/**
 * A `LogWindow` plus a revision signal.
 *
 * The window is a plain class on purpose — it is used by `find.ts`, by tests
 * and (once W14 adopts it) by the Results pane, none of which want a reactive
 * runtime. This is the adapter: one signal bumped from the window's `onChange`,
 * so a component re-reads exactly when the model moved.
 */
export function createLogWindow(options: LogWindowOptions = {}): {
  window: LogWindow;
  revision: () => number;
} {
  const [revision, bump] = createSignal(0);
  const window = new LogWindow({
    ...options,
    onChange: () => {
      options.onChange?.();
      bump((v) => v + 1);
    },
  });
  return { window, revision };
}

export interface LogViewProps {
  window: LogWindow;
  revision: () => number;
  find?: LogFind;
  /** `aria-label` for the scroll region. "Results", "Viewer", … */
  label: string;
  /** 06 §9.2's right-click verbs, supplied by the pane that owns the log. */
  onContextMenu?: (event: MouseEvent, selection: LogSelection) => void;
}

/** `--lh-code`. Read from the element so an OS text-size change is honoured. */
const FALLBACK_LINE_HEIGHT = 20;
const FALLBACK_CHAR_WIDTH = 7.8;

export function LogView(props: LogViewProps): JSX.Element {
  let viewport: HTMLDivElement | undefined;
  let rows: HTMLDivElement | undefined;
  let probe: HTMLSpanElement | undefined;
  let track: HTMLDivElement | undefined;

  const [selection, setSelection] = createSignal<LogSelection>(collapsedAt({ line: 0, col: 0 }));
  const [dragging, setDragging] = createSignal(false);
  const [charWidth, setCharWidth] = createSignal(FALLBACK_CHAR_WIDTH);
  const [trackPx, setTrackPx] = createSignal(0);

  const win = (): LogWindow => props.window;
  const view = createMemo(() => {
    props.revision();
    return win().visibleLines();
  });

  // Viewport size in ROWS, not pixels: everything downstream of here is row
  // arithmetic, and converting once is what keeps it that way.
  const remeasure = (): void => {
    if (viewport === undefined) return;
    // `--lh-code` is read off the element, not assumed, because 06 §17 requires
    // OS text-size scaling to be honoured and the whole type scale moves with
    // `--fs-root`. A row grid that stayed at 20 px while the type grew would
    // shear the log against its own lines. `line-height: normal` and jsdom both
    // resolve to no px value, which is what the fallback is for.
    const declared = Number.parseFloat(getComputedStyle(viewport).lineHeight);
    const lh = Number.isFinite(declared) && declared > 0 ? declared : FALLBACK_LINE_HEIGHT;
    win().lineHeight = lh;
    const height = viewport.clientHeight;
    win().setViewport(Math.max(1, Math.floor(height / Math.max(1, lh))));
    setTrackPx(height);
    const measured = probe?.getBoundingClientRect().width ?? 0;
    if (measured > 0) setCharWidth(measured / 10);
    void win().ensureVisible();
  };

  onMount(() => {
    remeasure();
    const observer =
      typeof ResizeObserver === "undefined" ? undefined : new ResizeObserver(() => remeasure());
    if (viewport !== undefined) observer?.observe(viewport);
    onCleanup(() => observer?.disconnect());
  });

  // After every window shift the native Range is re-applied over whatever part
  // of the model is now rendered — 06 §15.2. Doing it in an effect keyed on the
  // revision means it also runs after a streamed append, which is when a
  // selection made mid-run would otherwise silently vanish.
  createEffect(() => {
    props.revision();
    selection();
    if (rows === undefined || isEmpty(selection())) return;
    const { first, lines } = view();
    applySelectionToDom(rows, selection(), { from: first, to: first + lines.length });
  });

  const pointFromEvent = (event: PointerEvent): LogPoint => {
    const rect = rows?.getBoundingClientRect();
    return pointAt({
      clientX: event.clientX,
      clientY: event.clientY,
      rowsTop: rect?.top ?? 0,
      lineHeight: win().lineHeight,
      firstLine: view().first,
      textLeft: rect?.left ?? 0,
      charWidth: charWidth(),
      lineLength: (line) => residentLineLength(win(), line),
      totalLines: Math.max(1, win().totalLines),
    });
  };

  const onPointerDown = (event: PointerEvent): void => {
    if (event.button !== 0) return;
    const point = pointFromEvent(event);
    if (event.detail >= 3) {
      setSelection(lineRange(point.line, point.line));
      return;
    }
    setSelection(event.shiftKey ? extendTo(selection(), point) : collapsedAt(point));
    setDragging(true);
    (event.currentTarget as Element | null)?.setPointerCapture?.(event.pointerId);
  };

  const onPointerMove = (event: PointerEvent): void => {
    if (!dragging()) return;
    const rect = viewport?.getBoundingClientRect();
    if (rect !== undefined) {
      const rows = autoScrollRows(event.clientY, rect.top, rect.bottom);
      if (rows !== 0) {
        win().scrollBy(rows);
        void win().ensureVisible();
      }
    }
    setSelection(extendTo(selection(), pointFromEvent(event)));
  };

  const endDrag = (event: PointerEvent): void => {
    if (!dragging()) return;
    setDragging(false);
    (event.currentTarget as Element | null)?.releasePointerCapture?.(event.pointerId);
  };

  const onWheel = (event: WheelEvent): void => {
    const rows = rowsForWheel(event, win().lineHeight, win().viewport);
    if (rows === 0) return;
    event.preventDefault();
    win().scrollBy(rows);
    void win().ensureVisible();
  };

  const onKeyDown = (event: KeyboardEvent): void => {
    const map: Record<string, Parameters<LogWindow["key"]>[0]> = {
      ArrowUp: "up",
      ArrowDown: "down",
      PageUp: "pageUp",
      PageDown: "pageDown",
      Home: "home",
      End: "end",
    };
    const verb = map[event.key];
    if (verb !== undefined) {
      event.preventDefault();
      win().key(verb);
      void win().ensureVisible();
      return;
    }
    // `Mod+C` — the only copy path, and it is Rust's. `document.execCommand`
    // and the browser's own copy would take the rendered 600 lines and no more.
    if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "c") {
      if (isEmpty(selection())) return;
      event.preventDefault();
      void copySelection(win(), selection()).then((text) => {
        void navigator.clipboard?.writeText?.(text);
      });
    }
  };

  const onTrackPointerDown = (event: PointerEvent): void => {
    const rect = track?.getBoundingClientRect();
    if (rect === undefined) return;
    const geometry = thumb(win().metrics(), rect.height);
    const y = event.clientY - rect.top;
    // Click above or below the thumb pages, as every native scrollbar does;
    // click ON the thumb starts a drag.
    if (y < geometry.offset) win().key("pageUp");
    else if (y > geometry.offset + geometry.size) win().key("pageDown");
    else {
      const grab = y - geometry.offset;
      const move = (e: PointerEvent): void => {
        const offset = e.clientY - rect.top - grab;
        const travel = Math.max(1, rect.height - geometry.size);
        win().scrollTo((offset / travel) * Math.max(0, win().totalLines - win().viewport));
        void win().ensureVisible();
      };
      const up = (): void => {
        globalThis.removeEventListener("pointermove", move);
        globalThis.removeEventListener("pointerup", up);
      };
      globalThis.addEventListener("pointermove", move);
      globalThis.addEventListener("pointerup", up);
    }
    void win().ensureVisible();
  };

  const geometry = createMemo(() => {
    props.revision();
    return thumb(win().metrics(), trackPx());
  });

  const offsetPx = createMemo(() => {
    props.revision();
    const fraction = win().position - Math.floor(win().position);
    return -fraction * win().lineHeight;
  });

  return (
    <div class="logv" data-log-view data-wrap={win().wrap ? "on" : "off"}>
      {/* Ten zeros in the log's own face. `ch` units would be simpler and are
          wrong on a face whose zero is not the advance width. */}
      <span class="logv__probe" ref={probe} aria-hidden="true">
        0000000000
      </span>

      {/* The scrollback IS the interactive surface — it takes arrow keys, Page
          keys and Mod+C — so it is a tab stop. A keyboard user with no way to
          focus it cannot read the log at all. */}
      <div
        class="logv__viewport"
        ref={viewport}
        tabindex={0}
        role="log"
        aria-label={props.label}
        aria-live="off"
        data-log-viewport
        onWheel={onWheel}
        onKeyDown={onKeyDown}
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={endDrag}
        onPointerCancel={endDrag}
        onContextMenu={(event) => props.onContextMenu?.(event, selection())}
      >
        <div class="logv__rows" ref={rows} style={{ transform: `translateY(${offsetPx()}px)` }}>
          <Index each={view().lines}>
            {(line, i) => (
              <LogRow
                line={line()}
                index={view().first + i}
                selection={selection()}
                find={props.find}
              />
            )}
          </Index>
        </div>
      </div>

      <Show when={!geometry().hidden}>
        {/* The track is pointer-only on purpose: the keyboard equivalent is on
            the viewport itself (Arrow/Page/Home/End), and a focusable track
            beside it would be a second tab stop for the same action. */}
        <div
          class="logv__track"
          ref={track}
          data-log-track
          onPointerDown={onTrackPointerDown}
          role="presentation"
        >
          <div
            class="logv__thumb"
            data-log-thumb
            style={{
              transform: `translateY(${geometry().offset}px)`,
              height: `${geometry().size}px`,
            }}
          />
        </div>
      </Show>
    </div>
  );
}

interface LogRowProps {
  line: LogLine | undefined;
  index: number;
  selection: LogSelection;
  find: LogFind | undefined;
}

/**
 * One physical line.
 *
 * A line still being fetched renders as an empty row of the right height rather
 * than as a spinner or a skeleton: 06 §15.1 budgets 16 ms to placeholders on a
 * 10 k-line jump, and the only thing that fits in 16 ms is nothing.
 */
function LogRow(props: LogRowProps): JSX.Element {
  const text = createMemo(() => (props.line === undefined ? "" : lineText(props.line)));

  const selected = createMemo(() => {
    const cols = columnsOnLine(props.selection, props.index, text().length);
    return cols !== undefined && cols.to > cols.from;
  });

  return (
    <div
      class="logv__line"
      data-log-line={props.index}
      data-selected={selected() ? "" : undefined}
      data-placeholder={props.line === undefined ? "" : undefined}
    >
      <Show when={props.line} fallback={<span class="logv__gap"> </span>}>
        {(line) => (
          <For each={line().runs}>
            {(run) => <RunSpans run={run} line={props.index} find={props.find} />}
          </For>
        )}
      </Show>
    </div>
  );
}

interface RunSpansProps {
  run: { text: string; style: unknown };
  line: number;
  find: LogFind | undefined;
}

/**
 * One styled run, split again around any search hits that fall inside it.
 *
 * `styleClass` is W14's — the `StyleId` → class table already exists in
 * `renderers/log`, and a second copy here is exactly the drift that makes the
 * Results pane and the Viewer print `{res}` in different inks.
 */
function RunSpans(props: RunSpansProps): JSX.Element {
  const hits = createMemo(() => props.find?.hitsOnLine(props.line) ?? []);
  const pieces = createMemo(() =>
    hits().length === 0
      ? [{ text: props.run.text, hit: undefined }]
      : segmentLine(props.run.text, hits()),
  );
  const cls = createMemo(() => styleClassOf(props.run.style));

  return (
    <For each={pieces()}>
      {(piece) => (
        <span
          class={cls()}
          data-hit={piece.hit === undefined ? undefined : ""}
          data-hit-current={
            piece.hit !== undefined && props.find?.isCurrent(piece.hit) ? "" : undefined
          }
        >
          {piece.text}
        </span>
      )}
    </For>
  );
}

/**
 * `StyleId` → class.
 *
 * Structurally identical to `renderers/log`'s `styleClass` and deliberately not
 * an import: `renderers/**` is W14's and this module must not depend on a card
 * renderer to draw a log line. The classes themselves come from `log.css` in
 * `renderers/log`, which is where the palette lives, so the two agree by
 * sharing the stylesheet rather than by sharing code.
 */
function styleClassOf(style: unknown): string {
  if (typeof style === "string") return `smcl--${style}`;
  return "smcl--link";
}
