/**
 * User settings — 06 §14.3 (type scale), §14.4 (density), §12 (keymap preset),
 * §8.1 (theme and inline-results defaults).
 *
 * A layout carries `defaults`; a setting overrides them. The distinction
 * matters: switching from Modern to Focus should change the inline-results mode
 * because Focus's default is `always`, but it must NOT undo a user who has
 * explicitly chosen `compact`. So every field here is `undefined` until the user
 * touches it, and `effective*` folds the layout default in underneath.
 */

import { batch, createSignal } from "solid-js";
import type { InlineResultsMode, ThemeChoice } from "../ipc/hand";
import type { KeymapPreset } from "../keys/presets";
import { bridge } from "../platform/bridge";

export type Density = "dense" | "comfortable";
export type MonoFace = "IBM Plex Mono" | "Iosevka Term" | "JetBrains Mono";

/** 06 §14.3: the editor size is user-adjustable 11–18, line height locked at 1.54. */
export const CODE_SIZE_MIN = 11;
export const CODE_SIZE_MAX = 18;
export const CODE_LINE_HEIGHT = 1.54;

export interface Settings {
  theme?: ThemeChoice;
  keymap: KeymapPreset;
  inlineResults?: InlineResultsMode;
  density: Density;
  monoFace: MonoFace;
  codeSizePx: number;
  /** 06 §15.2: wrapping off is the default, as in Stata, and it is the fast path. */
  wrapLog: boolean;
  /** 06 §14.3: `--fs-root` multiplier, so OS text scaling does not break columns. */
  uiScale: number;
}

const DEFAULTS: Settings = {
  keymap: "modern",
  density: "dense",
  monoFace: "IBM Plex Mono",
  codeSizePx: 13,
  wrapLog: false,
  uiScale: 1,
};

const [settings, setSettings] = createSignal<Settings>(DEFAULTS);

export const userSettings = settings;

const clampCodeSize = (px: number): number =>
  Math.min(CODE_SIZE_MAX, Math.max(CODE_SIZE_MIN, Math.round(px)));

export function updateSettings(patch: Partial<Settings>): void {
  setSettings((prev) => {
    const next = { ...prev, ...patch };
    if (patch.codeSizePx !== undefined) next.codeSizePx = clampCodeSize(patch.codeSizePx);
    return next;
  });
  schedulePersist();
}

/** The layout's default is the floor; an explicit user choice wins. */
export function effectiveTheme(layoutDefault: ThemeChoice | undefined): ThemeChoice {
  return settings().theme ?? layoutDefault ?? "system";
}

export function effectiveInlineResults(layoutDefault: InlineResultsMode): InlineResultsMode {
  return settings().inlineResults ?? layoutDefault;
}

/** 06 §12.2 `Mod+Alt+I` cycles the four modes in the order §8.1 lists them. */
const INLINE_CYCLE: readonly InlineResultsMode[] = ["always", "editor-run", "compact", "off"];

export function cycleInlineResults(layoutDefault: InlineResultsMode): InlineResultsMode {
  const current = effectiveInlineResults(layoutDefault);
  const at = INLINE_CYCLE.indexOf(current);
  const next = INLINE_CYCLE[(at + 1) % INLINE_CYCLE.length] ?? "always";
  updateSettings({ inlineResults: next });
  return next;
}

// ---------------------------------------------------------------------------
// Persistence
// ---------------------------------------------------------------------------

let timer: ReturnType<typeof setTimeout> | undefined;

function schedulePersist(): void {
  if (timer !== undefined) clearTimeout(timer);
  timer = setTimeout(() => {
    timer = undefined;
    void bridge()
      .invoke<void>("workspace_save", { state: { settings: settings() } })
      .catch(() => {
        // No host: settings are per-window and die with it. Correct in a browser
        // tab, and the alternative — a localStorage shadow copy the host would
        // later disagree with — is worse than forgetting.
      });
  }, 500);
}

export function hydrateSettings(stored: Partial<Settings> | undefined): void {
  if (stored === undefined) return;
  batch(() => {
    setSettings((prev) => ({
      ...prev,
      ...stored,
      codeSizePx: clampCodeSize(stored.codeSizePx ?? prev.codeSizePx),
    }));
  });
}

/** Test seam. */
export function resetSettings(): void {
  if (timer !== undefined) clearTimeout(timer);
  timer = undefined;
  setSettings(DEFAULTS);
}
