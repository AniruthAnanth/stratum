/**
 * Production wiring — plan W17, the frontend half of "desktop host wiring".
 *
 * `boot()` renders the shell; this module is what makes the shell an
 * application. It does four things, in this order:
 *
 *  1. **Registers every pane surface** the other units shipped export-but-
 *     unwired: W13's editor + sections, W14's results + compare (component-only,
 *     mounted here), W15's repro, W16's history/variables/properties/project/
 *     viewer + command bar, W19's graph deck. W18's data editor needs a live
 *     `SessionId` and registers when the session opens.
 *  2. **Loads the wasm segmenter** (W11a) and attaches it to the live editor —
 *     the editor mounts first and segments a frame later, which is the design
 *     (`boot/segmenter.ts`).
 *  3. **Installs the real IPC sinks**: the editor's run sink and the command
 *     bar's submit/interrupt sinks stop recording and start speaking
 *     CONTRACTS §11 (`exec_submit`, `exec_cancel`).
 *  4. **Performs the boot handshake**: `app_ready` (which returns the
 *     `X-Stratum-Token` for `stratum-asset://` fetches and says whether this is
 *     an e2e-driven run), then either the e2e webview host (ADR-011, dev-only,
 *     lazily imported) or a live `session_open` against the supervised engine.
 *
 * Nothing here is awaited by the caller: 06 §15.1 budgets 400 ms to an
 * interactive shell, and every IPC round trip below happens after first paint.
 */

import { createEffect, createRoot } from "solid-js";
import { render } from "solid-js/web";
import { registerCommandBarPane, setInterruptSink, setSubmitSink } from "../commandbar";
import { registerPane } from "../dock/panes";
import type { RunRequest } from "../editor/blocks/run";
import { setRunSink } from "../editor/blocks/run";
import { registerEditorCommands } from "../editor/commands";
import { activeEditor } from "../editor/commands";
import { attachSegmenter, registerEditorPane } from "../editor/setup";
import type { CodeHash, DatasetStateId, DocumentId } from "../ipc/hand";
import { asResultId, codeHash } from "../ipc/hand";
import { ComparePane } from "../panes/compare";
import { createGraphDeck, registerGraphsPane } from "../panes/graphs";
import { registerHistoryPane } from "../panes/history";
import { registerProjectPane } from "../panes/project";
import { registerPropertiesPane } from "../panes/properties";
import { registerReproPane } from "../panes/repro";
import { ResultsPane } from "../panes/results";
import { registerSectionsPane } from "../panes/sections";
import { registerVariablesPane } from "../panes/variables";
import { registerViewerPane } from "../panes/viewer";
import { bridge, setAssetToken } from "../platform/bridge";
import type { ResultEnvelopeView } from "../renderers/types";
import { openDocument, setActiveDocument } from "../state/doc";
import type { ExecEventView } from "../state/exec";
import { applyExecEvent, applyRunPlan, readout, runPlan, setExecDocument } from "../state/exec";
import type { HasResultId } from "../state/results";
import { recordResult, result as resultById, resultState } from "../state/results";
import { currentSession, openSession, setFrameHandler } from "../state/session";
import { invalidateStats, loadVariables, variables } from "../state/vars";
import { loadSegmenter } from "../wasm/loader";
import type { WindowIdentity } from "./role";

// ---------------------------------------------------------------------------
// MessagePack — the host → webview event frames (CONTRACTS §11: rmp-serde
// `to_vec_named`, one event per channel message, never JSON)
// ---------------------------------------------------------------------------

/**
 * Covers exactly what `rmp_serde::to_vec_named` emits for `EngineEvent` and
 * `HostEvent`: maps with string keys, arrays, strings, both integer families,
 * both float widths, booleans, nil and bin. `u64` is narrowed to `number` only
 * when exactly representable, `bigint` otherwise — the same rule as W14's
 * reference decoder in `renderers/fixtures.ts`, which `wire.test.ts` diffs this
 * one against frame-by-frame over the committed mock stream. (That decoder
 * cannot be imported here: its module top-level reads `node:fs`.)
 */
export type WireValue =
  | null
  | boolean
  | number
  | bigint
  | string
  | Uint8Array
  | WireValue[]
  | { [k: string]: WireValue };

const utf8 = new TextDecoder("utf-8", { fatal: true });

class Reader {
  private pos = 0;
  private readonly view: DataView;

  constructor(private readonly bytes: Uint8Array) {
    this.view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  }

  private u8(): number {
    const b = this.bytes[this.pos];
    if (b === undefined) throw new RangeError("msgpack: truncated");
    this.pos += 1;
    return b;
  }

  private take(n: number): Uint8Array {
    const out = this.bytes.subarray(this.pos, this.pos + n);
    if (out.length !== n) throw new RangeError("msgpack: truncated");
    this.pos += n;
    return out;
  }

  private num(read: (view: DataView, at: number) => number, width: number): number {
    if (this.pos + width > this.bytes.length) throw new RangeError("msgpack: truncated");
    const value = read(this.view, this.pos);
    this.pos += width;
    return value;
  }

  private big(read: (view: DataView, at: number) => bigint): number | bigint {
    if (this.pos + 8 > this.bytes.length) throw new RangeError("msgpack: truncated");
    const value = read(this.view, this.pos);
    this.pos += 8;
    const n = Number(value);
    return BigInt(n) === value && Number.isSafeInteger(n) ? n : value;
  }

  private str(n: number): string {
    return utf8.decode(this.take(n));
  }

  private seq(n: number): WireValue[] {
    const out: WireValue[] = [];
    for (let i = 0; i < n; i += 1) out.push(this.value());
    return out;
  }

  private map(n: number): { [k: string]: WireValue } {
    const out: { [k: string]: WireValue } = {};
    for (let i = 0; i < n; i += 1) {
      const key = this.value();
      if (typeof key !== "string") throw new TypeError("msgpack: non-string map key");
      out[key] = this.value();
    }
    return out;
  }

  value(): WireValue {
    const b = this.u8();
    if (b <= 0x7f) return b;
    if (b >= 0xe0) return b - 0x100;
    if (b >= 0x80 && b <= 0x8f) return this.map(b & 0x0f);
    if (b >= 0x90 && b <= 0x9f) return this.seq(b & 0x0f);
    if (b >= 0xa0 && b <= 0xbf) return this.str(b & 0x1f);
    switch (b) {
      case 0xc0:
        return null;
      case 0xc2:
        return false;
      case 0xc3:
        return true;
      case 0xc4:
        return this.take(this.num((v, a) => v.getUint8(a), 1));
      case 0xc5:
        return this.take(this.num((v, a) => v.getUint16(a), 2));
      case 0xc6:
        return this.take(this.num((v, a) => v.getUint32(a), 4));
      case 0xca:
        return this.num((v, a) => v.getFloat32(a), 4);
      case 0xcb:
        return this.num((v, a) => v.getFloat64(a), 8);
      case 0xcc:
        return this.num((v, a) => v.getUint8(a), 1);
      case 0xcd:
        return this.num((v, a) => v.getUint16(a), 2);
      case 0xce:
        return this.num((v, a) => v.getUint32(a), 4);
      case 0xcf:
        return this.big((v, a) => v.getBigUint64(a));
      case 0xd0:
        return this.num((v, a) => v.getInt8(a), 1);
      case 0xd1:
        return this.num((v, a) => v.getInt16(a), 2);
      case 0xd2:
        return this.num((v, a) => v.getInt32(a), 4);
      case 0xd3:
        return this.big((v, a) => v.getBigInt64(a));
      case 0xd9:
        return this.str(this.num((v, a) => v.getUint8(a), 1));
      case 0xda:
        return this.str(this.num((v, a) => v.getUint16(a), 2));
      case 0xdb:
        return this.str(this.num((v, a) => v.getUint32(a), 4));
      case 0xdc:
        return this.seq(this.num((v, a) => v.getUint16(a), 2));
      case 0xdd:
        return this.seq(this.num((v, a) => v.getUint32(a), 4));
      case 0xde:
        return this.map(this.num((v, a) => v.getUint16(a), 2));
      case 0xdf:
        return this.map(this.num((v, a) => v.getUint32(a), 4));
      default:
        throw new TypeError(`msgpack: unsupported byte 0x${b.toString(16)}`);
    }
  }
}

/** Decode one host → webview frame. Exported for `wire.test.ts`. */
export function decodeFrame(bytes: Uint8Array): WireValue {
  return new Reader(bytes).value();
}

// ---------------------------------------------------------------------------
// Boundary conversions — local copies of the two shims `e2e/bridge.ts`
// documents (`hexOfCodeHash`, `storeShapeOf`). Copied, not imported: the e2e
// module is the dev/browser-tab path and must stay out of the entry chunk.
// ---------------------------------------------------------------------------

function hexHash(bytes: unknown): CodeHash | undefined {
  if (!Array.isArray(bytes) || bytes.length !== 16) return undefined;
  let hex = "";
  for (const b of bytes) {
    if (typeof b !== "number" || !Number.isInteger(b) || b < 0 || b > 255) return undefined;
    hex += b.toString(16).padStart(2, "0");
  }
  return codeHash(hex);
}

const isRecord = (v: unknown): v is Record<string, unknown> =>
  typeof v === "object" && v !== null && !Array.isArray(v);

// ---------------------------------------------------------------------------
// The live event fan-in: decoded frames → the production stores
// ---------------------------------------------------------------------------

/**
 * Apply one decoded host/engine event to the stores that model it. Exported
 * for `wire.test.ts`, which pushes the whole committed mock stream through.
 *
 * `StatusChanged`, `BlockStarted`, `BlockFinished`, `RunStarted`,
 * `RunFinished` and `StateChanged` land in W15's `state/exec.ts` — its own
 * header names them — and `Result` lands in `state/results.ts`. Everything
 * else (Output → W16's log, DataPage refs → W18) is consumed uncounted: this
 * is a router, not a second frontend.
 */
export function applyWireEvent(raw: WireValue): boolean {
  if (!isRecord(raw) || typeof raw["event"] !== "string") return false;
  const ev: Record<string, unknown> = { ...raw };

  // The wire carries `CodeHash(pub [u8; 16])`; the stores are typed on 32-char
  // lowercase hex (CONTRACTS §12). Convert at the boundary, nowhere else.
  const hash = hexHash(ev["code_hash"]);
  if (hash !== undefined) ev["code_hash"] = hash;

  if (ev["event"] === "result" && isRecord(ev["envelope"])) {
    const envelope: Record<string, unknown> = { ...ev["envelope"] };
    const envelopeHash = hexHash(envelope["code_hash"]);
    if (envelopeHash !== undefined) envelope["code_hash"] = envelopeHash;
    const id = envelope["result"];
    if (typeof id !== "number") return false;
    // The store reads `.id`; the contract carries `.result` (the join
    // `e2e/bridge.ts` reported). Ordinal 0 when the wire does not say.
    const shaped = { ...envelope, id: asResultId(id) } as HasResultId;
    const ordinal = typeof envelope["hash_ordinal"] === "number" ? envelope["hash_ordinal"] : 0;
    recordResult(shaped, envelopeHash === undefined ? undefined : { hash: envelopeHash, ordinal });
    return true;
  }

  return applyExecEvent(ev as unknown as ExecEventView);
}

// ---------------------------------------------------------------------------
// Sinks — the editor's and the command bar's IPC boundaries
// ---------------------------------------------------------------------------

/** `RunRequest` → CONTRACTS §11 `exec_submit`. Exported for `wire.test.ts`. */
export function intentOf(request: RunRequest): Record<string, unknown> | undefined {
  const doc = request.doc;
  if (doc === undefined || request.blocks.length === 0) return undefined;
  const first = request.blocks[0];
  const last = request.blocks[request.blocks.length - 1];
  if (first === undefined || last === undefined) return undefined;
  switch (request.verb) {
    case "run.blockAndAdvance":
      return { intent: "run_and_advance", doc, cursor: first.from };
    case "run.block":
    case "run.line":
    case "run.statement":
      return { intent: "current_block", doc, cursor: first.from };
    case "run.section":
      return { intent: "current_section", doc, cursor: first.from };
    case "run.file":
    case "run.fileClean":
    case "run.entryPoint":
      return request.mode === "clean"
        ? { intent: "clean_run", entry: doc, isolation: "in_process" }
        : { intent: "whole_file", doc };
    case "run.allStale":
      return { intent: "all_stale", doc };
    default:
      // Selection-shaped verbs (`run.selection`, `run.above`, `run.below`,
      // `run.fromHere`, `run.toCursor`): one span covering the resolved blocks.
      return { intent: "selection", doc, span: { start: first.from, end: last.to } };
  }
}

function installSinks(): void {
  setRunSink(async (request) => {
    if (request.verb === "run.break") {
      // No `level`: the host walks the C21 ladder (interrupt → abort → kill).
      await bridge().invoke("exec_cancel", { run: runPlan()?.run ?? 0 });
      return;
    }
    const intent = intentOf(request);
    if (intent === undefined) return;
    const plan = await bridge().invoke("exec_submit", { intent, inlineMode: "always" });
    applyRunPlan(plan as never);
  });

  setSubmitSink(async (request) => {
    await bridge().invoke("exec_submit", {
      intent: { intent: "command_bar", text: request.text },
      inlineMode: "always",
    });
    return { rc: 0 };
  });

  setInterruptSink(async (request) => {
    await bridge().invoke("exec_cancel", { run: runPlan()?.run ?? 0, level: request.level });
  });
}

// ---------------------------------------------------------------------------
// OS file opens — the frontend half of the double-click path
// ---------------------------------------------------------------------------

/**
 * The host's file-open event — `file_open::OPEN_PATH_EVENT`, emitted from
 * `RunEvent::Opened` and from argv, with cold-start buffering on that side.
 * The payload is `{ kind, path }`; the spelling is pinned against the Rust
 * constant in `wire.test.ts`, because the two halves were written in parallel
 * against different guesses and a name that only one side knows is an open
 * that silently does nothing.
 *
 * The host submits a double-clicked `.dta` itself and emits this event only
 * for `.do`; {@link routeOpenedFile} still handles both kinds, so a host that
 * ever routes a dataset here opens it rather than dropping it.
 */
export const FILE_OPEN_EVENT = "stratum://open-path";

/**
 * The path an opened-file payload names. `{ path }` is the host's shape; a
 * bare string is accepted too, so the `menu-action` convention routes rather
 * than silently dropping the open.
 */
export function openedPathOf(payload: unknown): string | undefined {
  if (typeof payload === "string" && payload !== "") return payload;
  if (isRecord(payload) && typeof payload["path"] === "string" && payload["path"] !== "") {
    return payload["path"];
  }
  return undefined;
}

/** The slice of `DocumentOpenedReply` (`src-tauri/src/ipc.rs`) this router reads. */
interface DocOpenedReply {
  readonly doc: DocumentId;
  readonly path?: string;
  readonly text: string;
  readonly version: number;
  readonly eol: "lf" | "crlf";
  readonly bom: boolean;
}

/**
 * Route one OS-opened path. Returns false for an extension with no route, so
 * the listener can say so instead of swallowing the open.
 *
 * `.do` goes through `doc_open` — the same reply every other open uses — and
 * lands in the active editor plus the document store, so run verbs and status
 * events attribute to it. `.dta` is submitted as the command-bar `use`: one
 * path for every command the product runs, so the load is echoed, logged and
 * recorded exactly as if the user had typed it.
 */
export async function routeOpenedFile(path: string): Promise<boolean> {
  if (/\.do$/i.test(path)) {
    const reply = await bridge().invoke<DocOpenedReply>("doc_open", { path });
    const view = activeEditor();
    view?.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: reply.text } });
    openDocument({
      doc: reply.doc,
      path: reply.path ?? path,
      version: reply.version,
      eol: reply.eol,
      bom: reply.bom,
      ownerLabel: bridge().label(),
      dirty: false,
    });
    setActiveDocument(reply.doc);
    setExecDocument(reply.doc);
    return true;
  }
  if (/\.dta$/i.test(path)) {
    await bridge().invoke("exec_submit", {
      intent: { intent: "command_bar", text: `use "${path}"` },
      inlineMode: "always",
    });
    return true;
  }
  return false;
}

// ---------------------------------------------------------------------------
// Panes
// ---------------------------------------------------------------------------

function residentEnvelopes(): readonly ResultEnvelopeView[] {
  return resultState.order
    .map((id) => resultById(id))
    .filter((e): e is HasResultId => e !== undefined) as unknown as ResultEnvelopeView[];
}

function registerPanes(): void {
  // W13. The editor mounts unsegmented; the segmenter attaches below.
  registerEditorPane("", {});
  registerSectionsPane();
  // W14 ships components, not registrations (its escalation); mounted here with
  // the live stores behind getters so Solid re-renders on store changes.
  registerPane("results", (host, register) => {
    register(render(() => <ResultsPane envelopes={residentEnvelopes()} />, host));
  });
  registerPane("compare", (host, register) => {
    register(render(() => <ComparePane models={[]} />, host));
  });
  // W15, W16.
  registerReproPane();
  registerHistoryPane();
  registerVariablesPane();
  registerPropertiesPane();
  registerProjectPane();
  registerViewerPane();
  registerCommandBarPane({});
  // W19.
  registerGraphsPane(createGraphDeck());
  // W18's data editor needs a live SessionId; it registers in `handshake()`
  // once `session_open` has answered. W21's assistant has no register surface.
}

async function registerDataEditor(): Promise<void> {
  const session = currentSession();
  if (session === undefined) return;
  const { registerDataEditorPane } = await import("../panes/dataeditor");
  // Live props, not a snapshot: getters over the stores every `state_changed`
  // and `variables_list` lands in, so the grid tracks the dataset as it
  // advances. Solid's JSX spread preserves getters, which is what makes this
  // reactive through `registerDataEditorPane`. `variables.rows` (W16's cheap
  // tier) is `VariableLike`-shaped already — `grid/engine.ts` reads `storage`
  // and `valueLabel` by those spellings.
  registerDataEditorPane({
    session,
    get state(): DatasetStateId | undefined {
      return readout().dataset;
    },
    get obs(): number | undefined {
      return readout().obs;
    },
    get variables() {
      return variables.rows;
    },
  });
}

/**
 * The dataset-advance fan-out: a `state_changed` off the wire lands in
 * `state/exec`'s readout; the variables list and the per-state stats cache then
 * describe a frame that no longer exists. Refetch the one, drop the other.
 * The grid itself invalidates through its reactive `state` prop — this is the
 * only *fetch* keyed on the signal. Returns the root's disposer.
 */
function watchDatasetState(): () => void {
  return createRoot((dispose) => {
    let known: DatasetStateId | undefined;
    createEffect(() => {
      const dataset = readout().dataset;
      if (dataset === undefined || dataset === known) return;
      // Not yet subscribed: leave `known` unset so the session signal firing
      // re-runs this and the fetch happens then.
      if (currentSession() === undefined) return;
      known = dataset;
      invalidateStats();
      void loadVariables(variables.frame).catch(() => {
        // An engine may honestly refuse (`variables_list` answers "cannot yet"
        // on backends without it); the pane keeps drawing its chrome.
      });
    });
    return dispose;
  });
}

// ---------------------------------------------------------------------------
// The handshake
// ---------------------------------------------------------------------------

interface ReadyReply {
  assetToken: string;
  e2e?: boolean;
}

/**
 * The e2e request event — spelled here as a LITERAL, not imported from
 * `e2e/webview.ts`, so listening for it does not pull the e2e module into the
 * entry chunk. `wire.test.ts` pins all three spellings (this one, the module's
 * export, and `REQUEST_EVENT` in `src-tauri/src/e2e_cmds.rs`) to one string.
 */
const E2E_REQUEST_EVENT = "stratum://e2e-request";

async function handshake(): Promise<void> {
  const host = bridge();

  // The listener BEFORE `app_ready`, because `app_ready` is the host's cue to
  // dial the harness back (ADR-011), and the harness's first question is
  // forwarded to this window the moment it connects. A listener registered
  // after the reply loses that race on every boot. In an ordinary run the
  // event never fires and the closure below never imports anything.
  await host.listen<unknown>(E2E_REQUEST_EVENT, (request) => {
    void import("../e2e/webview").then((m) => m.answerE2eRequest(request));
  });

  // Same reasoning, same ordering: `app_ready` is also the host's cue to flush
  // any file opens it buffered during cold start (a double-clicked `.do`/`.dta`
  // arrives before the webview exists), so this listener must be installed
  // before the reply too. `e2eDriven` gates routing rather than registration:
  // in a harness-driven window the fed events must stay the stores' only writer.
  let e2eDriven = false;
  await host.listen<unknown>(FILE_OPEN_EVENT, (payload) => {
    if (e2eDriven) return;
    const path = openedPathOf(payload);
    if (path === undefined) return;
    void routeOpenedFile(path)
      .then((routed) => {
        if (!routed) console.warn(`stratum: no route for opened file ${path}`);
      })
      .catch((error: unknown) => {
        console.error("stratum: opening a file failed", error);
      });
  });

  const ready = await host.invoke<ReadyReply>("app_ready");
  setAssetToken(ready.assetToken);

  if (ready.e2e === true) {
    // A harness is driving this window. The live sinks stay recording and no
    // session is opened: the scenario's fed events must be the only writer of
    // the stores the e2e bridge snapshots.
    e2eDriven = true;
    return;
  }

  installSinks();
  setFrameHandler((bytes) => {
    try {
      applyWireEvent(decodeFrame(bytes));
    } catch {
      // A frame this build cannot decode is dropped, not fatal: the stores
      // resync through the generation-gap machinery (`state/session.ts`).
    }
  });
  // "." — the host resolves the project root; the label's `project` prefix is
  // a display name, not a path.
  await openSession(".");
  await registerDataEditor();
}

// ---------------------------------------------------------------------------
// Entry
// ---------------------------------------------------------------------------

/**
 * Wire the application. Called by `boot()` after the shell renders; returns a
 * disposer (registrations and sinks are process-wide, so the disposer exists
 * for tests rather than for windows, which die with their process).
 */
export function wireApp(identity: WindowIdentity): () => void {
  const disposeEditorCommands = registerEditorCommands();
  registerPanes();
  const disposeDatasetWatch = watchDatasetState();

  // The segmenter: loaded once, attached to the live editor. (W12's
  // `boot/segmenter.ts` slot is deliberately not fed: its reduced interface
  // has no consumer, and `attachSegmenter` is the wiring W13 actually reads.)
  // Failure leaves an unsegmented editor and says why — a document with no
  // block outline beats a blank window.
  void loadSegmenter()
    .then((seg) => {
      const view = activeEditor();
      if (view !== null) attachSegmenter(view, seg);
    })
    .catch((error: unknown) => {
      console.error("stratum: segmenter failed to load", error);
    });

  // IPC after first paint, never awaited by the caller (06 §15.1). Pane-role
  // windows skip it: the main window owns the session (spec §13.2).
  if (identity.role !== "pane" && bridge().isHosted) {
    void handshake().catch((error: unknown) => {
      console.error("stratum: boot handshake failed", error);
    });
  }

  return () => {
    disposeEditorCommands();
    disposeDatasetWatch();
    setRunSink(null);
    setSubmitSink(null);
    setInterruptSink(null);
    setFrameHandler(() => {});
  };
}
