/**
 * The dev-only dispatch/snapshot bridge — plan W25, the frontend half.
 *
 * `e2e_dispatch { action, args }` routes into the **same** command registry the
 * keymap and the command palette use, and `e2e_snapshot { what }` reads the
 * **same** stores the panes read. That is the whole design: a tier-1 test
 * presses `run.blockAndAdvance`, not a pixel, and what it observes afterwards is
 * the application's own state rather than a shadow copy this file keeps.
 *
 * # The rule this file is written to
 *
 * **Nothing is invented, and nothing stays owed once its owner has shipped.**
 * Where the application has no answer yet the snapshot field says which unit
 * owes it ({@link owedBy}) instead of producing a plausible value: a bridge that
 * filled those in would turn a harness into a mirror, and a green scenario
 * against a mirror is indistinguishable in a summary from a green scenario
 * against the product.
 *
 * The second half of that rule is what repair round 3 is about. Through wave 1
 * this file blocked `doc`, `gutter` and `cards` on W13 and W14 with reasons that
 * said their work did not exist — and both units landed in that same wave, so
 * the ledger went on understating the tree while looking rigorous. Nothing
 * failed, because a sentence has no truth value a test can read.
 *
 * So every `owedBy` now carries a **witness**: the repo-relative path whose
 * ABSENCE is the claim. `has no expired claim in its blocked ledger`
 * (`serve.test.ts`) and `assert_no_expired_claims` (the live half of
 * `tests/e2e/harness.rs`) both fail if any witness exists in the tree. The
 * ledger cannot go stale again without a red test in the same run that prints
 * it.
 *
 * Everything is answered from the real thing:
 *
 * | field | source |
 * |---|---|
 * | `doc` | W13's real `EditorState` — `doc.toString()`, `selection.main.head` |
 * | `blocks` | `allBlocks()` over W13's `blockField`, driving the real segmenter |
 * | `blocks[].status` | `displayedStatus` — `worseOf(local, kernel)`, `state/doc.ts` |
 * | `gutter` | `displayStatus(anchorForBlock(...))` — the gutter's own function |
 * | `cards` | W14's `ResultCard`, rendered, then read back out of the DOM |
 * | `results` | `state/results.ts`, keyed by `clientKey(hash, ordinal)` |
 * | `history` | `state/history.ts` |
 * | `layout` | `state/layout.ts` + `state/settings.ts` |
 * | `focus` | `EditorView.hasFocus` — the only focusable surface here |
 * | `panes[].visible` | `paneOrder()` over the live `LayoutSpec` |
 * | `panes[].content` | the pane's own DOM, mounted through W12's `registerPane` |
 *
 * What is still owed is owed per PANE, not wholesale: `variables`,
 * `properties`, `project` and `viewer` have no module, and each says so with
 * the path it is waiting for.
 *
 * # What this host still cannot DO
 *
 * Reading a pane is not driving one. There is no pointer here and no command
 * bar focus, so `Capability::Panes` is deliberately NOT advertised: the
 * `Submit` and `Click` actions have no implementation and the harness must go
 * on reporting those steps blocked rather than failing them. Real input is
 * tier 2's whole reason to exist (ADR-011).
 *
 * # Two defects found by being the first caller to push real envelopes through
 *
 * Both are shimmed HERE, in one place each, loudly, and both have a test in
 * `serve.test.ts` that fails the day they are fixed. They are reported in W25's
 * return rather than quietly absorbed.
 *
 * 1. **`ResultEnvelope.result` vs `HasResultId.id`.** CONTRACTS §5 names the
 *    field `result`; `state/results.ts` reads `envelope.id`. A wire-shaped
 *    envelope therefore files itself under the string `"undefined"` and
 *    `latestResult()` never updates. {@link storeShapeOf} renames it.
 * 2. **`CodeHash` is 16 bytes on the wire and 32 hex characters in the UI.**
 *    CONTRACTS §1.1 declares `CodeHash(pub [u8; 16])` with a derived
 *    `Serialize`, so JSON and MessagePack both carry an array of integers;
 *    CONTRACTS §12 and `src/ipc/hand.ts` declare `CodeHash = string` and
 *    `codeHash()` throws a `TypeError` on anything that is not 32 lowercase hex.
 *    Nothing in `stratum-proto` converts between them. {@link hexOfCodeHash}
 *    does it at the boundary.
 */

import type { RunVerb } from "../editor/blocks/run.ts";
import type { Block } from "../editor/blocks/segmenter.ts";
import type { ExecRecord } from "../editor/results/anchor.ts";
import {
  type CodeHash,
  type DocumentId,
  asDatasetStateId,
  asDocumentId,
  asExecId,
  asResultId,
  clientKey,
  codeHash,
} from "../ipc/hand.ts";
import type { HasBlockState } from "../ipc/hand.ts";
import type { KeyContext } from "../keys/context.ts";
import { presetKeymap } from "../keys/presets.ts";
import { runCommand } from "../keys/registry.ts";
import { parseKeystroke } from "../keys/trie.ts";
import type { ResultEnvelopeView } from "../renderers/types.ts";
import {
  displayedStatus,
  openDocument,
  resetDocState,
  setExecutedHash,
  setKernelStatus,
} from "../state/doc.ts";
import { historyState, resetHistoryState } from "../state/history.ts";
import { currentLayoutId, layoutSpec, paneOrder } from "../state/layout.ts";
import type { HasResultId } from "../state/results.ts";
import {
  RESIDENT_CAP,
  recordResult,
  resetResultState,
  result as resultById,
  resultState,
  resultsForBlock,
} from "../state/results.ts";
import { effectiveInlineResults, resetSettings } from "../state/settings.ts";
import type { StratumSegmenter } from "../wasm/types.ts";
import { ensureDom } from "./dom.ts";
import {
  type Action,
  BRIDGE_GLOBAL,
  type BlockView,
  type Capability,
  type Card,
  type Dispatched,
  type DocView,
  type E2eBridge,
  type Glyph,
  type GutterRow,
  type HistoryRow,
  type PaneView,
  type ResultView,
  type Section,
  type Snapshot,
  owedBy,
  present,
} from "./protocol.ts";

/** The one document this bridge holds. Matches W07's canned stream. */
const DOC: DocumentId = asDocumentId(1);

/**
 * Who owes each pane, and the module whose absence is the claim.
 *
 * Transcribed from `docs/ownership.toml` — the `apps/desktop/src/panes/*` and
 * `apps/desktop/src/commandbar/**` globs — and only used for a pane W12's
 * registry reports as unregistered. The witness is checked by
 * `the_blocked_ledger_has_not_expired` in `serve.test.ts`, so a wrong entry here
 * fails a test rather than misattributing a blocked step for a wave.
 *
 * `editor` is the one entry whose witness path is claimed by NO unit in the
 * manifest — `apps/desktop/src/panes/editor/**` is missing from the partition,
 * which W13 escalated in the header of its `registerEditorPane`. It is
 * attributed to W13 because that is where the registration lives.
 */
const PANE_OWNERS: Readonly<Record<string, { unit: string; witness: string }>> = {
  editor: { unit: "W13", witness: "apps/desktop/src/panes/editor/index.tsx" },
  sections: { unit: "W13", witness: "apps/desktop/src/panes/sections/index.tsx" },
  results: { unit: "W14", witness: "apps/desktop/src/panes/results/index.tsx" },
  compare: { unit: "W14", witness: "apps/desktop/src/panes/compare/index.tsx" },
  repro: { unit: "W15", witness: "apps/desktop/src/panes/repro/index.tsx" },
  history: { unit: "W16", witness: "apps/desktop/src/panes/history/index.tsx" },
  variables: { unit: "W16", witness: "apps/desktop/src/panes/variables/index.tsx" },
  properties: { unit: "W16", witness: "apps/desktop/src/panes/properties/index.tsx" },
  project: { unit: "W16", witness: "apps/desktop/src/panes/project/index.tsx" },
  viewer: { unit: "W16", witness: "apps/desktop/src/panes/viewer/index.tsx" },
  commandbar: { unit: "W16", witness: "apps/desktop/src/commandbar/index.tsx" },
  dataeditor: { unit: "W18", witness: "apps/desktop/src/panes/dataeditor/index.tsx" },
  graphs: { unit: "W19", witness: "apps/desktop/src/panes/graphs/index.tsx" },
  assistant: { unit: "W21", witness: "apps/desktop/src/panes/assistant/index.tsx" },
};

/**
 * A pane id the table above does not know.
 *
 * `PANE_COMPONENT_IDS` and this table are two lists that must agree, and the day
 * they stop agreeing the honest answer is "the partition does not say", not a
 * guess. The witness is the module such a pane would live in, which by
 * definition does not exist yet.
 */
const unclaimedPane = (id: string): { unit: string; witness: string } => ({
  unit: "ARCHITECT",
  witness: `apps/desktop/src/panes/${id}/index.tsx`,
});

/**
 * Where the harness is told its answers come from.
 *
 * Named for what it actually is, because the report prints it beside every
 * scenario and "pre-host bridge" alone read, for a whole wave, as "a stand-in
 * for the app". It is the real W12 stores, the real W13 editor and the real W14
 * renderers, in a node process; what it is NOT is W17's packaged host, which is
 * what tier 1 will drive when that lands.
 */
export const HOST_NAME = "pre-host bridge (node + jsdom over the app's own modules)";

// ---------------------------------------------------------------------------
// Boundary conversions — see the two defects in the module header
// ---------------------------------------------------------------------------

/** `[u8; 16]` on the wire → the 32-lowercase-hex `CodeHash` the UI is typed on. */
export function hexOfCodeHash(bytes: unknown): CodeHash {
  if (!Array.isArray(bytes)) throw new TypeError("code_hash is not a byte array");
  const hex = bytes
    .map((b) => {
      const n = typeof b === "number" ? b : Number.NaN;
      if (!Number.isInteger(n) || n < 0 || n > 255) throw new TypeError("code_hash has a non-byte");
      return n.toString(16).padStart(2, "0");
    })
    .join("");
  return codeHash(hex);
}

/**
 * The envelope as `state/results.ts` expects to receive it.
 *
 * One property added, nothing removed: the store reads `.id`, the contract
 * carries `.result`, and until one of the two moves this is the join.
 */
export function storeShapeOf(
  envelope: Record<string, unknown>,
): HasResultId & Record<string, unknown> {
  const id = envelope["result"];
  if (typeof id !== "number") throw new TypeError("ResultEnvelope.result is not a number");
  return { ...envelope, id: asResultId(id) };
}

// ---------------------------------------------------------------------------
// Narrowing helpers for untrusted wire values
// ---------------------------------------------------------------------------

const asRecord = (v: unknown): Record<string, unknown> =>
  typeof v === "object" && v !== null ? (v as Record<string, unknown>) : {};

const asString = (v: unknown, fallback = ""): string => (typeof v === "string" ? v : fallback);

const asNumber = (v: unknown, fallback = 0): number => (typeof v === "number" ? v : fallback);

const asArray = (v: unknown): unknown[] => (Array.isArray(v) ? v : []);

/** A `KeyContext` from untrusted JSON: only the three value shapes it allows. */
const asKeyContext = (v: unknown): KeyContext => {
  const out: Record<string, boolean | string | number> = {};
  for (const [k, value] of Object.entries(asRecord(v))) {
    if (typeof value === "boolean" || typeof value === "string" || typeof value === "number") {
      out[k] = value;
    }
  }
  return out;
};

// ---------------------------------------------------------------------------
// The bridge
// ---------------------------------------------------------------------------

interface BridgeOptions {
  segmenter: StratumSegmenter;
  /** Which keymap preset the chord assertions resolve against. */
  preset?: "modern" | "stata" | "vscode";
  /** `Mod` expands to Cmd here and Ctrl elsewhere; the trie needs to know. */
  platform?: "macos" | "windows" | "linux";
}

/**
 * Every module the bridge reaches for that touches the DOM at import time.
 *
 * Loaded through `await import(...)` rather than a static import for the reason
 * `dom.ts`'s header gives: static imports are hoisted above every statement, so
 * a module that needs `window` to exist cannot be pulled in by a file that is
 * also responsible for creating one.
 */
async function loadHostModules() {
  const [editor, blocks, run, anchor, widget, orphans, collapse, solid, renderers, panes] =
    await Promise.all([
      import("../editor/setup.ts"),
      import("../editor/blocks/blockField.ts"),
      import("../editor/blocks/run.ts"),
      import("../editor/results/anchor.ts"),
      import("../editor/results/widget.ts"),
      import("../editor/results/orphans.ts"),
      import("../editor/results/collapse.ts"),
      import("solid-js/web"),
      import("../renderers/index.ts"),
      import("../dock/panes.ts"),
    ]);
  return { editor, blocks, run, anchor, widget, orphans, collapse, solid, renderers, panes };
}

/**
 * Register every pane whose module exists, without naming any of them.
 *
 * `import.meta.glob` rather than a list of imports: a list is a second place to
 * remember a unit landed, and forgetting to update it is precisely the defect
 * repair round 3 exists to fix. The convention is the one every pane already
 * follows — `panes/<id>/index.tsx` (or `commandbar/index.tsx`) exporting a
 * `register<Name>Pane()` that calls W12's `registerPane` — so a pane written
 * next week is picked up here with no edit to this file.
 *
 * Each registration is attempted independently. A pane whose module throws on
 * registration is left unregistered, which the snapshot then reports as owed
 * with the throw as its reason, rather than taking the whole host down: this
 * bridge runs against a tree where most units are mid-flight.
 */
async function mountAvailablePanes(
  registerPane: typeof import("../dock/panes.ts").registerPane,
  render: typeof import("solid-js/web").render,
  propsFor: (id: string) => Record<string, unknown> | undefined,
): Promise<Map<string, string>> {
  const notMounted = new Map<string, string>();
  const modules = {
    ...import.meta.glob("../panes/*/index.tsx"),
    ...import.meta.glob("../commandbar/index.tsx"),
  } as Record<string, () => Promise<Record<string, unknown>>>;

  for (const [path, load] of Object.entries(modules)) {
    const id = path
      .replace(/^\.\.\/panes\//, "")
      .replace(/^\.\.\//, "")
      .replace(/\/index\.tsx$/, "");
    try {
      const module = await load();
      const registrar = Object.entries(module).find(
        ([name, value]) =>
          typeof value === "function" && name.startsWith("register") && name.endsWith("Pane"),
      );
      if (registrar !== undefined) {
        (registrar[1] as () => unknown)();
        continue;
      }

      // No registrar. W14's `results` and `compare` panes are the case: they
      // export a component that takes its data as props — which is the right
      // shape (the pane reports, the host supplies) but means only a HOST can
      // mount them. This is a host, so it supplies what it has. Reported in
      // W25's return: W13 and W16 both ship a `register…Pane` and W14 does not,
      // so nothing can mount W14's panes generically.
      const component = Object.entries(module).find(
        ([name, value]) => typeof value === "function" && /^[A-Z].*Pane$/.test(name),
      );
      const props = propsFor(id);
      if (component === undefined || props === undefined) {
        notMounted.set(
          id,
          component === undefined
            ? `${path} exports neither a register…Pane nor a …Pane component`
            : `${path} exports ${component[0]} but takes host-supplied props this host has none of`,
        );
        continue;
      }
      const draw = component[1] as (p: Record<string, unknown>) => unknown;
      registerPane(id as never, (element, register) => {
        register(render(() => draw(props) as never, element));
      });
    } catch (error) {
      notMounted.set(id, `mounting it threw: ${String(error)}`);
    }
  }
  return notMounted;
}

/**
 * Build the bridge.
 *
 * `async` since repair round 3: the editor and the renderers are the product's
 * own modules and both need a `document` to have been created before they are
 * evaluated. See `dom.ts`.
 */
export async function createBridge(options: BridgeOptions): Promise<E2eBridge> {
  const seg = options.segmenter;
  const preset = options.preset ?? "modern";
  const platform = options.platform ?? "macos";
  const trie = presetKeymap(preset, platform);

  const dom = await ensureDom();
  const host = await loadHostModules();
  const paneFailures = await mountAvailablePanes(
    host.panes.registerPane,
    host.solid.render,
    (id) => {
      // Getters, not snapshots of the arrays: Solid tracks the read, so a pane
      // mounted once here re-renders when the store behind it changes — which
      // is what makes `panes[].content` an observation of the live app rather
      // than of whatever the store held when the bridge started.
      switch (id) {
        case "results":
          return {
            get envelopes() {
              return residentEnvelopes();
            },
          };
        // An empty Compare pane is a real state of that pane (nothing pinned),
        // not a stand-in: `buildCompareTable([])` is the pane's own empty case.
        case "compare":
          return { models: [] };
        default:
          return undefined;
      }
    },
  );

  let opened = false;
  /** The path `open_doc` was given, which is what `doc.path` reports. */
  let docPath: string | null = null;
  /** Engine `BlockId` → index among executable regions. */
  const blockIndexOf = new Map<number, number>();
  /** Block index → the `CodeHash` the engine reported for its last execution. */
  const engineHash = new Map<number, string>();
  /** Block index → the `(hash, ordinal)` its results are filed under. */
  const resultKey = new Map<number, { hash: CodeHash; ordinal: number }>();
  /** Executable block index → the id of the `ResultAnchor` its last run opened. */
  const anchorOf = new Map<number, number>();
  let last: Dispatched = { via: "", result: "ran", chord_resolves_to: null, events_applied: 0 };

  // The real editor, on a detached element. Not `mountEditor` from W13's
  // `harness.ts`, which is that unit's *test* seam: `createEditor` is the
  // function `registerEditorPane` and W17's host will call, so what tier 1
  // drives is the editor the product ships rather than one assembled for a test.
  const editorParent = dom.document.createElement("div");
  dom.document.body.append(editorParent);
  const view = host.editor.createEditor(editorParent, "", { segmenter: seg });
  view.focus();

  /** Where W14's cards are rendered so their text can be read back out. */
  const cardParent = dom.document.createElement("div");
  dom.document.body.append(cardParent);

  const text = (): string => view.state.doc.toString();

  /**
   * Every resident envelope, oldest first — `state/results.ts`'s own order.
   *
   * A `function` and not a `const` arrow because the pane props above read it
   * during `mountAvailablePanes`, which runs before the rest of this body.
   */
  function residentEnvelopes(): readonly ResultEnvelopeView[] {
    return resultState.order
      .map((id) => resultById(id))
      .filter((e): e is HasResultId => e !== undefined) as unknown as ResultEnvelopeView[];
  }

  const executableRegions = (): Block[] =>
    host.blocks.allBlocks(view.state).filter((b) => b.executable);

  const regionAt = (index: number): Block | undefined => executableRegions()[index];

  /**
   * `displayedStatus`'s `local` argument.
   *
   * The segmenter's hash, never the engine's. The two are the same function of
   * the same text in production (CONTRACTS §1.2), but W07's canned stream uses
   * synthetic constants — `CodeHash([n; 16])` — so comparing them here would
   * mark every executed block stale for a reason that is an artifact of the
   * fixture rather than a property of the product. `blocks[].engine_hash` is
   * reported alongside so a host with a real engine can be asserted to agree.
   */
  const localHash = (index: number): string | undefined => regionAt(index)?.hashKey;

  /**
   * Open a document: one transaction, caret at the top.
   *
   * The segmenter is NOT poked directly any more. `blockField` owns the wasm
   * mirror and splices this change into it (`applyChanges`), which is the same
   * path a keystroke takes — a bridge that called `setDoc` itself would be
   * testing a second segmentation strategy that the product never runs.
   */
  function openText(next: string): void {
    view.dispatch({
      changes: { from: 0, to: view.state.doc.length, insert: next },
      selection: { anchor: 0 },
    });
  }

  /** Scenario B's transformation edit. A document change, caret left alone. */
  function replaceRange(from: number, to: number, insert: string): void {
    view.dispatch({ changes: { from, to, insert } });
  }

  /** Every anchor id currently in the document, for the new-anchor diff below. */
  const anchorIds = (): number[] =>
    host.anchor.anchorsIn(view.state, 0, view.state.doc.length).map((a) => a.rec.id);

  /**
   * Remember which anchor `submitRun` just opened for which executable block.
   *
   * The engine's events are keyed by `BlockId` and the anchors by a
   * window-local id, and nothing in the tree joins the two: `BlockMap` is the
   * join in production and W07's canned stream carries one, so the bridge maps
   * `BlockId → executable index` from the map and `executable index → anchor id`
   * from here. Diffing rather than reading `submitRun`'s return value because it
   * returns a boolean — asking W13 to return the ids would be an API change this
   * unit needs and the editor does not.
   */
  function noteNewAnchors(before: Set<number>): void {
    executableRegions().forEach((block, index) => {
      const rec = host.anchor.anchorForBlock(view.state, block);
      if (rec !== null && !before.has(rec.id)) anchorOf.set(index, rec.id);
    });
  }

  /** Tell the editor what the engine said about a run it opened an anchor for. */
  function patchAnchor(index: number, patch: Partial<Omit<ExecRecord, "id">>): void {
    const id = anchorOf.get(index);
    if (id === undefined) return;
    view.dispatch({ effects: host.anchor.updateRun.of({ id, patch }) });
  }

  function applyEvent(raw: unknown): boolean {
    const ev = asRecord(raw);
    switch (asString(ev["event"])) {
      case "block_map_changed": {
        const map = asRecord(ev["map"]);
        const blocks = asArray(map["blocks"]);
        const regions = asArray(map["regions"]);
        blockIndexOf.clear();
        blocks.forEach((id, index) => {
          if (typeof id === "number") blockIndexOf.set(id, index);
        });
        regions.forEach((r, index) => {
          const region = asRecord(r);
          const hash = region["code_hash"];
          if (hash !== undefined) {
            resultKey.set(index, {
              hash: hexOfCodeHash(hash),
              ordinal: asNumber(region["hash_ordinal"]),
            });
          }
        });
        return true;
      }
      case "status_changed": {
        for (const pair of asArray(ev["changed"])) {
          const tuple = asArray(pair);
          const id = asNumber(tuple[0], -1);
          const index = blockIndexOf.get(id);
          const status = asRecord(tuple[1]);
          if (index !== undefined && typeof status["state"] === "string") {
            setKernelStatus(DOC, index, status as unknown as HasBlockState);
            // The same verdict to the editor's anchor, which is what the gutter
            // reads. Two stores, one event, no second staleness rule: the gutter
            // glyph is `displayStatus(rec, block)` and the pane's is
            // `displayedStatus(doc, index, local)`, and both are the product's.
            patchAnchor(index, { kernel: status as unknown as HasBlockState });
          }
        }
        return true;
      }
      case "block_started": {
        const index = blockIndexOf.get(asNumber(ev["block"], -1));
        if (index === undefined) return true;
        const hash = hexOfCodeHash(ev["code_hash"]);
        engineHash.set(index, hash);
        resultKey.set(index, { hash, ordinal: resultKey.get(index)?.ordinal ?? 0 });
        // The LOCAL hash at the moment of execution — see `localHash`.
        const local = localHash(index);
        if (local !== undefined) setExecutedHash(DOC, index, codeHash(local));
        return true;
      }
      case "result": {
        const envelope = asRecord(ev["envelope"]);
        const index = blockIndexOf.get(asNumber(envelope["block"], -1));
        const key =
          index === undefined
            ? undefined
            : (resultKey.get(index) ?? {
                hash: hexOfCodeHash(envelope["code_hash"]),
                ordinal: 0,
              });
        recordResult(storeShapeOf(envelope), key);
        if (index !== undefined) {
          // The card's own record. `durationMs` is RECORDED, never asserted
          // (ADR-017) — it is what the card's readout shows.
          patchAnchor(index, {
            result: asResultId(asNumber(envelope["result"], -1)),
            exec: asExecId(asNumber(envelope["exec"], 0)),
            dataset: asDatasetStateId(asNumber(envelope["dataset_state"], 0)),
            durationMs: Math.round(asNumber(envelope["duration_us"]) / 1000),
            streaming: false,
          });
        }
        return true;
      }
      // Consumed and deliberately not modelled: this bridge is not a second
      // frontend. `Output` belongs to W16's log pane, `StateChanged` to W18's
      // variables pane, `RunStarted`/`RunFinished`/`BlockFinished` to W15's run
      // chrome. Counting them keeps `events_applied` honest — the harness
      // asserts the whole canned run was consumed, not that we understood it.
      default:
        return true;
    }
  }

  function resolveChord(chord: unknown, context: unknown): string | null {
    if (typeof chord !== "string" || chord === "") return null;
    const ctx = asRecord(context) as KeyContext;
    try {
      const strokes = chord.trim().split(/\s+/);
      let prefix: string[] = [];
      for (const stroke of strokes) {
        const resolution = trie.resolve(prefix, parseKeystroke(stroke, platform), ctx);
        if (resolution.kind === "command") return resolution.command;
        if (resolution.kind === "pending") {
          prefix = [...resolution.prefix];
          continue;
        }
        return null;
      }
      return null;
    } catch {
      // A chord this build cannot parse is "bound to nothing", which is what the
      // harness will report. Throwing here would lose the scenario's context.
      return null;
    }
  }

  function feed(events: unknown): number {
    let applied = 0;
    for (const ev of asArray(events)) if (applyEvent(ev)) applied += 1;
    return applied;
  }

  function dispatch(action: Action): Dispatched {
    const kind = asString(action["action"]);
    const out: Dispatched = {
      via: "bridge",
      result: "ran",
      chord_resolves_to: resolveChord(action["chord"], action["context"]),
      events_applied: 0,
    };

    switch (kind) {
      case "open_doc": {
        docPath = asString(action["fixture"]);
        openText(asString(action["text"]));
        openDocument({
          doc: DOC,
          path: docPath,
          version: 1,
          eol: "lf",
          bom: false,
          ownerLabel: "main",
          dirty: false,
        });
        opened = true;
        out.events_applied = feed(action["feed"]);
        out.via = out.events_applied > 0 ? "injection" : "bridge";
        break;
      }
      case "place_caret": {
        const offset = Math.max(0, Math.min(asNumber(action["offset"]), view.state.doc.length));
        view.dispatch({ selection: { anchor: offset } });
        out.via = "bridge";
        break;
      }
      case "run": {
        // The real run path. `submitRun` opens one anchor per block in ONE
        // transaction and, for `run.blockAndAdvance`, moves the caret to the
        // next runnable block — that caret move is the product's, not this
        // file's, which is the whole reason §38-A's step 4 can be asserted.
        const verb = asString(action["verb"], "run.block") as RunVerb;
        const before = new Set(anchorIds());
        if (host.run.submitRun(view, verb)) {
          noteNewAnchors(before);
        } else {
          // No runnable block under the caret. Reported rather than swallowed:
          // a scenario whose run silently did nothing must fail, not pass with
          // an unchanged snapshot.
          out.result = "disabled";
        }
        out.events_applied = feed(action["feed"]);
        out.via = "injection";
        break;
      }
      case "edit": {
        const span = asArray(action["span"]);
        replaceRange(asNumber(span[0]), asNumber(span[1]), asString(action["text"]));
        out.via = "bridge";
        break;
      }
      case "observe": {
        out.via = "observe";
        break;
      }
      case "verb": {
        const args = action["args"];
        out.result = runCommand(asString(action["command"]), args, asKeyContext(action["context"]));
        out.via = "verb";
        break;
      }
      // Everything else needs a pane, an editor or a pointer. The harness gates
      // those steps on a capability this bridge does not advertise, so reaching
      // here at all means the gate is wrong — say so rather than pretending.
      default:
        out.result = "unknown";
        out.via = "bridge";
        break;
    }

    last = out;
    return out;
  }

  function blocks(): BlockView[] {
    return executableRegions().map((r, index) => {
      const local = r.hashKey;
      const status = displayedStatus(DOC, index, codeHash(local));
      return {
        index,
        span: [r.from, r.to] as [number, number],
        status: status.state as Glyph,
        hash: local,
        engine_hash: engineHash.get(index) ?? null,
      };
    });
  }

  function results(): ResultView[] {
    const out: ResultView[] = [];
    for (const [index, key] of [...resultKey.entries()].sort((a, b) => a[0] - b[0])) {
      for (const id of resultsForBlock(key.hash, key.ordinal)) {
        const envelope = asRecord(resultById(id));
        const raw = asRecord(envelope["raw"]);
        out.push({
          result: asNumber(envelope["id"], -1),
          block: index,
          client_key: clientKey(key.hash, key.ordinal),
          cmdline: asString(envelope["cmdline"]),
          rc: asNumber(envelope["rc"]),
          raw_head: asString(raw["head"]).split("\n"),
          payloads: asArray(envelope["payloads"]).map((p) => asString(asRecord(p)["kind"])),
        });
      }
    }
    return out;
  }

  function panes(): PaneView[] {
    return paneOrder().map((id) => {
      // W12's own pane registry. `paneHost` builds the persistent element a
      // pane mounts into and marks it `data-unregistered` when no unit has
      // claimed the id — which is exactly the question this field is asking, so
      // it is asked of the registry rather than of a list kept here.
      const element = host.panes.paneHost(id);
      if (element.hasAttribute("data-unregistered")) {
        const owed = PANE_OWNERS[id] ?? unclaimedPane(id);
        const reason = paneFailures.get(id);
        return {
          id,
          visible: true,
          content: owedBy<string[]>(
            owed.unit,
            reason ?? `no module registered a \`${id}\` pane`,
            // A module that exists but could not be mounted is NOT waiting on a
            // file, and saying it were would be the stale-claim bug again in a
            // new place. The witness is then the pane host itself, which cannot
            // be a path in the tree.
            reason === undefined ? owed.witness : `(mounted from ${id}, but: ${reason})`,
          ),
        };
      }
      const lines: string[] = [];
      for (const child of element.children) cardLines(child, lines);
      return { id, visible: true, content: present(lines) };
    });
  }

  /**
   * The gutter, from the gutter's own function.
   *
   * `blockGutter()`'s markers are built from `displayStatus(anchorForBlock(…))`
   * and turned into SVG by `glyphNode`. Reading the rendered gutter DOM back
   * would be reading `glyphNode`'s `path` geometry, which is a test of the icon
   * set; what a scenario means by "the glyph on block 1" is the state that
   * chooses the icon, so this calls the same function the marker calls and
   * stops there.
   */
  function gutter(): GutterRow[] {
    return executableRegions().map((block, index) => ({
      block: index,
      glyph: host.widget.displayStatus(
        host.anchor.anchorForBlock(view.state, block),
        block,
      ) as Glyph,
    }));
  }

  /**
   * Container tags whose children are separate lines on a card.
   *
   * Anything else is a leaf for reporting purposes: a `<p>` with three `<span>`s
   * in it is one sentence, and splitting it would make `CardBodyContains` depend
   * on where a renderer chose to open a span.
   */
  const CARD_CONTAINERS = new Set([
    "DIV",
    "SECTION",
    "ARTICLE",
    "HEADER",
    "FOOTER",
    "FIGURE",
    "UL",
    "OL",
    "DL",
  ]);

  function cardLines(node: Element, out: string[]): void {
    if (node.tagName === "TABLE") {
      for (const row of node.querySelectorAll("tr")) {
        const cells = [...row.querySelectorAll("th,td")].map((c) => (c.textContent ?? "").trim());
        const line = cells.join("  ").trim();
        if (line.length > 0) out.push(line);
      }
      return;
    }
    const children = [...node.children];
    if (children.length > 0 && (CARD_CONTAINERS.has(node.tagName) || node.querySelector("table"))) {
      for (const child of children) cardLines(child, out);
      return;
    }
    const line = (node.textContent ?? "").trim();
    if (line.length > 0) out.push(line);
  }

  /**
   * One rendered W14 card, memoised by result id.
   *
   * Memoised because a card is drawn once per result and read on every snapshot,
   * and a scenario takes a snapshot after every step: re-rendering the whole
   * scrollback per snapshot would make the harness's cost quadratic in the
   * number of steps for no new information — the envelope is immutable once
   * recorded (`state/results.ts` replaces, never edits).
   */
  const cardCache = new Map<number, { header: string; body: string[] }>();

  function drawCard(id: number, envelope: ResultEnvelopeView): { header: string; body: string[] } {
    const cached = cardCache.get(id);
    if (cached !== undefined) return cached;

    const slot = dom.document.createElement("div");
    cardParent.append(slot);
    const dispose = host.solid.render(
      () => host.renderers.ResultCard({ envelope }) as unknown as Element,
      slot,
    );
    const header = slot.querySelector("[data-card-cmd]")?.textContent?.trim() ?? "";
    const body: string[] = [];
    for (const section of slot.querySelectorAll(".card__section")) cardLines(section, body);
    dispose();
    slot.remove();

    const drawn = { header, body };
    cardCache.set(id, drawn);
    return drawn;
  }

  /**
   * The cards, in document order, one per executed block that has a result.
   *
   * An anchor whose engine answer has not arrived yet is a *running* card in the
   * product; it has no envelope, so there is nothing here to read back and it is
   * omitted rather than filled in with the anchor's own label.
   */
  function cards(): Card[] {
    const out: Card[] = [];
    executableRegions().forEach((block, index) => {
      const rec = host.anchor.anchorForBlock(view.state, block);
      if (rec === null || rec.result === undefined) return;
      const id = rec.result as unknown as number;
      const envelope = resultById(rec.result);
      if (envelope === undefined) return;
      const drawn = drawCard(id, envelope as unknown as ResultEnvelopeView);
      out.push({
        block: index,
        result: id,
        header: drawn.header,
        body: drawn.body,
        rc: asNumber(asRecord(envelope)["rc"]),
      });
    });
    return out;
  }

  function history(): HistoryRow[] {
    return historyState.entries.map((e) => ({ command: e.command, rc: e.rc }));
  }

  function snapshot(what: Section[]): Snapshot {
    const want = new Set<Section>(what.length === 0 ? (["blocks"] as Section[]) : what);
    return {
      host: HOST_NAME,
      // The editor's own state, never a copy this file keeps: asserting the
      // harness's bytes against the harness's bytes proves nothing.
      doc: present<DocView>({
        path: docPath,
        text: text(),
        caret: view.state.selection.main.head,
        // CodeMirror has no document version of its own; `state/doc.ts` holds
        // the one the IPC contract means, and `openDocument` set it to 1.
        version: opened ? 1 : 0,
      }),
      gutter: want.has("gutter") ? present(gutter()) : present(opened ? gutter() : []),
      cards: present(cards()),
      results: want.has("results") ? present(results()) : present([]),
      panes: want.has("panes") ? present(panes()) : present([]),
      // The editor is the only focusable surface this host mounts — there is no
      // dock, so there is nothing else focus could be on. Reported rather than
      // owed: `createEditor` called `setActiveEditor`, and `hasFocus` is the
      // view's own answer, not an assumption about it.
      focus: present(view.hasFocus ? "editor" : "none"),
      layout: present({
        id: currentLayoutId(),
        inline_results: effectiveInlineResults(layoutSpec().defaults.inlineResults),
      }),
      history: want.has("history") ? present(history()) : present([]),
      blocks: want.has("blocks") || want.has("doc") ? present(opened ? blocks() : []) : present([]),
    };
  }

  function capabilities(): Capability[] {
    // Exactly what this host can answer, and nothing aspirational. Every entry
    // is one the harness will hold it to: a capability advertised here whose
    // command comes back `unknown` is a FAILED step, not a blocked one.
    //
    // `editor`, `gutter` and `cards` joined the list in repair round 3, when W13
    // and W14 landed and the DOM they need became one `jsdom` import away. What
    // is still absent is still absent for a reason: `panes` is W16's and W16 has
    // written nothing, `data_editor` is W18's, and `engine` would mean a real
    // kernel rather than W07's replayed stream.
    return [
      "commands",
      "keymap",
      "layout",
      "settings",
      "results",
      "history",
      "event_injection",
      "editor",
      "gutter",
      "cards",
    ];
  }

  function reset(): void {
    opened = false;
    docPath = null;
    blockIndexOf.clear();
    engineHash.clear();
    resultKey.clear();
    anchorOf.clear();
    cardCache.clear();
    // W13's own reset seams, in the order its `harness.ts` uses them: the anchor
    // id counter is window-global, and a second scenario in the same process
    // that started at id 4 would make every transcript position-dependent.
    host.run.resetRuns();
    host.run.setRunSink(null);
    host.orphans.resetOrphans();
    host.collapse.resetCollapse();
    host.anchor.resetAnchorIds();
    // Unmount the panes but keep their registrations: `paneHost` remounts on the
    // next read, so scenario two starts with the same panes drawing from an
    // empty store rather than with scenario one's rows still on screen.
    host.panes.disposePaneHosts();
    view.dispatch({
      changes: { from: 0, to: view.state.doc.length, insert: "" },
      selection: { anchor: 0 },
      effects: host.anchor.setInlineMode.of("always"),
    });
    resetDocState();
    resetResultState();
    resetHistoryState();
    resetSettings();
    last = { via: "", result: "ran", chord_resolves_to: null, events_applied: 0 };
  }

  return {
    capabilities,
    dispatch,
    settle: () => last,
    snapshot,
    reset,
  };
}

/** Install the bridge where tier 2's `executeScript` can reach it. */
export function installBridge(bridge: E2eBridge): void {
  (globalThis as unknown as Record<string, unknown>)[BRIDGE_GLOBAL] = bridge;
}

/** The resident cap, re-exported so a test can assert against the real number. */
export { RESIDENT_CAP };
