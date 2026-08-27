/**
 * Detach and re-dock — 06 §13.3.
 *
 * dockview cannot drag across native windows, so we implement the outward half:
 * a tab dragged past the source window's client rect becomes a `pane.html`
 * window and leaves the source dock.
 *
 * The inward half is deliberately NOT a drag in v1. §13.3 step 3 requires
 * hit-testing a dragged window against "every other window's screen rect", and
 * CONTRACTS §11 declares no command that returns another window's bounds —
 * `window_open_pane` and `window_close` are the whole window surface. §13.3's
 * own open question names this and states the fallback ("accept detach-by-menu
 * only in v1"), so re-docking is `redockPane`, a verb, reachable from the pane
 * menu and the palette. Escalated rather than invented.
 */

import { type WindowBounds, bridge } from "../platform/bridge";
import type { DockAdapter } from "./adapter";
import { type PaneComponentId, paneTitle } from "./panes";

/** Default geometry for a freshly detached pane, before the user moves it. */
const DETACHED = { w: 520, h: 640 };

export interface DetachOptions {
  /** The label prefix the host expects, `${project}`. */
  project: string;
  /** Defaults to `window`. */
  host?: Window;
}

const paneLabel = (project: string, pane: PaneComponentId): string => `${project}:pane:${pane}`;

/**
 * `true` when a drag ended outside the window that started it. Read from screen
 * coordinates rather than client ones: a `dragend` fired over another window
 * reports client coordinates relative to the source window anyway, and only the
 * screen pair is meaningful once the pointer has left it.
 */
export function endedOutside(event: DragEvent, host: Window): boolean {
  const { screenX, screenY } = event;
  // Chromium fires a terminal `dragend` at (0, 0) when a drag is cancelled;
  // treating that as "outside" would detach a pane on every Escape.
  if (screenX === 0 && screenY === 0) return false;
  const left = host.screenX;
  const top = host.screenY;
  return (
    screenX < left ||
    screenY < top ||
    screenX > left + host.outerWidth ||
    screenY > top + host.outerHeight
  );
}

export function detachBounds(event: DragEvent): WindowBounds {
  return {
    x: Math.round(event.screenX - DETACHED.w / 2),
    y: Math.round(event.screenY - 16),
    w: DETACHED.w,
    h: DETACHED.h,
  };
}

/** Moves one pane out of this window into its own. */
export async function detachPane(
  dock: DockAdapter,
  pane: PaneComponentId,
  project: string,
  bounds?: WindowBounds,
): Promise<string> {
  const label = await bridge().openPaneWindow({
    role: "pane",
    paneId: pane,
    label: paneLabel(project, pane),
    bounds,
  });
  // Close only after the host confirms the window: a failed spawn that had
  // already removed the panel would lose the pane entirely.
  dock.close(pane);
  return label;
}

/** Brings a detached pane back. The detached window closes; the dock re-adds it. */
export async function redockPane(
  dock: DockAdapter,
  pane: PaneComponentId,
  project: string,
): Promise<void> {
  await bridge().closeWindow(paneLabel(project, pane));
  dock.focus(pane);
}

/**
 * Wires the drag-out gesture. Returns a disposer.
 *
 * We do not cancel dockview's own drop: by `dragend` its drop target has already
 * declined the event (the pointer is outside every one of them), so there is
 * nothing left to cancel and racing it would be the fragile version.
 */
export function installDetach(dock: DockAdapter, options: DetachOptions): () => void {
  const host = options.host ?? window;
  let dragging: PaneComponentId | undefined;

  const offDrag = dock.onPanelDragStart((panelId) => {
    dragging = panelId as PaneComponentId;
  });

  const onDragEnd = (event: DragEvent): void => {
    const pane = dragging;
    dragging = undefined;
    if (pane === undefined || !endedOutside(event, host)) return;
    void detachPane(dock, pane, options.project, detachBounds(event)).catch(() => {
      // The pane stays docked. A window that failed to open is a host problem;
      // losing the user's Results pane over it would be ours.
    });
  };

  host.addEventListener("dragend", onDragEnd, true);
  return () => {
    offDrag();
    host.removeEventListener("dragend", onDragEnd, true);
  };
}

export { paneTitle };
