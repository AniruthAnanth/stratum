/**
 * Theme and scale application — 06 §14.5, §17.
 *
 * `resources/tokens.generated.css` already encodes the three-state theme
 * pattern: the light palette is the unconditional `:root`, dark applies under
 * `prefers-color-scheme` unless the root carries `data-theme="light"`, and
 * `data-theme="dark"` wins in both directions. So applying a theme is setting or
 * removing ONE attribute. This module does not know a single colour, and it must
 * not: the generated stylesheet is the only place colour is declared.
 */

import type { ThemeChoice } from "../ipc/hand";

export function applyTheme(
  choice: ThemeChoice,
  root: HTMLElement = document.documentElement,
): void {
  if (choice === "system") root.removeAttribute("data-theme");
  else root.dataset["theme"] = choice;
}

/** The theme actually in effect, for a component that must branch on it. */
export function resolvedTheme(root: HTMLElement = document.documentElement): "light" | "dark" {
  const explicit = root.dataset["theme"];
  if (explicit === "light" || explicit === "dark") return explicit;
  return globalThis.matchMedia?.("(prefers-color-scheme: dark)").matches === true
    ? "dark"
    : "light";
}

/**
 * 06 §14.3, §17: the type scale is a `--fs-root` multiplier so OS text scaling
 * moves the whole UI without breaking column alignment, and the editor's own
 * size is separate because it is user-adjustable 11-18 with the line height
 * locked at 1.54.
 */
export function applyScale(
  uiScale: number,
  codeSizePx: number,
  root: HTMLElement = document.documentElement,
): void {
  root.style.setProperty("--fs-root", String(uiScale));
  root.style.setProperty("--code-size", `${codeSizePx}px`);
}

/** Re-resolves on an OS theme change while the user is on "system". */
export function watchSystemTheme(onChange: (theme: "light" | "dark") => void): () => void {
  const query = globalThis.matchMedia?.("(prefers-color-scheme: dark)");
  if (query === undefined) return () => {};
  const handler = (event: MediaQueryListEvent): void => {
    onChange(event.matches ? "dark" : "light");
  };
  query.addEventListener("change", handler);
  return () => query.removeEventListener("change", handler);
}
