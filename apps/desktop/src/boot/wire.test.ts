/**
 * W17's wiring, tested at its joints.
 *
 * 1. The msgpack decoder in `wire.tsx` is DIFFED against W14's reference
 *    decoder (`renderers/fixtures.ts`) over the whole committed mock stream —
 *    every frame StataMP 18.5's numbers travel in, byte-identical semantics or
 *    the test says which frame differs. Two decoders exist only because the
 *    reference reads `node:fs` at module top level and can never ship.
 * 2. `applyWireEvent` routes that same stream into the production stores
 *    (`state/exec.ts`, `state/results.ts`) — the claim behind "inline result
 *    appears below" made against the stores the panes actually read.
 * 3. `intentOf` — the RunRequest → CONTRACTS §11 `RunIntent` join, which no
 *    compiler checks across the language boundary.
 * 4. The e2e request event name — `webview.ts` here, `REQUEST_EVENT` in
 *    `src-tauri/src/e2e_cmds.rs` — two spellings of one wire that must agree.
 */

import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { afterEach, describe, expect, it } from "vitest";
import { REQUEST_EVENT } from "../e2e/webview";
import type { RunRequest } from "../editor/blocks/run";
import type { DocumentId } from "../ipc/hand";
import { detachedBridge, setBridge } from "../platform/bridge";
import { decodeFrames } from "../renderers/fixtures";
import { documents, resetDocState } from "../state/doc";
import { execCounters, resetExecCounters, resetExecState } from "../state/exec";
import { resetResultState, resultState } from "../state/results";
import {
  FILE_OPEN_EVENT,
  applyWireEvent,
  decodeFrame,
  intentOf,
  openedPathOf,
  routeOpenedFile,
} from "./wire";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "../../../..");
const streamBytes = (): Uint8Array =>
  new Uint8Array(readFileSync(resolve(repoRoot, "tests/fixtures/mock/scenario_a.msgpack")));

/**
 * The committed fixture is §10-framed (`len:u32LE | kind:u8 | corr:u32LE |
 * payload`); the channel the production decoder reads delivers bare payloads
 * (`windows.rs` encodes one event per message). Strip the framing here, the
 * same way the reference's `decodeFrames` does internally.
 */
function payloads(bytes: Uint8Array): Uint8Array[] {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const out: Uint8Array[] = [];
  let at = 0;
  while (at + 4 <= bytes.length) {
    const len = view.getUint32(at, true);
    out.push(bytes.subarray(at + 4 + 5, at + 4 + len));
    at += 4 + len;
  }
  return out;
}

afterEach(() => {
  resetExecState();
  resetExecCounters();
  resetResultState();
});

describe("the wire decoder", () => {
  it("agrees with W14's reference decoder on every frame of the mock stream", () => {
    const bytes = streamBytes();
    const reference = decodeFrames(bytes);
    const mine = payloads(bytes).map(decodeFrame);
    expect(mine.length).toBe(reference.length);
    mine.forEach((frame, i) => {
      expect(frame, `frame ${i}`).toEqual(reference[i]);
    });
  });

  it("keeps a u64 above 2^53 as itself", () => {
    // 0xcf + 0x5354415441313835 — the mock's `sample_hash`.
    const big = new Uint8Array([0xcf, 0x53, 0x54, 0x41, 0x54, 0x41, 0x31, 0x38, 0x35]);
    expect(decodeFrame(big)).toBe(0x53_54_41_54_41_31_38_35n);
  });
});

describe("applyWireEvent", () => {
  it("lands the mock stream's results and statuses in the production stores", () => {
    const frames = payloads(streamBytes()).map(decodeFrame);
    let results = 0;
    let exec = 0;
    for (const frame of frames) {
      const before = execCounters.eventsApplied;
      if (applyWireEvent(frame)) {
        if (execCounters.eventsApplied > before) exec += 1;
        else results += 1;
      }
    }
    expect(results, "Result events recorded into state/results").toBeGreaterThan(0);
    expect(exec, "exec events applied into state/exec").toBeGreaterThan(0);
    expect(resultState.order.length).toBe(results);
    // The envelope is stored under `.id` (the store's key), carrying the
    // contract's own `result` id — the rename `e2e/bridge.ts` reported.
    for (const id of resultState.order) {
      const envelope = resultState.byId[String(id)] as unknown as Record<string, unknown>;
      expect(envelope).toBeDefined();
      expect(envelope["result"]).toBe(id);
      // The wire's `[u8; 16]` code hash was converted to the UI's 32-hex form.
      const hash = envelope["code_hash"];
      if (hash !== undefined) expect(hash).toMatch(/^[0-9a-f]{32}$/);
    }
  });

  it("refuses what is not an event, without throwing", () => {
    expect(applyWireEvent(null)).toBe(false);
    expect(applyWireEvent(42)).toBe(false);
    expect(applyWireEvent({ notanevent: true })).toBe(false);
    expect(applyWireEvent({ event: "no_such_event" })).toBe(false);
  });
});

describe("intentOf", () => {
  const DOC = 1 as DocumentId;
  const block = { from: 20, to: 39, code_hash: "ab".repeat(16), ordinal: 0 };
  const request = (over: Partial<RunRequest>): RunRequest => ({
    doc: DOC,
    verb: "run.blockAndAdvance",
    blocks: [block],
    mode: "interactive",
    origin: "editor",
    ...over,
  });

  it("maps Shift+Enter's verb to run_and_advance at the block's own offset", () => {
    expect(intentOf(request({}))).toEqual({ intent: "run_and_advance", doc: DOC, cursor: 20 });
  });

  it("maps the block/section/file verbs to their CONTRACTS §11 intents", () => {
    expect(intentOf(request({ verb: "run.block" }))).toEqual({
      intent: "current_block",
      doc: DOC,
      cursor: 20,
    });
    expect(intentOf(request({ verb: "run.section" }))).toEqual({
      intent: "current_section",
      doc: DOC,
      cursor: 20,
    });
    expect(intentOf(request({ verb: "run.file" }))).toEqual({ intent: "whole_file", doc: DOC });
    expect(intentOf(request({ verb: "run.fileClean", mode: "clean" }))).toEqual({
      intent: "clean_run",
      entry: DOC,
      isolation: "in_process",
    });
    expect(intentOf(request({ verb: "run.allStale" }))).toEqual({ intent: "all_stale", doc: DOC });
    expect(intentOf(request({ verb: "run.selection" }))).toEqual({
      intent: "selection",
      doc: DOC,
      span: { start: 20, end: 39 },
    });
  });

  it("declines a request with no document or no blocks", () => {
    expect(intentOf(request({ doc: undefined }))).toBeUndefined();
    expect(intentOf(request({ blocks: [] }))).toBeUndefined();
  });
});

describe("the file-open event", () => {
  afterEach(() => {
    setBridge(undefined);
    resetDocState();
  });

  it("follows the stratum:// event convention menu.rs established", () => {
    expect(FILE_OPEN_EVENT).toBe("stratum://open-path");
  });

  it("reads the path from the conventional bare string, and from { path }", () => {
    expect(openedPathOf("/data/auto.dta")).toBe("/data/auto.dta");
    expect(openedPathOf({ path: "/data/auto.dta" })).toBe("/data/auto.dta");
    expect(openedPathOf("")).toBeUndefined();
    expect(openedPathOf({ path: 3 })).toBeUndefined();
    expect(openedPathOf(null)).toBeUndefined();
    expect(openedPathOf(42)).toBeUndefined();
  });

  it("routes a .dta to the command-bar use — the one path every command takes", async () => {
    const calls: Array<[string, Record<string, unknown> | undefined]> = [];
    setBridge(
      detachedBridge({
        invoke: <T>(command: string, args?: Record<string, unknown>): Promise<T> => {
          calls.push([command, args]);
          return Promise.resolve(undefined as T);
        },
      }),
    );
    await expect(routeOpenedFile("/data/Auto.DTA")).resolves.toBe(true);
    expect(calls).toEqual([
      [
        "exec_submit",
        {
          intent: { intent: "command_bar", text: 'use "/data/Auto.DTA"' },
          inlineMode: "always",
        },
      ],
    ]);
  });

  it("routes a .do through doc_open and records the document", async () => {
    setBridge(
      detachedBridge({
        invoke: <T>(command: string, args?: Record<string, unknown>): Promise<T> => {
          expect(command).toBe("doc_open");
          expect(args).toEqual({ path: "/proj/analysis.do" });
          return Promise.resolve({
            doc: 7,
            path: "/proj/analysis.do",
            text: "sysuse auto, clear\n",
            version: 1,
            eol: "lf",
            bom: false,
          } as T);
        },
      }),
    );
    await expect(routeOpenedFile("/proj/analysis.do")).resolves.toBe(true);
    const record = documents.docs["7"];
    expect(record).toBeDefined();
    expect(record?.path).toBe("/proj/analysis.do");
    expect(documents.active).toBe(7 as DocumentId);
  });

  it("declines an extension it has no route for, without an IPC call", async () => {
    setBridge(
      detachedBridge({
        invoke: <T>(): Promise<T> => Promise.reject(new Error("must not be called")),
      }),
    );
    await expect(routeOpenedFile("/proj/notes.txt")).resolves.toBe(false);
  });
});

describe("the file-open event", () => {
  it("is spelled the same in the host and the boot listener", () => {
    // These two halves were written in parallel and disagreed: the host
    // emitted `stratum://open-path` while the listener waited on
    // `stratum://file-open`, so every double-clicked file was dropped in
    // silence. Nothing in either language could catch that; this can.
    const host = readFileSync(resolve(repoRoot, "apps/desktop/src-tauri/src/file_open.rs"), "utf8");
    const declared = host.match(/pub const OPEN_PATH_EVENT: &str = "([^"]+)";/);
    expect(declared?.[1]).toBe(FILE_OPEN_EVENT);
  });
});

describe("the e2e request event", () => {
  it("is spelled the same in the host, the webview module and the boot listener", () => {
    const host = readFileSync(resolve(repoRoot, "apps/desktop/src-tauri/src/e2e_cmds.rs"), "utf8");
    const declared = host.match(/pub const REQUEST_EVENT: &str = "([^"]+)";/);
    expect(declared?.[1]).toBe(REQUEST_EVENT);
    // `boot/wire.tsx` listens with a literal (importing the module would
    // bundle it into the entry chunk), so the literal is pinned here too.
    const wire = readFileSync(resolve(here, "wire.tsx"), "utf8");
    const listened = wire.match(/const E2E_REQUEST_EVENT = "([^"]+)";/);
    expect(listened?.[1]).toBe(REQUEST_EVENT);
  });
});
