/**
 * Keymap presets and the user overlay — 06 §12.
 *
 * Presets are read-only JSON in `resources/keymaps/`; the user's overlay is
 * `<config>/keymaps/user.json`, fetched through `keymap_load`. A preset is never
 * mutated, so "reset my keymap" is a file delete rather than a merge to undo.
 */

import modernJson from "../../resources/keymaps/modern.json";
import stataJson from "../../resources/keymaps/stata.json";
import vscodeJson from "../../resources/keymaps/vscode.json";
import type { HostPlatform } from "../platform/bridge";
import { bridge } from "../platform/bridge";
import { type KeyBinding, KeyTrie } from "./trie";

export type KeymapPreset = "modern" | "stata" | "vscode";

export const KEYMAP_PRESETS = ["modern", "stata", "vscode"] as const;

interface KeymapFile {
  schema: number;
  id: string;
  name: string;
  basedOn?: string;
  bindings: { command: string; key: string; when?: string; args?: unknown }[];
}

const FILES: Readonly<Record<KeymapPreset, KeymapFile>> = {
  modern: modernJson as KeymapFile,
  stata: stataJson as KeymapFile,
  vscode: vscodeJson as KeymapFile,
};

export function isKeymapPreset(s: string): s is KeymapPreset {
  return (KEYMAP_PRESETS as readonly string[]).includes(s);
}

export function keymapName(preset: KeymapPreset): string {
  return FILES[preset].name;
}

/**
 * A preset's bindings with its `basedOn` chain resolved, base first.
 *
 * Order is the whole mechanism: §12.3 and §12.4 are stated as *deltas*, and the
 * trie's rule is that the last binding on a keystroke wins. Concatenating base
 * then delta therefore gives exactly the documented semantics — a delta on a key
 * the base also binds replaces it, and every base binding the delta is silent
 * about survives. Nothing has to be subtracted, which is why §12.3 can say
 * `Mod+Enter` is "additive, never removed" and mean it.
 */
export function presetBindings(preset: KeymapPreset): KeyBinding[] {
  const chain: KeymapFile[] = [];
  const seen = new Set<string>();
  let cursor: KeymapFile | undefined = FILES[preset];
  while (cursor !== undefined) {
    if (seen.has(cursor.id)) throw new Error(`keymap ${preset}: basedOn cycle at ${cursor.id}`);
    seen.add(cursor.id);
    chain.unshift(cursor);
    const base: string | undefined = cursor.basedOn;
    cursor = base !== undefined && isKeymapPreset(base) ? FILES[base] : undefined;
  }
  return chain.flatMap((file) =>
    file.bindings.map((b): KeyBinding => ({ ...b, source: "preset" })),
  );
}

/** Compiles a trie from a preset plus whatever the host has stored for the user. */
export async function loadKeymap(
  preset: KeymapPreset,
  platform: HostPlatform = bridge().platform(),
): Promise<KeyTrie> {
  let user: KeyBinding[] = [];
  try {
    const loaded = await bridge().invoke<KeyBinding[]>("keymap_load", { preset });
    user = loaded.map((b) => ({ ...b, source: "user" }));
  } catch {
    // No host, or no overlay yet. The preset alone is a complete keymap; a
    // missing overlay must never leave the app with no keyboard at all.
  }
  return new KeyTrie([...presetBindings(preset), ...user], platform);
}

/** Synchronous trie from presets only — boot's first keymap, before any IPC. */
export function presetKeymap(preset: KeymapPreset, platform: HostPlatform): KeyTrie {
  return new KeyTrie(presetBindings(preset), platform);
}
