/**
 * Test harness for the editor. **Not shipped** — nothing under `src/` outside
 * `*.test.ts` imports it, so it never enters the bundle graph.
 *
 * It exists because every acceptance bullet in this unit is about what happens
 * across a real `EditorView` transaction cycle, and constructing one takes
 * enough setup that repeating it per test file is how two test files end up
 * testing two different editors.
 *
 * The segmenter is whichever backend the caller asks for. `loadSegmenter` hides
 * which one that is on purpose (`wasm/types.ts`: "the editor imports layer 2 and
 * never layer 1"), and these tests are the consumer W11a's conformance suite
 * says is still owed — a suite that cannot tell the backends apart.
 */

import { existsSync, readFileSync } from "node:fs";
import { resolve } from "node:path";
import type { EditorView } from "@codemirror/view";
import { asDatasetStateId, asExecId, asResultId } from "../ipc/hand";
import { loadSegmenter } from "../wasm/loader";
import type { StratumSegmenter } from "../wasm/types";
import { blockAt } from "./blocks/blockField";
import { resetRuns, setRunSink } from "./blocks/run";
import { submitRun } from "./blocks/run";
import { resetCounters } from "./blocks/segmenter";
import { anchorsIn, resetAnchorIds, updateRun } from "./results/anchor";
import { resetCollapse } from "./results/collapse";
import { resetOrphans } from "./results/orphans";
import { type EditorCtx, createEditor } from "./setup";

/**
 * Every backend this checkout can produce.
 *
 * W11a's `conformance.test.ts` says, in as many words, that its cross-backend
 * checks stand in for a suite that does not exist yet, and that "when W13 lands,
 * someone must point it at both backends". This is that. The real module is
 * included only when `cargo xtask wasm` has produced it, so a fresh checkout
 * runs the suite once rather than failing.
 */
export async function editorBackends(): Promise<
  { name: string; load: () => Promise<StratumSegmenter> }[]
> {
  const backends: { name: string; load: () => Promise<StratumSegmenter> }[] = [
    { name: "stub", load: stubSegmenter },
  ];
  const path = resolve(process.cwd(), "src/wasm/generated/stratum_wasm_bg.wasm");
  if (existsSync(path)) {
    const bytes = readFileSync(path);
    backends.push({
      name: "wasm",
      // `allowUnlinked` because the shipped module may still be the reference
      // segmenter until W11b links `stratum-parse`; the editor cannot tell, and
      // that is the property being tested.
      load: () => loadSegmenter({ wasmSource: bytes, allowUnlinked: true, requireReal: true }),
    });
  }
  return backends;
}

/** A segmenter over the development stub — always available, no build step. */
export async function stubSegmenter(): Promise<StratumSegmenter> {
  const stub = await import("../wasm/stub/index.ts");
  return loadSegmenter({ module: stub.createStubModule(), backend: "stub" });
}

export interface Harness {
  readonly view: EditorView;
  readonly parent: HTMLElement;
  destroy(): void;
}

/** Mount an editor on a detached element with a stub-backed segmenter. */
export async function mountEditor(doc: string, ctx: EditorCtx = {}): Promise<Harness> {
  resetAll();
  const segmenter = ctx.segmenter ?? (await stubSegmenter());
  const parent = document.createElement("div");
  document.body.append(parent);
  const view = createEditor(parent, doc, { ...ctx, segmenter });
  return {
    view,
    parent,
    destroy() {
      view.destroy();
      parent.remove();
    },
  };
}

/** Every module-level store this unit owns, back to its initial state. */
export function resetAll(): void {
  resetCounters();
  resetRuns();
  setRunSink(null);
  resetOrphans();
  resetCollapse();
  resetAnchorIds();
}

/** Type one character at `pos`, the way a keystroke actually arrives. */
export function typeChar(view: EditorView, pos: number, ch: string): void {
  view.dispatch({ changes: { from: pos, to: pos, insert: ch } });
}

/** Run the block containing `pos` and return its anchor id. */
export function runAt(view: EditorView, pos: number): number | null {
  const block = blockAt(view.state, pos);
  if (block === null) return null;
  const before = new Set(anchorsIn(view.state, 0, view.state.doc.length).map((a) => a.rec.id));
  submitRun(view, "run.block", { blocks: [block] });
  const after = anchorsIn(view.state, 0, view.state.doc.length);
  return after.find((a) => !before.has(a.rec.id))?.rec.id ?? null;
}

/** Complete a run the way the engine's `Finished` event would. */
export function completeRun(view: EditorView, id: number, result: number): void {
  view.dispatch({
    effects: updateRun.of({
      id,
      patch: {
        kernel: { state: "current" },
        exec: asExecId(41),
        dataset: asDatasetStateId(17),
        durationMs: 80,
        result: asResultId(result),
      },
    }),
  });
}

/** A do-file with `blocks` executable regions and a section every ten. */
export function syntheticDoc(blocks: number): string {
  const lines: string[] = [];
  for (let i = 0; i < blocks; i++) {
    if (i % 10 === 0) lines.push(`// %% Part ${i / 10}`);
    lines.push(`generate v${i} = log(income${i}) + ${i}`);
  }
  return lines.join("\n");
}
