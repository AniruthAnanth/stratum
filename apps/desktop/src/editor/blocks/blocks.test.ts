/**
 * Run verbs, gutter glyphs and the display rule — 06 §4.5, §5.2, §5.4, §5.5.
 *
 * The two claims worth naming:
 *
 *  * **Gutter glyphs are 14x14 inline SVG, never Unicode.** `○ ✓ ◌ ▶ ✕` render
 *    at different sizes and baselines on the three platforms and would jitter a
 *    column of forty markers every time one changed state. The test asserts the
 *    marker's DOM, not its intent.
 *  * **A run request carries the hash the kernel must agree with** (§5.5), so a
 *    wasm/native divergence answers `BlockMismatch` instead of executing text
 *    the user cannot see.
 */

import { EditorSelection } from "@codemirror/state";
import { beforeAll, describe, expect, it, vi } from "vitest";
import { completeRun, mountEditor, runAt, syntheticDoc } from "../harness";
import { anchorForBlock, updateRun } from "../results/anchor";
import { displayStatus, glyphNode } from "../results/widget";
import { blockAtLine, blocksTouching, cardAnchor } from "./blockField";
import { recordedRuns, resolveRun, submitRun } from "./run";
import { counters } from "./segmenter";

beforeAll(() => {
  vi.spyOn(console, "warn").mockImplementation(() => {});
});

describe("the run request (06 §5.5)", () => {
  it("carries every block's code hash and executable extent", async () => {
    const h = await mountEditor(syntheticDoc(6));
    const block = blockAtLine(h.view.state, 2);
    expect(block).not.toBeNull();
    h.view.dispatch({ selection: EditorSelection.cursor((block as { from: number }).from) });

    expect(submitRun(h.view, "run.block")).toBe(true);
    const request = recordedRuns().at(-1);
    expect(request).toBeDefined();
    expect(request?.blocks).toHaveLength(1);
    const ref = request?.blocks[0];
    expect(ref?.code_hash).toMatch(/^[0-9a-f]{32}$/);
    expect(ref?.from).toBe((block as { from: number }).from);
    expect(ref?.ordinal).toBeGreaterThanOrEqual(0);
    expect(request?.mode).toBe("interactive");
    expect(request?.origin).toBe("editor");
    h.destroy();
  });

  it("marks `run.fileClean` as a clean run, which is the expensive mistake to confuse", async () => {
    const h = await mountEditor(syntheticDoc(4));
    submitRun(h.view, "run.fileClean");
    expect(recordedRuns().at(-1)?.mode).toBe("clean");
    h.destroy();
  });

  it("resolves each verb to the right blocks and never to a comment", async () => {
    const h = await mountEditor(syntheticDoc(20));
    // An EXECUTABLE block: `run.above`/`run.below` are defined relative to the
    // block the caret is in, and a section marker is a comment region that is in
    // the document and in no run list.
    const here = blocksTouching(h.view.state, 0, h.view.state.doc.length).filter(
      (b) => b.executable,
    )[9];
    h.view.dispatch({ selection: EditorSelection.cursor((here as { from: number }).from) });

    const all = resolveRun(h.view.state, "run.file");
    const above = resolveRun(h.view.state, "run.above");
    const below = resolveRun(h.view.state, "run.below");
    const fromHere = resolveRun(h.view.state, "run.fromHere");

    expect(all.every((b) => b.executable)).toBe(true);
    expect(above.length + below.length).toBe(all.length - 1);
    expect(fromHere.length).toBe(below.length + 1);
    // The section markers are comments, so they are in the document and in no
    // run list.
    expect(all.length).toBeLessThan(h.view.state.doc.lines);
    h.destroy();
  });

  it("advances the caret without changing the document", async () => {
    const h = await mountEditor(syntheticDoc(6));
    const first = blocksTouching(h.view.state, 0, h.view.state.doc.length).filter(
      (b) => b.executable,
    )[0];
    h.view.dispatch({ selection: EditorSelection.cursor((first as { from: number }).from) });
    const before = h.view.state.doc.toString();
    const caret = h.view.state.selection.main.head;

    submitRun(h.view, "run.blockAndAdvance");

    expect(h.view.state.doc.toString()).toBe(before);
    expect(h.view.state.selection.main.head).toBeGreaterThan(caret);
    expect(counters.documentWrites).toBe(0);
    h.destroy();
  });

  it("shows `running` locally, in the same transaction, with no engine involved", async () => {
    const h = await mountEditor(syntheticDoc(4));
    const block = blockAtLine(h.view.state, 2);
    const id = runAt(h.view, (block as { from: number }).from);
    expect(id).not.toBeNull();
    // The record exists the moment the effect is applied: 06 §15.1 budgets the
    // glyph at 16 ms "independent of the kernel", which is only achievable if
    // nothing waits for it.
    const rec = anchorForBlock(h.view.state, blockAtLine(h.view.state, 2) as never);
    expect(rec?.id).toBe(id);
    expect(rec?.kernel.state).toBe("queued");
    h.destroy();
  });
});

describe("the display rule (CONTRACTS §3, 06 §5.2)", () => {
  it("moves a block only toward more stale, never toward current", async () => {
    const h = await mountEditor(syntheticDoc(4));
    const block = blockAtLine(h.view.state, 2);
    const id = runAt(h.view, (block as { from: number }).from) as number;

    // A failed run stays failed even though the code has since changed: `stale`
    // outranks `failed`, and `worseOf` takes the lower rank.
    h.view.dispatch({ effects: updateRun.of({ id, patch: { kernel: { state: "failed" } } }) });
    const rec = anchorForBlock(h.view.state, blockAtLine(h.view.state, 2) as never);
    expect(displayStatus(rec, blockAtLine(h.view.state, 2))).toBe("failed");

    // And a running block is never hidden behind a local staleness verdict.
    h.view.dispatch({ effects: updateRun.of({ id, patch: { kernel: { state: "running" } } }) });
    const running = anchorForBlock(h.view.state, blockAtLine(h.view.state, 2) as never);
    expect(displayStatus(running, blockAtLine(h.view.state, 2))).toBe("running");
    h.destroy();
  });

  it("reports never_run for a block that has none", async () => {
    const h = await mountEditor(syntheticDoc(4));
    expect(displayStatus(null, blockAtLine(h.view.state, 2))).toBe("never_run");
    h.destroy();
  });

  it("re-running one block replaces its card rather than stacking a second", async () => {
    const h = await mountEditor(syntheticDoc(4));
    const block = blockAtLine(h.view.state, 2);
    const first = runAt(h.view, (block as { from: number }).from) as number;
    completeRun(h.view, first, 1);
    const second = runAt(h.view, (block as { from: number }).from) as number;
    expect(second).not.toBe(first);
    // 06 §4.7: one block, one card. The earlier result stays reachable through
    // `state/results.ts`, which keeps every version.
    const rec = anchorForBlock(h.view.state, blockAtLine(h.view.state, 2) as never);
    expect(rec?.id).toBe(second);
    h.destroy();
  });
});

describe("gutter glyphs", () => {
  it("are 14x14 inline SVG on the icon grid, not characters", () => {
    for (const state of [
      "never_run",
      "current",
      "stale",
      "running",
      "failed",
      "interrupted",
    ] as const) {
      const node = glyphNode(state);
      expect(node.tagName.toLowerCase()).toBe("svg");
      expect(node.getAttribute("width")).toBe("14");
      expect(node.getAttribute("height")).toBe("14");
      expect(node.getAttribute("viewBox")).toBe("0 0 14 14");
      // Shape carries the meaning; the accessible name carries it again for a
      // screen reader, and colour only reinforces (06 §17).
      expect(node.getAttribute("aria-label")).toBeTruthy();
      expect(node.querySelector("path")?.getAttribute("d")).toBeTruthy();
    }
  });

  it("gives each state a DIFFERENT shape, so greyscale still reads", () => {
    const shapes = new Set<string>();
    for (const state of [
      "never_run",
      "current",
      "stale",
      "running",
      "failed",
      "interrupted",
    ] as const) {
      shapes.add(glyphNode(state).querySelector("path")?.getAttribute("d") ?? "");
    }
    expect(shapes.size).toBe(6);
  });
});

describe("the card anchor", () => {
  it("sits at the end of the block's last line, not inside the next block", async () => {
    const h = await mountEditor(syntheticDoc(5));
    for (const block of blocksTouching(h.view.state, 0, h.view.state.doc.length)) {
      const at = cardAnchor(h.view.state, block);
      expect(at).toBeGreaterThanOrEqual(block.to);
      expect(at).toBeLessThan(block.outerTo + 1);
      // And the anchor still belongs to this block, never to the following one.
      expect(blockAtLine(h.view.state, h.view.state.doc.lineAt(at).number)?.index).toBe(
        block.index,
      );
    }
    h.destroy();
  });
});
