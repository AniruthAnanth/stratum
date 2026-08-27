/**
 * Selection across the window boundary — 06 §15.2.
 *
 * > The log is DOM (not canvas) precisely so that copying works. We keep
 * > `LogSelection { anchor:{line,col}, head:{line,col} }` in JS; after every
 * > window shift we re-apply a native `Range` over whatever part of the
 * > selection is currently rendered, and dragging past an edge auto-scrolls and
 * > extends the model. **Copy always goes through Rust**
 * > (`commands.logCopy(from, to, format)`), so text outside the rendered window
 * > is included and the copied output is byte-identical to the log file.
 *
 * The model is in *document* coordinates — an absolute line index out of up to
 * five million, and a column within that line — and never in DOM coordinates.
 * That is the whole point: a selection anchored to a DOM node dies the moment
 * the 600-line window shifts, which on a log is every scroll gesture, and the
 * user finds out by pressing Copy and getting the wrong 600 lines.
 */

import type { CopyFormat, LogWindow } from "./window";
import { lineText } from "./window";

export interface LogPoint {
  /** Absolute line index in the scrollback, 0-based. */
  readonly line: number;
  /** Column within the line, in UTF-16 code units, 0-based. */
  readonly col: number;
}

export interface LogSelection {
  readonly anchor: LogPoint;
  readonly head: LogPoint;
}

export interface OrderedSelection {
  readonly start: LogPoint;
  readonly end: LogPoint;
}

const before = (a: LogPoint, b: LogPoint): boolean =>
  a.line < b.line || (a.line === b.line && a.col <= b.col);

/** Document order, regardless of which way the user dragged. */
export function ordered(selection: LogSelection): OrderedSelection {
  return before(selection.anchor, selection.head)
    ? { start: selection.anchor, end: selection.head }
    : { start: selection.head, end: selection.anchor };
}

export function isEmpty(selection: LogSelection): boolean {
  return (
    selection.anchor.line === selection.head.line && selection.anchor.col === selection.head.col
  );
}

export function collapsedAt(point: LogPoint): LogSelection {
  return { anchor: point, head: point };
}

/** Drag, Shift+click and Shift+Arrow all move the head and leave the anchor. */
export function extendTo(selection: LogSelection, head: LogPoint): LogSelection {
  return { anchor: selection.anchor, head };
}

/** Select whole lines — a triple-click, and the `Select all` verb. */
export function lineRange(from: number, to: number): LogSelection {
  return { anchor: { line: from, col: 0 }, head: { line: to, col: Number.MAX_SAFE_INTEGER } };
}

/**
 * The part of a selection that intersects the rendered window `[from, to)`.
 *
 * `undefined` means the selection is entirely off-screen, which is a normal
 * state and not an error: a user who selects 40 000 lines and scrolls to the
 * middle of them has no rendered endpoint at all, and the highlight must simply
 * cover every visible row.
 */
export function clipToWindow(
  selection: LogSelection,
  from: number,
  to: number,
): OrderedSelection | undefined {
  const { start, end } = ordered(selection);
  if (end.line < from || start.line >= to) return undefined;
  const clippedStart: LogPoint = start.line >= from ? start : { line: from, col: 0 };
  const clippedEnd: LogPoint = end.line < to ? end : { line: to - 1, col: Number.MAX_SAFE_INTEGER };
  return { start: clippedStart, end: clippedEnd };
}

/** Is this line inside the selection at all? Used to decide row highlighting. */
export function coversLine(selection: LogSelection, line: number): boolean {
  const { start, end } = ordered(selection);
  return line >= start.line && line <= end.line;
}

/**
 * The `[startCol, endCol)` a given line contributes, given the line's length.
 *
 * Interior lines contribute everything. This is the function a renderer uses to
 * paint the highlight, and the one {@link copySelection} uses to trim the ends,
 * so a highlight and a copy can never disagree about where the selection stops.
 */
export function columnsOnLine(
  selection: LogSelection,
  line: number,
  length: number,
): { from: number; to: number } | undefined {
  const { start, end } = ordered(selection);
  if (line < start.line || line > end.line) return undefined;
  const from = line === start.line ? Math.min(start.col, length) : 0;
  const to = line === end.line ? Math.min(end.col, length) : length;
  return to <= from && !(start.line === end.line && start.col === end.col) && to === from
    ? { from, to }
    : { from, to };
}

/**
 * Auto-scroll while dragging past an edge, in rows per frame.
 *
 * Proportional to the overshoot and capped, because a linear map means a drag
 * 400 px past the bottom of a 5 M-line log jumps four hundred thousand lines in
 * one frame and the user loses the selection they were making.
 */
export function autoScrollRows(pointerY: number, top: number, bottom: number): number {
  const MAX = 12;
  if (pointerY < top) return -Math.min(MAX, Math.ceil((top - pointerY) / 8));
  if (pointerY > bottom) return Math.min(MAX, Math.ceil((pointerY - bottom) / 8));
  return 0;
}

/**
 * Copy a selection.
 *
 * Whole lines come from Rust — `log_copy` is the only reader of the ring, and
 * it is what makes the copied bytes identical to the log file. The first and
 * last lines are then trimmed to their columns **in Rust's own reply**, because
 * `log_copy` is line-granular (CONTRACTS §11) and a half-line selection is a
 * real gesture. Nothing here reconstructs text from the resident window, so a
 * selection that runs off the rendered window is copied in full.
 */
export async function copySelection(
  window: LogWindow,
  selection: LogSelection,
  format: CopyFormat = "text",
): Promise<string> {
  const { start, end } = ordered(selection);
  const text = await window.copy(start.line, end.line + 1, format);
  if (format !== "text") return text;

  const lines = text.split("\n");
  const last = lines.length - 1;
  // A trailing empty element from the final newline is a line terminator, not a
  // line; trimming it here keeps `end.col` meaning what it says on the last row.
  const bodyEnd = lines[last] === "" ? last - 1 : last;
  if (bodyEnd < 0) return text;

  const firstLine = lines[0];
  const lastLine = lines[bodyEnd];
  if (firstLine === undefined || lastLine === undefined) return text;

  if (bodyEnd === 0) {
    return firstLine.slice(
      Math.min(start.col, firstLine.length),
      Math.min(end.col, firstLine.length),
    );
  }
  lines[0] = firstLine.slice(Math.min(start.col, firstLine.length));
  lines[bodyEnd] = lastLine.slice(0, Math.min(end.col, lastLine.length));
  return lines.slice(0, bodyEnd + 1).join("\n");
}

/**
 * Re-apply the model as a native `Range` over the rendered rows.
 *
 * Called after every window shift. It walks the row elements the caller hands
 * in — keyed by absolute line index in `data-log-line` — and sets the document
 * selection over the intersection. When the selection is entirely off-screen it
 * clears the native selection and leaves the model alone, which is what makes
 * scrolling away from a selection and back restore it exactly.
 */
export function applySelectionToDom(
  root: ParentNode,
  selection: LogSelection,
  windowRange: { from: number; to: number },
): boolean {
  const doc =
    (root as unknown as { ownerDocument?: Document }).ownerDocument ?? globalThis.document;
  const view = doc?.defaultView;
  const native = view?.getSelection?.();
  if (native === null || native === undefined) return false;

  const clipped = clipToWindow(selection, windowRange.from, windowRange.to);
  if (clipped === undefined || isEmpty(selection)) {
    native.removeAllRanges();
    return false;
  }

  const startEl = root.querySelector(`[data-log-line="${clipped.start.line}"]`);
  const endEl = root.querySelector(`[data-log-line="${clipped.end.line}"]`);
  if (startEl === null || endEl === null) {
    native.removeAllRanges();
    return false;
  }

  const range = doc.createRange();
  const [startNode, startOffset] = textPositionIn(startEl, clipped.start.col);
  const [endNode, endOffset] = textPositionIn(endEl, clipped.end.col);
  range.setStart(startNode, startOffset);
  range.setEnd(endNode, endOffset);
  native.removeAllRanges();
  native.addRange(range);
  return true;
}

/**
 * Column → (text node, offset) inside one rendered row.
 *
 * A row is a sequence of styled spans, so a column lands inside whichever span
 * covers it. Walking the text nodes rather than assuming one child is what
 * makes a selection that starts mid-`{res}` land on the right character.
 */
function textPositionIn(row: Element, col: number): [Node, number] {
  let remaining = col;
  const walk = (node: Node): [Node, number] | undefined => {
    if (node.nodeType === 3 /* TEXT_NODE */) {
      const length = node.nodeValue?.length ?? 0;
      if (remaining <= length) return [node, remaining];
      remaining -= length;
      return undefined;
    }
    for (const child of Array.from(node.childNodes)) {
      const hit = walk(child);
      if (hit !== undefined) return hit;
    }
    return undefined;
  };
  return walk(row) ?? [row, row.childNodes.length];
}

/**
 * Point under a pointer, given the rendered rows.
 *
 * Column is derived from the character advance of the monospace cell rather
 * than from `caretRangeFromPoint`, which WebKitGTK and WebView2 disagree about
 * and which jsdom does not implement at all. On a `white-space: pre` monospace
 * log every character is exactly one advance wide, so the arithmetic is exact
 * and identical on all three platforms — which is the whole reason 06 §9.2
 * pins the log to a fixed line height and no wrapping by default.
 */
export function pointAt(options: {
  clientX: number;
  clientY: number;
  rowsTop: number;
  lineHeight: number;
  firstLine: number;
  textLeft: number;
  charWidth: number;
  lineLength: (line: number) => number;
  totalLines: number;
}): LogPoint {
  const row = Math.floor((options.clientY - options.rowsTop) / Math.max(1, options.lineHeight));
  const line = Math.max(0, Math.min(options.totalLines - 1, options.firstLine + row));
  const rawCol = Math.round((options.clientX - options.textLeft) / Math.max(1, options.charWidth));
  const col = Math.max(0, Math.min(options.lineLength(line), rawCol));
  return { line, col };
}

/** Length of a resident line, for {@link pointAt}. Off-window lines read as 0. */
export function residentLineLength(window: LogWindow, line: number): number {
  const resident = window.lineAt(line);
  return resident === undefined ? 0 : lineText(resident).length;
}
