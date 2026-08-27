/**
 * The scrollback's acceptance bullets — plan W16, 06 §9.2, §15.2.
 *
 *  * Results is a **scrollback log, not a card feed**: append-only, `pre`, no
 *    wrapping by default.
 *  * **Copy always goes through Rust** (`log_copy`) so text outside the rendered
 *    window is included and is byte-identical to the log file.
 *  * **Synthetic scrollbar** — 5 M lines × 18 px overflows the browser's
 *    ~33.5 M px element cap, so no tall spacer div.
 *
 * The recording source below is the whole harness: a `LogSource` over a plain
 * array of lines, which is what lets "the selection ran past the rendered
 * window" be an assertion about *who was asked* rather than about pixels.
 */

import { afterEach, beforeEach, describe, expect, test } from "vitest";
import type { StyledRunView } from "../renderers/types";
import { LogFind, findCounters, resetFindCounters, segmentLine } from "./find";
import {
  ELEMENT_HEIGHT_CAP_PX,
  applyKey,
  fitsInASpacerDiv,
  maxPosition,
  positionForThumbOffset,
  rowsForWheel,
  thumb,
} from "./scrollbar";
import { collapsedAt, copySelection, extendTo, lineRange, ordered } from "./selection";
import {
  type CopyFormat,
  type LogHitView,
  type LogLine,
  type LogSearchOptions,
  type LogSource,
  LogWindow,
  counters,
  lineText,
  resetLogCounters,
  splitRunsIntoLines,
} from "./window";

/** One line of the fake log, as the engine would style it. */
const line = (n: number): string => `. line ${n} of the log`;

interface Recorder extends LogSource {
  readonly ranges: { from: number; to: number }[];
  readonly copies: { from: number; to: number; format: CopyFormat }[];
}

/**
 * A `LogSource` over an array. Every call is recorded, because the acceptance
 * bullets are about which calls happen, not about what comes back.
 */
function recorder(total: number): Recorder {
  const ranges: { from: number; to: number }[] = [];
  const copies: { from: number; to: number; format: CopyFormat }[] = [];
  return {
    ranges,
    copies,
    range(from, to) {
      ranges.push({ from, to });
      const runs: StyledRunView[] = [];
      for (let i = from; i < to; i++) runs.push({ text: `${line(i)}\n`, style: "text" });
      return Promise.resolve({ fromLine: from, runs });
    },
    copy(from, to, format) {
      copies.push({ from, to, format });
      const out: string[] = [];
      for (let i = from; i < Math.min(to, total); i++) out.push(line(i));
      return Promise.resolve(`${out.join("\n")}\n`);
    },
    search(query: string, opts: LogSearchOptions) {
      const hits: LogHitView[] = [];
      for (let i = 0; i < total && hits.length < opts.maxHits; i++) {
        const col = line(i).indexOf(query);
        if (col >= 0) hits.push({ line: i, col, len: query.length, preview: line(i) });
      }
      return Promise.resolve({ hits, total: hits.length });
    },
  };
}

beforeEach(() => {
  resetLogCounters();
  resetFindCounters();
});

afterEach(() => {
  resetLogCounters();
});

// ---------------------------------------------------------------------------
// No tall spacer div
// ---------------------------------------------------------------------------

describe("synthetic scrollbar (06 §15.2)", () => {
  test("5 M lines × 18 px does not fit in a spacer div", () => {
    // The number the bullet is about. 90 M px against a ~33.5 M px cap.
    expect(5_000_000 * 18).toBeGreaterThan(ELEMENT_HEIGHT_CAP_PX);
    expect(fitsInASpacerDiv(5_000_000, 18)).toBe(false);
    // And the shipped line height is worse, not better.
    expect(fitsInASpacerDiv(5_000_000, 20)).toBe(false);
  });

  test("a small log would fit — so the check is a real one, not always false", () => {
    expect(fitsInASpacerDiv(10_000, 20)).toBe(true);
  });

  test("the thumb is computed from rows, so it never needs a pixel document", () => {
    const metrics = { total: 5_000_000, viewport: 40, position: 2_500_000 };
    const geometry = thumb(metrics, 600);
    expect(geometry.hidden).toBe(false);
    expect(geometry.size).toBeGreaterThanOrEqual(24);
    expect(geometry.offset).toBeGreaterThan(0);
    expect(geometry.offset + geometry.size).toBeLessThanOrEqual(600);

    // The round trip a drag performs, at a scale no element could express.
    const back = positionForThumbOffset(metrics, 600, geometry.offset);
    expect(Math.abs(back - metrics.position)).toBeLessThan(metrics.total * 0.001);
  });

  test("a 5 M-line log pages and homes without arithmetic overflow", () => {
    const metrics = { total: 5_000_000, viewport: 40, position: 0 };
    expect(applyKey(metrics, "end")).toBe(maxPosition(metrics));
    expect(applyKey({ ...metrics, position: maxPosition(metrics) }, "home")).toBe(0);
    expect(applyKey(metrics, "pageDown")).toBe(39);
    // A trackpad reports pixels; a Firefox mouse wheel reports lines.
    expect(rowsForWheel({ deltaY: 100, deltaMode: 0 }, 20, 40)).toBe(5);
    expect(rowsForWheel({ deltaY: 3, deltaMode: 1 }, 20, 40)).toBe(3);
  });
});

// ---------------------------------------------------------------------------
// Append-only, and the line split
// ---------------------------------------------------------------------------

describe("a scrollback, not a card feed (06 §9.2)", () => {
  test("the model exposes no way to edit a line that has already been shown", () => {
    const win = new LogWindow({ source: recorder(0) });
    const surface = Object.getOwnPropertyNames(Object.getPrototypeOf(win) as object);
    // `append`, `clear` and `setTotal` are the only mutators. In particular
    // there is no `replace`, `patch`, `update` or `set` for a line.
    expect(surface.filter((k) => /^(replace|patch|update|edit|insertAt)/u.test(k))).toEqual([]);
    expect(surface).toContain("append");
  });

  test("append lands at the tail and moves a tail-locked viewport", () => {
    const win = new LogWindow({ source: recorder(0) });
    win.setViewport(10);
    win.append([{ text: "a\nb\nc\n", style: "text" }]);
    expect(win.totalLines).toBe(3);
    expect(counters.appended).toBe(3);
    expect(win.followingTail).toBe(true);

    // A user who scrolled up is NOT yanked back down by the next output. The
    // log has to be longer than the viewport for "scrolled up" to be a state at
    // all, so this second half appends its way past it first.
    win.append([{ text: `${"x\n".repeat(40)}`, style: "text" }]);
    expect(win.totalLines).toBe(43);
    win.scrollTo(0);
    expect(win.followingTail).toBe(false);
    win.append([{ text: "d\n", style: "text" }]);
    expect(win.position).toBe(0);
  });

  test("wrapping is off by default — the O(1) height path", () => {
    const win = new LogWindow({ source: recorder(0), lineHeight: 20 });
    expect(win.wrap).toBe(false);
    expect(win.topOf(1_000)).toBe(20_000);
    expect(win.lineAtOffset(20_000)).toBe(1_000);
    // The Fenwick tree is never touched on the fast path, and that is the claim.
    expect(counters.heightUpdates).toBe(0);
  });

  test("a run boundary inside a line survives as two spans", () => {
    const lines = splitRunsIntoLines([
      { text: ". summarize ", style: "input" },
      { text: "price\n", style: "result" },
      { text: "next\n", style: "text" },
    ]);
    expect(lines).toHaveLength(2);
    const first = lines[0] as LogLine;
    expect(first.runs).toHaveLength(2);
    // Byte-identical when flattened: this is what makes a copy of the rendered
    // text the same string the log file holds.
    expect(lineText(first)).toBe(". summarize price");
  });

  test("a final line with no terminator is still a line", () => {
    expect(splitRunsIntoLines([{ text: "no newline", style: "text" }])).toHaveLength(1);
  });
});

// ---------------------------------------------------------------------------
// The window, and its fetch counter
// ---------------------------------------------------------------------------

describe("the resident window (06 §15.2)", () => {
  test("scrolling inside the prefetch margin costs zero log_range calls", async () => {
    const source = recorder(100_000);
    const win = new LogWindow({ source });
    win.setTotal(100_000);
    win.setViewport(40);
    win.scrollTo(50_000);
    await win.ensureVisible();
    expect(counters.fetches).toBe(1);

    // 300 lines is well inside the 2 000-line margin on each side.
    for (let i = 0; i < 30; i++) {
      win.scrollBy(10);
      await win.ensureVisible();
    }
    expect(counters.fetches).toBe(1);
  });

  test("a jump beyond the margin fetches exactly once", async () => {
    const source = recorder(100_000);
    const win = new LogWindow({ source });
    win.setTotal(100_000);
    win.setViewport(40);
    await win.ensureVisible();
    const before = counters.fetches;
    win.scrollTo(90_000);
    await win.ensureVisible();
    expect(counters.fetches).toBe(before + 1);
    expect(source.ranges.at(-1)?.from).toBeLessThan(90_000);
  });

  test("a line outside the window renders as a placeholder rather than waiting", () => {
    const win = new LogWindow({ source: recorder(100_000) });
    win.setTotal(100_000);
    win.setViewport(5);
    win.scrollTo(50_000);
    const { lines } = win.visibleLines();
    expect(lines).toHaveLength(6);
    expect(lines.every((l) => l === undefined)).toBe(true);
  });
});

// ---------------------------------------------------------------------------
// Copy always goes through Rust
// ---------------------------------------------------------------------------

describe("copy goes through Rust (06 §15.2, CONTRACTS §11)", () => {
  test("a selection larger than the rendered window is copied in full", async () => {
    const source = recorder(100_000);
    const win = new LogWindow({ source });
    win.setTotal(100_000);
    win.setViewport(40);
    await win.ensureVisible();

    // 40 000 lines selected; at most ~600 were ever rendered.
    const selection = lineRange(1_000, 41_000);
    const text = await copySelection(win, selection);
    expect(source.copies).toHaveLength(1);
    expect(source.copies[0]).toEqual({ from: 1_000, to: 41_001, format: "text" });
    expect(text.split("\n")).toHaveLength(40_001);
    expect(text.startsWith(line(1_000))).toBe(true);
  });

  test("there is no local fallback: a failing log_copy is an error, not 600 lines", async () => {
    const win = new LogWindow({
      source: {
        range: () => Promise.resolve({ fromLine: 0, runs: [] }),
        copy: () => Promise.reject(new Error("no host")),
        search: () => Promise.resolve({ hits: [], total: 0 }),
      },
    });
    win.setTotal(10);
    win.append([{ text: "resident\n", style: "text" }]);
    await expect(copySelection(win, lineRange(0, 1))).rejects.toThrow("no host");
  });

  test("a part-line selection is trimmed from Rust's own reply", async () => {
    const source = recorder(10);
    const win = new LogWindow({ source });
    win.setTotal(10);
    const selection = extendTo(collapsedAt({ line: 2, col: 2 }), { line: 2, col: 6 });
    expect(ordered(selection).start.col).toBe(2);
    expect(await copySelection(win, selection)).toBe(line(2).slice(2, 6));
  });

  test("the copy formats are the four CONTRACTS §11 names", async () => {
    const source = recorder(4);
    const win = new LogWindow({ source });
    win.setTotal(4);
    for (const format of ["text", "tsv", "html", "latex"] as const) {
      await win.copy(0, 4, format);
    }
    expect(source.copies.map((c) => c.format)).toEqual(["text", "tsv", "html", "latex"]);
  });
});

// ---------------------------------------------------------------------------
// Find
// ---------------------------------------------------------------------------

describe("find runs in Rust over the whole buffer (06 §9.2)", () => {
  test("a search indexes its hits once and steps through them", async () => {
    const source = recorder(500);
    const win = new LogWindow({ source });
    win.setTotal(500);
    win.setViewport(30);
    const find = new LogFind(win);

    await find.search("line 4");
    expect(findCounters.searches).toBe(1);
    expect(findCounters.indexBuilds).toBe(1);
    expect(find.snapshot.hits.length).toBeGreaterThan(0);
    expect(find.snapshot.current).toBe(0);

    const first = find.snapshot.hits[0] as LogHitView;
    expect(find.isCurrent(first)).toBe(true);
    find.next();
    expect(find.isCurrent(first)).toBe(false);
    // Wrapping: Enter at the last hit returns to the first.
    for (let i = 1; i < find.snapshot.hits.length; i++) find.next();
    expect(find.snapshot.current).toBe(0);
  });

  test("an empty query clears rather than asking for five million hits", async () => {
    const win = new LogWindow({ source: recorder(500) });
    win.setTotal(500);
    const find = new LogFind(win);
    await find.search("");
    expect(findCounters.searches).toBe(0);
    expect(find.snapshot.hits).toEqual([]);
  });

  test("a hit splits its line into plain and matched pieces", () => {
    const pieces = segmentLine("summarize price", [{ line: 0, col: 10, len: 5, preview: "" }]);
    expect(pieces.map((p) => p.text)).toEqual(["summarize ", "price"]);
    expect(pieces[1]?.hit).toBeDefined();
  });
});
