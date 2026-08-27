// @vitest-environment node
//
// Not jsdom, which is the suite default, for the same reason
// `src/wasm/conformance.test.ts` opts out: the wasm segmenter is bytes, typed
// arrays and linear memory, and jsdom's `TextEncoder` comes from a different
// realm. Nothing this file touches is a DOM — the bridge reads stores, not
// elements.

/**
 * The e2e bridge: its unit tests, and — when the harness asks — the server loop
 * that lets `stratum-e2e`'s tier 1 drive it.
 *
 * # Why one file does both
 *
 * `STRATUM_E2E_PORT` is set only by `crates/stratum-e2e/src/tier1.rs`. When it
 * is absent this file is an ordinary member of `pnpm test`, and the tests below
 * are the ones that keep the bridge honest — that it advertises only
 * capabilities it can back, that it reaches the real command registry, and that
 * the blocks it reports agree with W07's canned block map. When it is set, the
 * last case connects back to the harness and serves the protocol until told to
 * quit.
 *
 * The alternative — a separate entry point run by `node` — needs a second module
 * resolver, because the frontend's imports are resolved by vite's config
 * (`resolve.conditions`, the solid plugin, `.json` imports of the layout
 * presets). A second resolver is a second answer to "what does
 * `../state/results` mean", and the two answers diverge exactly when it matters.
 */

import { existsSync } from "node:fs";
import { readFile } from "node:fs/promises";
import { createConnection } from "node:net";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { beforeEach, describe, expect, it } from "vitest";

import { registerShellCommands } from "../boot/commands.ts";
import { loadSegmenter } from "../wasm/loader.ts";
import type { StratumSegmenter } from "../wasm/types.ts";
import { HOST_NAME, createBridge, hexOfCodeHash, installBridge, storeShapeOf } from "./bridge.ts";
import type { E2eBridge, Request, Response, Section, Snapshot } from "./protocol.ts";

/**
 * The repo root, found the way `stratum_e2e::fixtures::repo_root` finds it.
 *
 * Not `process.cwd()`: vitest runs from `apps/desktop`, and a witness path is
 * repo-relative because that is what `docs/ownership.toml` speaks.
 */
const REPO_ROOT = (() => {
  let dir = dirname(fileURLToPath(import.meta.url));
  while (!existsSync(resolve(dir, "docs/ownership.toml"))) {
    const up = dirname(dir);
    if (up === dir) throw new Error("no repo root above this file");
    dir = up;
  }
  return dir;
})();

/** The document W07's canned stream is about, byte for byte. */
const AUTO_DO = "sysuse auto, clear\n\nsummarize price mpg\n\nregress price mpg weight foreign\n";

async function segmenter(): Promise<StratumSegmenter> {
  const url = new URL("../wasm/generated/stratum_wasm_bg.wasm", import.meta.url);
  const wasmSource = { module_or_path: new Uint8Array(await readFile(fileURLToPath(url))) };
  return loadSegmenter({ wasmSource, requireReal: true, allowUnlinked: true });
}

function present<T>(field: { present: T } | { unavailable: unknown }, what: string): T {
  if ("present" in field) return field.present;
  throw new Error(`${what} is unavailable: ${JSON.stringify(field)}`);
}

// ---------------------------------------------------------------------------
// Unit tests — always on
// ---------------------------------------------------------------------------

/**
 * Register every verb the bridge's host advertises.
 *
 * W12's shell verbs and W13's editor verbs, because the bridge now advertises
 * `editor`: the harness turns an advertised capability whose command answers
 * `unknown` into a FAILED step, so advertising `editor` without registering
 * `run.*` would be a claim this host could not back.
 *
 * Imported dynamically for `dom.ts`'s reason — `editor/commands.ts` reaches
 * `@codemirror/view`, which needs `window` at module-evaluation time.
 */
async function registerAllCommands(): Promise<() => void> {
  const shell = registerShellCommands();
  const { registerEditorCommands } = await import("../editor/commands.ts");
  const editor = registerEditorCommands();
  return () => {
    editor();
    shell();
  };
}

describe("the e2e bridge", () => {
  let bridge: E2eBridge;
  let disposeCommands: () => void;

  beforeEach(async () => {
    // `await`: the bridge mounts W13's editor and W14's renderers, and both need
    // a `document` to exist before their modules are evaluated. See `dom.ts`.
    bridge = await createBridge({ segmenter: await segmenter() });
    bridge.reset();
    disposeCommands?.();
    disposeCommands = await registerAllCommands();
    return () => disposeCommands();
  });

  it("advertises only what it can actually answer", () => {
    const caps = bridge.capabilities();
    expect(caps).toContain("commands");
    expect(caps).toContain("results");
    // Added in repair round 3, when W13 and W14 landed. The harness holds a host
    // to everything it advertises, so each of these is backed by a test below:
    // `editor` by the caret cases, `gutter` and `cards` by the run case.
    expect(caps).toContain("editor");
    expect(caps).toContain("gutter");
    expect(caps).toContain("cards");
    // Still absent, and still for a reason rather than out of caution: W16 has
    // written no pane, W18 no data editor, and `engine` would mean a real kernel
    // rather than W07's replayed stream. This list must not grow aspirationally.
    expect(caps).not.toContain("panes");
    expect(caps).not.toContain("data_editor");
    expect(caps).not.toContain("engine");
  });

  it("dispatches through the real command registry, not a copy of it", () => {
    const out = bridge.dispatch({
      action: "verb",
      command: "layout.apply",
      args: { id: "classic" },
      context: {},
    });
    expect(out.result).toBe("ran");
    expect(out.via).toBe("verb");
    const snap = bridge.snapshot(["layout"]);
    expect(present(snap.layout, "layout").id).toBe("classic");
    // 06 §8.3: Classic's own default hides inline results. Asserting it here is
    // what would catch somebody "improving" resources/layouts/classic.json.
    expect(present(snap.layout, "layout").inline_results).toBe("off");
  });

  it("reports an unregistered verb as unknown rather than throwing", () => {
    const out = bridge.dispatch({
      action: "verb",
      command: "nobody.registeredThis",
      args: null,
      context: { editorFocus: true },
    });
    expect(out.result).toBe("unknown");
  });

  it("reaches W13's own run verb, because it advertises `editor`", () => {
    bridge.dispatch({ action: "open_doc", fixture: "scenario_a.do", text: AUTO_DO, feed: [] });
    const out = bridge.dispatch({
      action: "verb",
      command: "run.blockAndAdvance",
      args: null,
      context: { editorFocus: true },
    });
    // Through `registerEditorCommands`, not through a copy: the day W13 renames
    // the verb this reads `unknown` and the capability claim is caught here
    // rather than as a mystery blocked step in a scenario report.
    expect(out.result).toBe("ran");
  });

  it("resolves a chord against the live keymap trie, honouring its when clause", () => {
    const out = bridge.dispatch({
      action: "observe",
      label: "chord resolution",
      chord: "Shift+Enter",
      context: { editorFocus: true },
    });
    expect(out.chord_resolves_to).toBe("run.blockAndAdvance");

    // Same chord, no editor focus: the binding is gated on `editorFocus`, so it
    // resolves to nothing. That is the app's rule, not the harness's.
    const gated = bridge.dispatch({
      action: "observe",
      label: "chord resolution",
      chord: "Shift+Enter",
      context: {},
    });
    expect(gated.chord_resolves_to).toBeNull();
  });

  it("segments the fixture into the blocks W07's canned map declares", () => {
    bridge.dispatch({ action: "open_doc", fixture: "scenario_a.do", text: AUTO_DO, feed: [] });
    const blocks = present(bridge.snapshot(["blocks"]).blocks, "blocks");
    // The spans in `mock_engine::scenario_a_block_map` are real offsets into
    // this text. The real wasm segmenter reproducing them independently is what
    // makes "the fixture is the document the canned stream is about" a fact
    // rather than a convention.
    expect(blocks.map((b) => b.span)).toEqual([
      [0, 18],
      [20, 39],
      [41, 73],
    ]);
    expect(blocks.every((b) => /^[0-9a-f]{32}$/.test(b.hash))).toBe(true);
    expect(blocks.map((b) => b.status)).toEqual(["never_run", "never_run", "never_run"]);
  });

  it("applies the engine's status events through the real display rule", () => {
    bridge.dispatch({
      action: "open_doc",
      fixture: "scenario_a.do",
      text: AUTO_DO,
      feed: [
        { event: "block_map_changed", seq: 1, map: { blocks: [1, 2, 3], regions: [] } },
        {
          event: "status_changed",
          seq: 2,
          doc: 1,
          changed: [[1, { state: "current", exec: 1, dataset: 17, duration_us: 8412 }]],
        },
      ],
    });
    const blocks = present(bridge.snapshot(["blocks"]).blocks, "blocks");
    expect(blocks[0]?.status).toBe("current");
    expect(blocks[1]?.status).toBe("never_run");
  });

  it("marks an edited block stale, by the app's own worseOf(local, kernel) rule", () => {
    bridge.dispatch({
      action: "open_doc",
      fixture: "scenario_a.do",
      text: AUTO_DO,
      feed: [
        { event: "block_map_changed", seq: 1, map: { blocks: [1, 2, 3], regions: [] } },
        {
          event: "block_started",
          seq: 2,
          block: 1,
          code_hash: new Array<number>(16).fill(1),
          span: { start: 0, end: 18 },
          text: "sysuse auto, clear",
        },
        {
          event: "status_changed",
          seq: 3,
          doc: 1,
          changed: [[1, { state: "current", exec: 1, dataset: 17, duration_us: 8412 }]],
        },
      ],
    });
    expect(present(bridge.snapshot(["blocks"]).blocks, "blocks")[0]?.status).toBe("current");

    // Change the transformation's code. The segmenter re-hashes it, and
    // `displayedStatus` — W12's, unmodified — takes the worse of the two.
    bridge.dispatch({ action: "edit", span: [0, 18], text: "sysuse auto" });
    const after = present(bridge.snapshot(["blocks"]).blocks, "blocks");
    expect(after[0]?.status).toBe("stale");
    expect(after[1]?.status).toBe("never_run");
  });

  it("files a result under the client key the store queries by", () => {
    const hash = new Array<number>(16).fill(2);
    bridge.dispatch({
      action: "open_doc",
      fixture: "scenario_a.do",
      text: AUTO_DO,
      feed: [
        {
          event: "block_map_changed",
          seq: 1,
          map: { blocks: [1, 2, 3], regions: [{}, { code_hash: hash, hash_ordinal: 0 }, {}] },
        },
        {
          event: "result",
          seq: 2,
          exec: 2,
          envelope: {
            result: 2,
            revision: 0,
            block: 2,
            code_hash: hash,
            cmdline: "summarize price mpg",
            rc: 0,
            payloads: [{ kind: "summarize" }],
            raw: { head: "       price |         74    6165.257\n", bytes: 0, lines: 1 },
          },
        },
      ],
    });
    const results = present(bridge.snapshot(["results"]).results, "results");
    expect(results).toHaveLength(1);
    expect(results[0]?.block).toBe(1);
    expect(results[0]?.cmdline).toBe("summarize price mpg");
    expect(results[0]?.payloads).toEqual(["summarize"]);
    expect(results[0]?.client_key).toBe(`${"02".repeat(16)}:0`);
    expect(results[0]?.raw_head.some((l) => l.includes("6165.257"))).toBe(true);
  });

  it("answers doc, gutter and cards out of W13 and W14 rather than owing them", () => {
    bridge.dispatch({ action: "open_doc", fixture: "scenario_a.do", text: AUTO_DO, feed: [] });
    const snap: Snapshot = bridge.snapshot([
      "doc",
      "gutter",
      "cards",
      "panes",
      "results",
      "layout",
      "history",
      "blocks",
    ]);
    // Through wave 1 these three said W13 and W14 owed them, and both units had
    // already landed. `no_snapshot_field_is_owed_by_a_unit_that_has_landed` in
    // tests/e2e/harness.rs is the check that keeps that from recurring; this is
    // its frontend half.
    const doc = present(snap.doc, "doc");
    expect(doc.text).toBe(AUTO_DO);
    expect(doc.path).toBe("scenario_a.do");
    expect(present(snap.gutter, "gutter").map((g) => g.glyph)).toEqual([
      "never_run",
      "never_run",
      "never_run",
    ]);
    expect(present(snap.cards, "cards")).toEqual([]);
  });

  /**
   * **The expiry on the blocked ledger.**
   *
   * Every `owedBy` carries a `witness`: the repo-relative path whose ABSENCE is
   * the claim. If the file exists, the claim is stale and this host is
   * under-reporting the tree.
   *
   * This is the check that did not exist through wave 1, which is why the bridge
   * went on saying W13 owed `doc` and W14 owed `cards` for a whole wave after
   * both units shipped. Prose cannot expire; a path can.
   */
  it("has no expired claim in its blocked ledger", () => {
    const snap = bridge.snapshot([
      "doc",
      "gutter",
      "results",
      "cards",
      "panes",
      "focus",
      "layout",
      "history",
      "blocks",
    ]);

    type Claim = { field: string; unit: string; why: string; witness: string };
    const claims: Claim[] = [];
    for (const [field, value] of Object.entries(snap)) {
      if (typeof value === "object" && value !== null && "unavailable" in value) {
        const owed = (value as { unavailable: Omit<Claim, "field"> }).unavailable;
        claims.push({ ...owed, field });
      }
    }
    if ("present" in snap.panes) {
      for (const pane of snap.panes.present) {
        if ("unavailable" in pane.content) {
          claims.push({ ...pane.content.unavailable, field: `panes[${pane.id}]` });
        }
      }
    }

    // Anti-vacuity, keyed to the MECHANISM rather than to the tree.
    //
    // This assertion used to read `claims.length > 0`, justified by "no unit has
    // written a `variables`, `properties`, `project` or `viewer` pane". W16 then
    // wrote all four in this wave and the test went red for the tree getting
    // BETTER — the mirror image of the staleness bug it was written to catch,
    // and the same mistake: an assertion about how finished the repository
    // happens to be, standing in for an assertion about the check.
    //
    // What has to be non-vacuous is the filter below, so that is what is tested:
    // a witness that exists must be classified expired, and one that does not
    // must not be. Both hold whether or not anything is owed today.
    const expired = (c: Claim) => existsSync(resolve(REPO_ROOT, c.witness));
    const probe = (witness: string): Claim => ({
      field: "probe",
      unit: "W25",
      why: "probe",
      witness,
    });
    expect(expired(probe("docs/ownership.toml")), "a landed witness must read as expired").toBe(
      true,
    );
    expect(
      expired(probe("apps/desktop/src/panes/__no_such_pane__/index.tsx")),
      "an absent witness must not read as expired",
    ).toBe(false);

    const stale = claims.filter(expired);
    expect(
      stale,
      `these claims have expired — the unit landed and the bridge still reports it owing the field:
${stale.map((c) => `  ${c.field}: ${c.unit} (${c.why}) but ${c.witness} exists`).join("\n")}`,
    ).toEqual([]);
  });

  it("moves the caret through the editor, and run.blockAndAdvance advances it", () => {
    bridge.dispatch({ action: "open_doc", fixture: "scenario_a.do", text: AUTO_DO, feed: [] });
    bridge.dispatch({ action: "place_caret", offset: 20 });
    expect(present(bridge.snapshot(["doc"]).doc, "doc").caret).toBe(20);

    bridge.dispatch({ action: "run", verb: "run.blockAndAdvance", label: "summarize", feed: [] });
    // 06 §5.4: the caret lands on the first line of the next RUNNABLE block.
    // That move is `submitRun`'s, not this file's — which is what makes §38-A
    // step 4 an assertion about the product.
    const blocks = present(bridge.snapshot(["blocks"]).blocks, "blocks");
    const caret = present(bridge.snapshot(["doc"]).doc, "doc").caret;
    expect(caret).toBe(blocks[2]?.span[0]);
  });

  it("renders W14's real card and reads its header and body back out", () => {
    const hash = new Array<number>(16).fill(2);
    bridge.dispatch({ action: "open_doc", fixture: "scenario_a.do", text: AUTO_DO, feed: [] });
    bridge.dispatch({ action: "place_caret", offset: 20 });
    bridge.dispatch({
      action: "run",
      verb: "run.blockAndAdvance",
      label: "summarize price mpg",
      feed: [
        {
          event: "block_map_changed",
          seq: 1,
          map: { blocks: [1, 2, 3], regions: [{}, { code_hash: hash, hash_ordinal: 0 }, {}] },
        },
        {
          event: "status_changed",
          seq: 2,
          doc: 1,
          changed: [[2, { state: "current", exec: 2, dataset: 17, duration_us: 8412 }]],
        },
        {
          event: "result",
          seq: 3,
          exec: 2,
          envelope: {
            result: 2,
            revision: 0,
            exec: 2,
            block: 2,
            dataset_state: 17,
            code_hash: hash,
            cmdline: "summarize price mpg",
            duration_us: 8412,
            rc: 0,
            payloads: [
              {
                kind: "summarize",
                detail: false,
                weight: null,
                qualifier: null,
                rows: [
                  {
                    var: "price",
                    label: null,
                    format: "%8.0gc",
                    missing: 0,
                    display: {
                      obs: "74",
                      mean: "6165.257",
                      sd: "2949.496",
                      min: "3291",
                      max: "15906",
                    },
                    detail: null,
                    var_kind: "numeric",
                    sparkline: null,
                  },
                ],
              },
            ],
            raw: { head: "       price |         74    6165.257\n", bytes: 0, lines: 1 },
            layout_hint: { rows: 1, cols: 6, est_px: 96 },
            actions: [{ action: "raw_output" }],
          },
        },
      ],
    });

    const cards = present(bridge.snapshot(["cards"]).cards, "cards");
    expect(cards).toHaveLength(1);
    // `data-card-cmd` — the header W14's card shell actually draws, not the
    // cmdline copied out of the envelope, which would assert nothing about it.
    expect(cards[0]?.header).toBe("summarize price mpg");
    expect(cards[0]?.body.some((l) => l.includes("6165.257"))).toBe(true);
    expect(cards[0]?.block).toBe(1);

    // And the gutter agrees, through `displayStatus(anchorForBlock(...))`.
    const gutter = present(bridge.snapshot(["gutter"]).gutter, "gutter");
    expect(gutter.find((g) => g.block === 1)?.glyph).toBe("current");
  });
});

// ---------------------------------------------------------------------------
// The two boundary conversions, and the defects behind them
// ---------------------------------------------------------------------------

describe("the wire/UI boundary", () => {
  it("converts a 16-byte CodeHash into the 32-hex one the UI is typed on", () => {
    expect(hexOfCodeHash(new Array<number>(16).fill(255))).toBe("ff".repeat(16));
    expect(() => hexOfCodeHash("deadbeef")).toThrow(TypeError);
    expect(() => hexOfCodeHash([1, 2, 300])).toThrow(TypeError);
  });

  /**
   * **Tripwire.** `state/results.ts` reads `envelope.id`; CONTRACTS §5 names the
   * field `result`. Until one of the two moves, a wire-shaped envelope handed
   * straight to `recordResult` is filed under the string `"undefined"`.
   *
   * Asserting the defect rather than only the shim, so that the day
   * `HasResultId` is re-typed against the contract this fails and the shim comes
   * out. Reported in W25's return.
   */
  it("still needs a shim because the store reads .id and the contract carries .result", async () => {
    const { recordResult, resultState } = await import("../state/results.ts");
    const wire = { result: 7, cmdline: "summarize price" } as unknown as { id: never };
    recordResult(wire);
    expect(
      Object.keys(resultState.byId),
      "state/results.ts now reads the contract's field name: delete storeShapeOf() in bridge.ts",
    ).toContain("undefined");

    expect(storeShapeOf({ result: 7 })["id"]).toBe(7);
  });
});

// ---------------------------------------------------------------------------
// The server loop — only when the harness asked for it
// ---------------------------------------------------------------------------

const PORT = Number.parseInt(process.env["STRATUM_E2E_PORT"] ?? "0", 10);

if (Number.isInteger(PORT) && PORT > 0) {
  it(
    "serves the tier-1 harness",
    async () => {
      const bridge = await createBridge({ segmenter: await segmenter() });
      installBridge(bridge);
      await registerAllCommands();

      await new Promise<void>((resolve, reject) => {
        const socket = createConnection(
          { port: PORT, host: process.env["STRATUM_E2E_HOST"] ?? "127.0.0.1" },
          () => {},
        );
        socket.setNoDelay(true);
        let pending = "";

        const reply = (response: Response): void => {
          socket.write(`${JSON.stringify(response)}\n`);
        };

        socket.on("data", (chunk: Buffer) => {
          pending += chunk.toString("utf8");
          for (;;) {
            const nl = pending.indexOf("\n");
            if (nl < 0) break;
            const line = pending.slice(0, nl);
            pending = pending.slice(nl + 1);
            if (line.trim() === "") continue;

            let request: Request;
            try {
              request = JSON.parse(line) as Request;
            } catch (error) {
              reply({ id: 0, ok: false, error: `unparseable request: ${String(error)}` });
              continue;
            }

            try {
              switch (request.op) {
                case "hello":
                  reply({
                    id: request.id,
                    ok: true,
                    // The bridge's own constant, never a second copy of it: the
                    // two drifted apart in wave 1 and the report printed the
                    // stale one.
                    host: HOST_NAME,
                    capabilities: bridge.capabilities(),
                  });
                  break;
                case "dispatch":
                  reply({ id: request.id, ok: true, dispatched: bridge.dispatch(request.action) });
                  break;
                case "snapshot":
                  reply({
                    id: request.id,
                    ok: true,
                    snapshot: bridge.snapshot(request.what as Section[]),
                  });
                  break;
                case "quit":
                  reply({ id: request.id, ok: true });
                  socket.end();
                  resolve();
                  break;
                default:
                  reply({ id: 0, ok: false, error: "unknown op" });
              }
            } catch (error) {
              reply({ id: request.id, ok: false, error: String(error) });
            }
          }
        });

        socket.on("error", reject);
        socket.on("close", () => resolve());
      });
    },
    // The harness's own deadline is what turns a hang into a report with the
    // last snapshot in it; this one only stops a forgotten process.
    10 * 60_000,
  );
}
