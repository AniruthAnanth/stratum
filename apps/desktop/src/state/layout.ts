/**
 * Layout state — 06 §8, and the ≤120 ms preset-switch budget (06 §15.1).
 *
 * The switch is synchronous on purpose. Presets are bundled JSON, the dock's
 * `fromJSON` is synchronous, and the pane hosts are re-parented rather than
 * rebuilt — so there is no await anywhere on this path and the elapsed time is
 * dominated by dockview's grid arithmetic. Anything that needs IPC (persisting
 * the new layout, loading a user layout) happens after the frame, never before.
 */

import { batch, createSignal } from "solid-js";
import type { DockAdapter } from "../dock/adapter";
import {
  paneForIndex as paneForIndexOf,
  paneOrder as paneOrderOf,
  validateLayoutSpec,
} from "../dock/layoutSpec";
import type { PaneComponentId } from "../dock/panes";
import { DEFAULT_PRESET, type PresetId, loadLayout, preset } from "../dock/presets";
import type { LayoutSpec } from "../ipc/hand";
import { bridge } from "../platform/bridge";

/** 06 §15.1. Exceeding it is a defect, so it is a number in the code, not a doc. */
export const SWITCH_BUDGET_MS = 120;

const [spec, setSpec] = createSignal<LayoutSpec>(preset(DEFAULT_PRESET));
const [notice, setNotice] = createSignal<string | undefined>(undefined);
const [lastSwitchMs, setLastSwitchMs] = createSignal(0);

let dock: DockAdapter | undefined;
let saveTimer: ReturnType<typeof setTimeout> | undefined;
let offChange: (() => void) | undefined;

export const layoutSpec = spec;
export const layoutNotice = notice;
/** The measured duration of the most recent switch, in ms. The status bar reads it in dev. */
export const layoutSwitchMs = lastSwitchMs;

export function currentLayoutId(): string {
  return spec().id;
}

/**
 * Binds the store to a dock. Called once per window, by the shell.
 * Re-binding disposes the previous subscription so a hot reload cannot leave two
 * layouts writing to one file.
 */
export function attachDock(next: DockAdapter): void {
  offChange?.();
  dock = next;
  dock.apply(spec());
  offChange = dock.onChange(scheduleSave);
}

export function detachDock(): void {
  offChange?.();
  offChange = undefined;
  dock = undefined;
  if (saveTimer !== undefined) clearTimeout(saveTimer);
  saveTimer = undefined;
}

/**
 * Applies a spec to the dock and the store. Synchronous, and measured.
 *
 * Returns the elapsed milliseconds so a test can assert the budget against the
 * real path rather than against a re-implementation of it.
 */
export function applyLayout(next: LayoutSpec): number {
  const started = performance.now();
  batch(() => {
    setSpec(next);
    setNotice(undefined);
  });
  dock?.apply(next);
  const elapsed = performance.now() - started;
  setLastSwitchMs(elapsed);
  return elapsed;
}

/** `Mod+Alt+1/2/3`, the palette, and the `layout.apply` command. */
export function applyPreset(id: PresetId): number {
  return applyLayout(preset(id));
}

/** Boot, and "Open layout…": consults the host, falls back with a notice. */
export async function loadAndApply(id: string): Promise<void> {
  const loaded = await loadLayout(id);
  applyLayout(loaded.spec);
  if (loaded.notice !== undefined) setNotice(loaded.notice);
}

/** `Save layout as…` writes a new user layout; a preset is never mutated. */
export async function saveLayoutAs(name: string, uuid: string): Promise<void> {
  const current = liveSpec();
  const next: LayoutSpec = {
    ...current,
    id: `user:${uuid}`,
    name,
    basedOn: current.basedOn ?? current.id,
  };
  await bridge().invoke<void>("layout_save", { spec: next });
  applyLayout(next);
}

/** **Reset layout** is a file delete, because a preset was never written to. */
export async function resetLayout(): Promise<void> {
  const id = spec().id;
  await bridge().invoke<void>("layout_reset", { id });
  const base = spec().basedOn;
  applyPreset(isPreset(base) ? base : isPreset(id) ? id : DEFAULT_PRESET);
}

const isPreset = (v: string | undefined): v is PresetId =>
  v === "modern" || v === "classic" || v === "classic-sidebar" || v === "focus";

/** The spec with the dock's live geometry folded back in. */
export function liveSpec(): LayoutSpec {
  const base = spec();
  if (dock === undefined) return base;
  const [main, ...rest] = base.windows;
  if (main === undefined) return base;
  return { ...base, windows: [{ ...main, dock: dock.toJSON() }, ...rest] };
}

/** Debounced 500 ms, as 06 §13.3 specifies for every geometry change. */
function scheduleSave(): void {
  if (saveTimer !== undefined) clearTimeout(saveTimer);
  saveTimer = setTimeout(() => {
    saveTimer = undefined;
    const next = liveSpec();
    setSpec(next);
    // A preset is read-only, so dragging a sash in Modern produces a user layout
    // rather than a modified Modern. The host decides the id; until then the
    // save is a no-op against the detached bridge and the geometry lives only in
    // this window, which is the correct behaviour with no host.
    void bridge()
      .invoke<void>("layout_save", { spec: next })
      .catch(() => {});
  }, 500);
}

// ---------------------------------------------------------------------------
// Pane verbs
// ---------------------------------------------------------------------------

export function paneOrder(): PaneComponentId[] {
  return paneOrderOf(spec());
}

export function paneForIndex(index: number): PaneComponentId | undefined {
  return paneForIndexOf(spec(), index);
}

export function togglePane(pane: PaneComponentId): void {
  dock?.toggle(pane);
}

export function focusPane(pane: PaneComponentId): void {
  dock?.focus(pane);
}

/** Test seam: reset the module between cases. */
export function resetLayoutState(): void {
  detachDock();
  batch(() => {
    setSpec(preset(DEFAULT_PRESET));
    setNotice(undefined);
    setLastSwitchMs(0);
  });
}

export { validateLayoutSpec };
export type { PresetId };
