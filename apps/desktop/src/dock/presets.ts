/**
 * The four shipped layouts — 06 §8.2–§8.4.
 *
 * "Presets ship as read-only JSON in `resources/layouts/`. User edits are
 * written to `<config>/layouts/user/<id>.json`; a preset is never mutated, so
 * **Reset layout** is a file delete."
 *
 * They are imported statically rather than fetched, so the first layout is
 * available in the same tick as the bundle and the 400 ms cold-shell budget
 * (06 §15.1) does not have a round trip in it.
 */

import classicSidebarJson from "../../resources/layouts/classic-sidebar.json";
import classicJson from "../../resources/layouts/classic.json";
import focusJson from "../../resources/layouts/focus.json";
import modernJson from "../../resources/layouts/modern.json";
import type { LayoutSpec } from "../ipc/hand";
import { bridge } from "../platform/bridge";
import { type PresetId, isPresetId, validateLayoutSpec } from "./layoutSpec";

const PRESETS: Readonly<Record<PresetId, unknown>> = {
  modern: modernJson,
  classic: classicJson,
  "classic-sidebar": classicSidebarJson,
  focus: focusJson,
};

/**
 * A preset, deep-copied. The caller owns its copy: the dock writes sizes back
 * into the spec as the user drags a sash, and a preset that could be mutated
 * would stop being a preset after the first drag.
 */
export function preset(id: PresetId): LayoutSpec {
  const result = validateLayoutSpec(structuredClone(PRESETS[id]));
  if (!result.ok) {
    // A shipped preset that does not validate is a build error, not a runtime
    // condition. It is checked in a test, so this can only fire in a tampered
    // bundle — and then failing loudly beats falling back to another preset.
    throw new Error(
      `layout preset ${id} is invalid: ${result.issues.map((i) => `${i.path}: ${i.message}`).join("; ")}`,
    );
  }
  return result.spec;
}

export const DEFAULT_PRESET: PresetId = "modern";

export interface LoadedLayout {
  spec: LayoutSpec;
  /** Non-empty when a stored layout was rejected and a preset was used instead. */
  notice?: string;
}

/**
 * Loads a layout by id: the host's stored copy if it has one, the preset
 * otherwise. A stored layout that fails validation falls back to its `basedOn`
 * preset and reports why — 06 §8.5's "malformed user layout falls back with a
 * status-bar notice".
 */
export async function loadLayout(id: string): Promise<LoadedLayout> {
  let stored: unknown;
  try {
    stored = await bridge().invoke<unknown>("layout_load", { id });
  } catch {
    return { spec: preset(fallbackPreset(id)) };
  }

  const result = validateLayoutSpec(stored);
  if (result.ok) return { spec: result.spec };

  const target = fallbackPreset(id, stored);
  const first = result.issues[0];
  return {
    spec: preset(target),
    notice: `Layout “${id}” is not valid (${first?.path ?? "?"}: ${first?.message ?? "?"}) — using ${target}.`,
  };
}

/** `basedOn` if it names a preset, else the id itself, else Modern. */
function fallbackPreset(id: string, stored?: unknown): PresetId {
  if (typeof stored === "object" && stored !== null) {
    const basedOn = (stored as { basedOn?: unknown }).basedOn;
    if (typeof basedOn === "string" && isPresetId(basedOn)) return basedOn;
  }
  return isPresetId(id) ? id : DEFAULT_PRESET;
}

export type { PresetId };
