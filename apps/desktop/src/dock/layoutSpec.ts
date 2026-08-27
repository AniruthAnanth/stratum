/**
 * `LayoutSpec` validation — 06 §8.1, CONTRACTS §12.
 *
 * "A malformed user layout falls back to its `basedOn` preset with a status-bar
 * notice" (06 §8.5). That sentence is the reason this file exists and the reason
 * it collects issues instead of throwing on the first one: a user is expected to
 * hand-edit this JSON, and the useful answer to a broken file is the whole list
 * of what is wrong plus a working app, not the first error plus a blank window.
 */

import type { LayoutSpec, PaneId, WindowSpec } from "../ipc/hand";
import { isPaneId } from "../ipc/hand";
import { type PaneComponentId, isPaneComponentId } from "./panes";

export type PresetId = "modern" | "classic" | "classic-sidebar" | "focus";

export const PRESET_IDS = ["modern", "classic", "classic-sidebar", "focus"] as const;

export function isPresetId(s: string): s is PresetId {
  return (PRESET_IDS as readonly string[]).includes(s);
}

export interface LayoutIssue {
  path: string;
  message: string;
}

export type LayoutValidation =
  | { ok: true; spec: LayoutSpec; warnings: LayoutIssue[] }
  | { ok: false; issues: LayoutIssue[] };

const TOP_BARS = ["full", "compact", "auto-hide"];
const INLINE = ["always", "editor-run", "compact", "off"];
const COMMAND_BARS = ["docked-bottom", "overlay", "pane"];
const THEMES = ["light", "dark", "system"];
const ROLES = ["main", "editor", "data", "graph", "pane", "viewer", "prefs"];

const isRecord = (v: unknown): v is Record<string, unknown> =>
  typeof v === "object" && v !== null && !Array.isArray(v);

export function validateLayoutSpec(value: unknown): LayoutValidation {
  const issues: LayoutIssue[] = [];
  const warnings: LayoutIssue[] = [];
  const bad = (path: string, message: string): void => {
    issues.push({ path, message });
  };

  if (!isRecord(value)) return { ok: false, issues: [{ path: "", message: "not an object" }] };

  if (value["schema"] !== 3) bad("schema", `expected 3, got ${JSON.stringify(value["schema"])}`);

  const id = value["id"];
  if (typeof id !== "string" || !(isPresetId(id) || id.startsWith("user:"))) {
    bad("id", "must be a preset id or `user:<uuid>`");
  }
  if (typeof value["name"] !== "string") bad("name", "must be a string");

  const basedOn = value["basedOn"];
  if (basedOn !== undefined && typeof basedOn !== "string") bad("basedOn", "must be a string");

  const chrome = value["chrome"];
  if (!isRecord(chrome)) bad("chrome", "missing");
  else {
    if (typeof chrome["topBar"] !== "string" || !TOP_BARS.includes(chrome["topBar"])) {
      bad("chrome.topBar", `one of ${TOP_BARS.join(", ")}`);
    }
    if (typeof chrome["statusBar"] !== "boolean") bad("chrome.statusBar", "must be a boolean");
  }

  const defaults = value["defaults"];
  if (!isRecord(defaults)) bad("defaults", "missing");
  else {
    const inline = defaults["inlineResults"];
    if (typeof inline !== "string" || !INLINE.includes(inline)) {
      bad("defaults.inlineResults", `one of ${INLINE.join(", ")}`);
    }
    if (typeof defaults["docView"] !== "boolean") bad("defaults.docView", "must be a boolean");
    const cb = defaults["commandBar"];
    if (typeof cb !== "string" || !COMMAND_BARS.includes(cb)) {
      bad("defaults.commandBar", `one of ${COMMAND_BARS.join(", ")}`);
    }
    const theme = defaults["theme"];
    if (theme !== undefined && (typeof theme !== "string" || !THEMES.includes(theme))) {
      bad("defaults.theme", `one of ${THEMES.join(", ")}`);
    }
  }

  const windows = value["windows"];
  if (!Array.isArray(windows) || windows.length === 0) {
    bad("windows", "must be a non-empty array; [0] is the main window");
  } else {
    windows.forEach((w, i) => {
      if (!isRecord(w)) {
        bad(`windows[${i}]`, "not an object");
        return;
      }
      const role = w["role"];
      if (typeof role !== "string" || !ROLES.includes(role)) {
        bad(`windows[${i}].role`, `one of ${ROLES.join(", ")}`);
      }
      if (i === 0 && role !== "main") bad("windows[0].role", "must be `main`");
      if (typeof w["label"] !== "string") bad(`windows[${i}].label`, "must be a string");
      if (w["dock"] === undefined) bad(`windows[${i}].dock`, "missing");
    });
  }

  const panes = value["panes"];
  if (panes !== undefined) {
    if (!isRecord(panes)) bad("panes", "must be an object");
    else {
      for (const key of Object.keys(panes)) {
        if (!isPaneId(key))
          warnings.push({ path: `panes.${key}`, message: "not a PaneId; ignored" });
      }
    }
  }

  if (issues.length > 0) return { ok: false, issues };

  // A pane docked twice would make two panels claim one persistent host element,
  // and the second `appendChild` would silently steal it from the first. That is
  // a corrupt layout rather than a cosmetic one, so it is fatal.
  const spec = value as unknown as LayoutSpec;
  for (const [i, w] of spec.windows.entries()) {
    const seen = new Set<string>();
    for (const component of dockComponents(w.dock)) {
      if (seen.has(component)) {
        return {
          ok: false,
          issues: [{ path: `windows[${i}].dock`, message: `pane ${component} appears twice` }],
        };
      }
      seen.add(component);
      if (!isPaneComponentId(component)) {
        warnings.push({ path: `windows[${i}].dock`, message: `unknown pane ${component}` });
      }
    }
  }

  return { ok: true, spec, warnings };
}

/**
 * The component ids docked in one window. `dock` is dockview's own blob and is
 * opaque by contract, so this reads only the two fields dockview's serializer is
 * documented to write and treats anything else as absent.
 */
export function dockComponents(dock: unknown): string[] {
  if (!isRecord(dock)) return [];
  const panels = dock["panels"];
  if (!isRecord(panels)) return [];
  const out: string[] = [];
  for (const panel of Object.values(panels)) {
    if (!isRecord(panel)) continue;
    const component = panel["contentComponent"];
    if (typeof component === "string") out.push(component);
  }
  return out;
}

/**
 * Pane order for `Mod+1..9` (06 §8.5, §12.2). Grid order, not the order the
 * panels happen to appear in the `panels` map: the user counts panes left to
 * right on screen, so `Mod+1` must be the leftmost one.
 */
export function paneOrder(spec: LayoutSpec): PaneComponentId[] {
  const main: WindowSpec | undefined = spec.windows[0];
  if (main === undefined) return [];
  const out: PaneComponentId[] = [];
  walkGrid(main.dock, (views) => {
    for (const view of views) {
      const component = panelComponent(main.dock, view);
      if (component !== undefined && isPaneComponentId(component) && !out.includes(component)) {
        out.push(component);
      }
    }
  });
  return out;
}

function panelComponent(dock: unknown, panelId: string): string | undefined {
  if (!isRecord(dock)) return undefined;
  const panels = dock["panels"];
  if (!isRecord(panels)) return undefined;
  const panel = panels[panelId];
  if (!isRecord(panel)) return undefined;
  const component = panel["contentComponent"];
  return typeof component === "string" ? component : panelId;
}

function walkGrid(dock: unknown, visit: (views: string[]) => void): void {
  if (!isRecord(dock)) return;
  const grid = dock["grid"];
  if (!isRecord(grid)) return;
  const step = (node: unknown): void => {
    if (!isRecord(node)) return;
    if (node["type"] === "branch") {
      const children = node["data"];
      if (Array.isArray(children)) for (const child of children) step(child);
      return;
    }
    const data = node["data"];
    if (!isRecord(data)) return;
    const views = data["views"];
    if (Array.isArray(views)) visit(views.filter((v): v is string => typeof v === "string"));
  };
  step(grid["root"]);
}

/** The panes a spec docks in its main window, for `pane.toggle` and the palette. */
export function panesIn(spec: LayoutSpec): Set<PaneComponentId> {
  return new Set(paneOrder(spec));
}

/** `Mod+N` → pane, 1-based, capped at 9. */
export function paneForIndex(spec: LayoutSpec, index: number): PaneComponentId | undefined {
  return paneOrder(spec)[index - 1];
}

export type { PaneId };
