/**
 * Session state — 06 §13.2, CONTRACTS §11.
 *
 * "The Rust core is the single source of truth. Each webview is a client."
 * This store holds the session handle, the connection state, and the one piece
 * of protocol logic every other store depends on: **generation gap detection**.
 * Every event carries a generation; a client that sees a gap re-snapshots that
 * pane rather than diverging. Doing that in one place is what keeps five stores
 * from each inventing their own resync.
 */

import { batch, createSignal } from "solid-js";
import type { SessionId } from "../ipc/hand";
import { bridge } from "../platform/bridge";

export type ConnectionState = "idle" | "opening" | "ready" | "degraded" | "closed";

const [session, setSession] = createSignal<SessionId | undefined>(undefined);
const [state, setState] = createSignal<ConnectionState>("idle");
const [epoch, setEpoch] = createSignal(0);
const [engineVersion, setEngineVersion] = createSignal<string | undefined>(undefined);
const [lastError, setLastError] = createSignal<string | undefined>(undefined);

export const currentSession = session;
export const connectionState = state;
export const sessionEpoch = epoch;
export const engineVersionText = engineVersion;
export const sessionError = lastError;

interface SessionOpened {
  session: SessionId;
  epoch: number;
  engineVersion: string;
}

/** Per-pane generation cursor, so a gap in one stream does not resnapshot all six. */
const generations = new Map<string, number>();

export type ResyncHandler = (stream: string) => void;
const resyncHandlers = new Set<ResyncHandler>();

/** A store registers here to be told "your stream skipped; re-snapshot". */
export function onResync(handler: ResyncHandler): () => void {
  resyncHandlers.add(handler);
  return () => {
    resyncHandlers.delete(handler);
  };
}

/**
 * Records a generation for a stream and reports whether it was contiguous.
 *
 * `false` means a gap: the caller has missed at least one event and its local
 * state is now a guess. The rule is re-snapshot, never patch-and-hope, because
 * a store that has silently diverged shows numbers that are wrong rather than
 * absent — and in a statistics product those are not the same failure.
 */
export function acceptGeneration(stream: string, generation: number): boolean {
  const previous = generations.get(stream);
  generations.set(stream, generation);
  if (previous === undefined) return true;
  if (generation === previous + 1 || generation === previous) return true;
  for (const handler of resyncHandlers) handler(stream);
  return false;
}

export function resetGenerations(): void {
  generations.clear();
}

let unsubscribe: (() => void) | undefined;

/** Frame handler installed by `boot`; every store's events arrive through it. */
export type FrameHandler = (bytes: Uint8Array) => void;
let frameHandler: FrameHandler = () => {};

export function setFrameHandler(handler: FrameHandler): void {
  frameHandler = handler;
}

export async function openSession(projectRoot: string, mode = "interactive"): Promise<void> {
  setState("opening");
  try {
    const opened = await bridge().invoke<SessionOpened>("session_open", { projectRoot, mode });
    batch(() => {
      setSession(() => opened.session);
      setEpoch(opened.epoch);
      setEngineVersion(opened.engineVersion);
      setLastError(undefined);
    });
    resetGenerations();
    unsubscribe = await bridge().subscribe(opened.session, (bytes) => frameHandler(bytes));
    setState("ready");
  } catch (error) {
    batch(() => {
      setState("degraded");
      setLastError(error instanceof Error ? error.message : String(error));
    });
  }
}

export async function closeSession(): Promise<void> {
  unsubscribe?.();
  unsubscribe = undefined;
  const id = session();
  if (id !== undefined) {
    await bridge()
      .invoke<void>("session_close", { session: id })
      .catch(() => {});
  }
  batch(() => {
    setSession(undefined);
    setState("closed");
  });
  resetGenerations();
}

/** Test seam. */
export function resetSessionState(): void {
  unsubscribe = undefined;
  frameHandler = () => {};
  resyncHandlers.clear();
  resetGenerations();
  batch(() => {
    setSession(undefined);
    setState("idle");
    setEpoch(0);
    setEngineVersion(undefined);
    setLastError(undefined);
  });
}
