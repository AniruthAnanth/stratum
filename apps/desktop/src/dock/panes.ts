/**
 * Pane hosts — the mechanism behind "editor text, undo history, scroll and
 * cards live outside the dock and are re-parented" (IMPLEMENTATION_PLAN W12).
 *
 * A pane's content element is created ONCE per window and cached here forever.
 * dockview's `IContentRenderer` hands back that same node every time it builds a
 * panel, so `dock.fromJSON(...)` — which disposes every panel and constructs new
 * ones — moves the node instead of rebuilding it. A `CodeMirror` `EditorView`,
 * its `EditorState`, its undo history and its block widgets are properties of
 * that subtree, so they survive a preset switch by construction rather than by
 * being saved and replayed.
 *
 * The one thing re-parenting does NOT preserve is scroll offset: removing an
 * element from the document resets `scrollTop` on every engine we ship on. So
 * the renderer captures scroll on dispose and restores it on init, and that is
 * the whole of the special-casing.
 */

import type { PaneId } from "../ipc/hand";
import { PANE_IDS } from "../ipc/hand";

/**
 * The dock's component ids. `commandbar` is the one that is not a `PaneId`:
 * CONTRACTS §12 does not list it, because the command bar is a `defaults`
 * setting present in every layout rather than a pane you can close — but
 * Classic docks it as a real pane (06 §8.3), so the dock needs a name for it.
 */
export type PaneComponentId = PaneId | "commandbar";

export const PANE_COMPONENT_IDS = [...PANE_IDS, "commandbar"] as const;

export function isPaneComponentId(s: string): s is PaneComponentId {
  return (PANE_COMPONENT_IDS as readonly string[]).includes(s);
}

/**
 * Mounts a pane's content into its host.
 *
 * Disposal is registered rather than returned. `(host) => (() => void) | void`
 * would be the shorter signature and it is the one every effect API reaches for,
 * but a union with `void` in it means the caller cannot tell "returned nothing"
 * from "returned undefined on purpose" — and a pane that forgot to return its
 * disposer would leak silently. An explicit `register` cannot be forgotten by
 * accident, only omitted on purpose.
 */
export type PaneMount = (host: HTMLElement, register: (dispose: () => void) => void) => void;

interface Registration {
  title: string;
  mount: PaneMount;
}

interface HostRecord {
  element: HTMLElement;
  unmount: (() => void) | undefined;
  mounted: boolean;
  scroll: ScrollState | undefined;
}

const DEFAULT_TITLES: Readonly<Record<PaneComponentId, string>> = {
  editor: "Editor",
  results: "Results",
  history: "History",
  variables: "Variables",
  properties: "Properties",
  project: "Project",
  assistant: "Assistant",
  graphs: "Graphs",
  compare: "Compare",
  dataeditor: "Data",
  sections: "Sections",
  viewer: "Viewer",
  repro: "Repro",
  commandbar: "Command",
};

const registry = new Map<PaneComponentId, Registration>();
const hosts = new Map<PaneComponentId, HostRecord>();

/**
 * W13–W21 call this from their own modules. Registration may arrive after the
 * dock has already built the panel — panes are code-split — so a late
 * registration mounts into the host that is already on screen rather than
 * waiting for the next layout change.
 */
export function registerPane(id: PaneComponentId, mount: PaneMount, title?: string): () => void {
  const registration: Registration = { mount, title: title ?? DEFAULT_TITLES[id] };
  registry.set(id, registration);

  const existing = hosts.get(id);
  if (existing !== undefined && !existing.mounted) mount_(existing, registration);

  return () => {
    if (registry.get(id) !== registration) return;
    registry.delete(id);
    const host = hosts.get(id);
    if (host !== undefined) unmount_(host);
  };
}

export function paneTitle(id: PaneComponentId): string {
  return registry.get(id)?.title ?? DEFAULT_TITLES[id];
}

/** The persistent element for a pane. Same node for the life of the window. */
export function paneHost(id: PaneComponentId): HTMLElement {
  const existing = hosts.get(id);
  if (existing !== undefined) return existing.element;

  const element = document.createElement("div");
  element.className = "pane-host";
  element.dataset["pane"] = id;
  const record: HostRecord = { element, unmount: undefined, mounted: false, scroll: undefined };
  hosts.set(id, record);

  const registration = registry.get(id);
  if (registration !== undefined) mount_(record, registration);
  else {
    // A pane whose unit has not landed yet is an empty host, not a crash. Every
    // wave after this one develops against a shell where most panes are absent.
    element.setAttribute("data-unregistered", "");
  }
  return element;
}

function mount_(record: HostRecord, registration: Registration): void {
  record.element.removeAttribute("data-unregistered");
  registration.mount(record.element, (dispose) => {
    record.unmount = dispose;
  });
  record.mounted = true;
}

function unmount_(record: HostRecord): void {
  record.unmount?.();
  record.unmount = undefined;
  record.mounted = false;
  record.element.replaceChildren();
  record.element.setAttribute("data-unregistered", "");
}

export function hasPaneHost(id: PaneComponentId): boolean {
  return hosts.has(id);
}

/** Window teardown, and the test seam. */
export function disposePaneHosts(): void {
  for (const record of hosts.values()) unmount_(record);
  hosts.clear();
}

/** Test seam: drops registrations without touching live hosts. */
export function clearPaneRegistry(): void {
  registry.clear();
}

// ---------------------------------------------------------------------------
// Scroll preservation across a re-parent
// ---------------------------------------------------------------------------

export type ScrollState = ReadonlyArray<readonly [Element, number, number]>;

/**
 * Records the scroll offset of every scrolled descendant, keyed by the element
 * itself. Element identity is safe to key on precisely because nothing here
 * recreates DOM — if a pane rebuilt its subtree, its scroll offset would be
 * meaningless anyway.
 */
export function captureScroll(root: HTMLElement): ScrollState {
  const out: (readonly [Element, number, number])[] = [];
  const visit = (el: Element): void => {
    if (el.scrollTop !== 0 || el.scrollLeft !== 0) {
      out.push([el, el.scrollTop, el.scrollLeft] as const);
    }
    for (const child of el.children) visit(child);
  };
  visit(root);
  return out;
}

export function restoreScroll(state: ScrollState): void {
  for (const [el, top, left] of state) {
    el.scrollTop = top;
    el.scrollLeft = left;
  }
}

/** Called by the dock adapter's renderer, so the pane never has to know. */
export function stashScroll(id: PaneComponentId): void {
  const record = hosts.get(id);
  if (record === undefined) return;
  record.scroll = captureScroll(record.element);
}

export function popScroll(id: PaneComponentId): void {
  const record = hosts.get(id);
  if (record?.scroll === undefined) return;
  restoreScroll(record.scroll);
  record.scroll = undefined;
}
