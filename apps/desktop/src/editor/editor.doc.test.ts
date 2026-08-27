/**
 * W13's headline acceptance bullet: **the document text is never modified by any
 * output operation.**
 *
 * Two independent proofs, because either alone is weak, and they live in two
 * files because they have different scopes.
 *
 * 1. THE BEHAVIOURAL ONE, below — 200 random runs, collapses, expands, detaches
 *    and view toggles against a real `EditorView`, asserting `doc.toString()` is
 *    byte-identical afterwards and that the counter on the one sanctioned write
 *    path never moved. This catches a write that the code we have performs.
 * 2. THE STRUCTURAL ONE, in `a15.lint.test.ts` — a scan for a `dispatch`
 *    carrying `changes` across the WHOLE frontend, matched against an enumerated
 *    registry. This catches a write path the random driver did not happen to
 *    reach, and a write path a future unit adds outside `editor/**`, neither of
 *    which the behavioural test can exclude. It is the bullet's second clause
 *    ("CI lints for any other write path"), so it is deliberately not scoped to
 *    the tree this unit owns.
 */

import { beforeAll, describe, expect, it, vi } from "vitest";
import { asResultId } from "../ipc/hand";
import { allBlocks, blockAt } from "./blocks/blockField";
import { counters, resetCounters } from "./blocks/segmenter";
import { completeRun, mountEditor, runAt, syntheticDoc, typeChar } from "./harness";
import { anchorsIn, detachAnchor, resultsField, setCardUi } from "./results/anchor";
import { toggleCollapsed } from "./results/collapse";
import { orphanResults } from "./results/orphans";
import { displayStatus } from "./results/widget";
import { toggleDocumentView } from "./sections/docview";
import { toggleFoldAt } from "./sections/fold";

beforeAll(() => {
  // The development stub announces itself on every construction, which is the
  // point of it, and 200 banners drown the runner.
  vi.spyOn(console, "warn").mockImplementation(() => {});
});

/** Narrow away an `undefined` that the test has already asserted against. */
function must<T>(value: T | undefined | null, what: string): T {
  if (value === undefined || value === null) throw new Error(`missing ${what}`);
  return value;
}

/** Deterministic PRNG: a fuzz that cannot be replayed is not evidence. */
function rng(seed: number): () => number {
  let state = seed >>> 0;
  return () => {
    state = (state * 1664525 + 1013904223) >>> 0;
    return state / 0x1_0000_0000;
  };
}

describe("the document is never modified by an output operation", () => {
  it("survives 200 random runs, collapses, expands and detaches", async () => {
    const doc = syntheticDoc(40);
    const h = await mountEditor(doc);
    const before = h.view.state.doc.toString();
    expect(before).toBe(doc);

    const random = rng(0xc0ffee);
    const attached: number[] = [];
    let results = 1;

    for (let step = 0; step < 200; step++) {
      const blocks = allBlocks(h.view.state).filter((b) => b.executable);
      expect(blocks.length).toBeGreaterThan(0);
      const block = blocks[Math.floor(random() * blocks.length)];
      if (block === undefined) continue;

      switch (Math.floor(random() * 5)) {
        case 0: {
          const id = runAt(h.view, block.from);
          if (id !== null) {
            completeRun(h.view, id, results++);
            attached.push(id);
          }
          break;
        }
        case 1: {
          const found = anchorsIn(h.view.state, block.outerFrom, block.outerTo)[0];
          if (found !== undefined) {
            const collapsed = toggleCollapsed(found.rec.executedHash);
            h.view.dispatch({
              effects: setCardUi.of({ id: found.rec.id, ui: { ...found.rec.ui, collapsed } }),
            });
          }
          break;
        }
        case 2: {
          const id = attached[Math.floor(random() * attached.length)];
          if (id !== undefined) h.view.dispatch({ effects: detachAnchor.of(id) });
          break;
        }
        case 3:
          toggleFoldAt(h.view, block.from);
          break;
        default:
          toggleDocumentView(h.view, step % 2 === 0);
          break;
      }
    }

    expect(h.view.state.doc.toString()).toBe(before);
    expect(counters.documentWrites).toBe(0);
    h.destroy();
  });
});

describe("cards anchor by offset, never by line number", () => {
  it("moves a card correctly when 40 lines are inserted above it", async () => {
    const h = await mountEditor(syntheticDoc(6));
    const blocks = allBlocks(h.view.state).filter((b) => b.executable);
    const target = must(blocks[blocks.length - 1], "last block");
    const id = must(runAt(h.view, target.from), "anchor id");

    completeRun(h.view, id, 1);
    const before = must(
      anchorsIn(h.view.state, 0, h.view.state.doc.length).find((a) => a.rec.id === id),
      "anchor",
    ).at;

    const inserted = `${"* filler\n".repeat(40)}`;
    h.view.dispatch({ changes: { from: 0, to: 0, insert: inserted } });

    const after = must(
      anchorsIn(h.view.state, 0, h.view.state.doc.length).find((a) => a.rec.id === id),
      "anchor after insert",
    );
    // Exactly the inserted length. Not "about right", not "the same line" —
    // the mapping is arithmetic and any drift is a bug in the anchoring.
    expect(after.at).toBe(before + inserted.length);
    h.destroy();
  });
});

describe("the three anchor policies (06 §4.6)", () => {
  it("1. identity survives: an edit elsewhere leaves the card attached and current", async () => {
    const h = await mountEditor(syntheticDoc(5));
    const blocks = allBlocks(h.view.state).filter((b) => b.executable);
    const target = must(blocks[2], "third block");
    const id = must(runAt(h.view, target.from), "anchor id");
    completeRun(h.view, id, 1);

    typeChar(h.view, 0, "*");
    const found = must(
      anchorsIn(h.view.state, 0, h.view.state.doc.length).find((a) => a.rec.id === id),
      "anchor",
    );
    expect(displayStatus(found.rec, blockAt(h.view.state, found.at))).toBe("current");
    h.destroy();
  });

  it("2. code_hash changed: stale instantly, with zero IPC", async () => {
    const h = await mountEditor(syntheticDoc(5));
    const blocks = allBlocks(h.view.state).filter((b) => b.executable);
    const target = must(blocks[2], "third block");
    const id = must(runAt(h.view, target.from), "anchor id");
    completeRun(h.view, id, 1);

    resetCounters();
    typeChar(h.view, target.to, "1");

    const found = must(
      anchorsIn(h.view.state, 0, h.view.state.doc.length).find((a) => a.rec.id === id),
      "anchor",
    );
    expect(displayStatus(found.rec, blockAt(h.view.state, found.at))).toBe("stale");
    // The entire promise of §12: no round trip decided this.
    expect(counters.ipcCalls).toBe(0);
    h.destroy();
  });

  it("3. anchor deleted: the widget goes, the result survives as an orphan", async () => {
    const h = await mountEditor(syntheticDoc(5));
    const blocks = allBlocks(h.view.state).filter((b) => b.executable);
    const target = must(blocks[2], "third block");
    const id = must(runAt(h.view, target.from), "anchor id");
    completeRun(h.view, id, 7);
    expect(anchorsIn(h.view.state, 0, h.view.state.doc.length)).toHaveLength(1);

    h.view.dispatch({
      changes: {
        from: target.outerFrom,
        to: Math.min(target.outerTo + 1, h.view.state.doc.length),
        insert: "",
      },
    });

    expect(anchorsIn(h.view.state, 0, h.view.state.doc.length)).toHaveLength(0);
    expect(orphanResults().map((o) => o.result)).toEqual([asResultId(7)]);
    h.destroy();
  });
});

describe("inline-results mode", () => {
  it("`off` produces no widget decorations and keeps every execution record", async () => {
    const h = await mountEditor(syntheticDoc(4), { inlineResults: "off" });
    const blocks = allBlocks(h.view.state).filter((b) => b.executable);
    const id = must(runAt(h.view, must(blocks[0], "first block").from), "anchor id");
    completeRun(h.view, id, 1);

    const field = h.view.state.field(resultsField);
    expect(field.mode).toBe("off");
    expect(field.deco.size).toBe(0);
    // The record is still there, which is what makes the gutter correct in
    // Classic and what makes switching inline results back on lose nothing.
    expect(field.anchors.size).toBe(1);
    h.destroy();
  });
});
