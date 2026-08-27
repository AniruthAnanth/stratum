/**
 * The entry point for both windows — 06 §13.3.
 *
 * `index.html` and `pane.html` load this same module; the role in the query
 * string decides which shell renders. Order matters here and is deliberate:
 *
 *  1. shims, so nothing below has to feature-detect;
 *  2. styles, so the first paint is the app's ground and not white;
 *  3. theme, before the first frame, so there is no light flash on a dark desktop;
 *  4. the keyboard authority, before any pane exists, so the very first
 *     keystroke resolves;
 *  5. the shell.
 *
 * IPC comes last and is not awaited. 06 §15.1 budgets 400 ms from cold window to
 * an interactive shell, and a round trip inside that window would spend it on
 * something the user cannot see.
 */

import { render } from "solid-js/web";
import { installShims } from "../platform/shims";

installShims();

import "../styles/base.css";

import { setKeymap } from "../keys/authority";
import { setKeyContext } from "../keys/context";
import { editorKeymapExtension } from "../keys/editor";
import { installKeyboardListener } from "../keys/listener";
import { loadKeymap, presetKeymap } from "../keys/presets";
import { bridge } from "../platform/bridge";
import { userSettings } from "../state/settings";
import { registerShellCommands } from "./commands";
import { installErrorHandlers } from "./errors";
import { PaneShell } from "./paneShell";
import { installLongTaskObserver } from "./perf";
import { currentIdentity } from "./role";
import { Shell } from "./shell";
import { applyScale, applyTheme, watchSystemTheme } from "./theme";
import { wireApp } from "./wire";

export function boot(root: HTMLElement): () => void {
  const identity = currentIdentity();
  const platform = bridge().platform();

  const disposeErrors = installErrorHandlers();
  const disposeLongTasks = import.meta.env.DEV ? installLongTaskObserver() : (): void => {};

  const settings = userSettings();
  applyTheme(settings.theme ?? "system");
  applyScale(settings.uiScale, settings.codeSizePx);
  const disposeThemeWatch = watchSystemTheme(() => {
    // Re-applying is a no-op on the attribute; it exists so a component that
    // cached `resolvedTheme()` gets a chance to re-read.
    applyTheme(userSettings().theme ?? "system");
  });

  // The preset trie is available synchronously; the user overlay arrives later
  // and replaces it. A window whose keyboard does not work for the first 40 ms
  // is a window that feels broken.
  setKeymap(presetKeymap(settings.keymap, platform));
  void loadKeymap(settings.keymap, platform).then(setKeymap);

  const disposeKeys = installKeyboardListener();
  const disposeCommands = registerShellCommands();
  setKeyContext({ platform, layout: "modern" });

  const disposeRender = render(
    () =>
      identity.role === "pane" && identity.paneId !== undefined ? (
        <PaneShell paneId={identity.paneId} />
      ) : (
        <Shell identity={identity} platform={platform} readout={{}} />
      ),
    root,
  );

  // W17: pane registrations, the wasm segmenter, the real IPC sinks, and the
  // `app_ready` handshake. After the render on purpose — the shell paints
  // first, the wiring fills it in (06 §15.1).
  const disposeWiring = wireApp(identity);

  return () => {
    disposeWiring();
    disposeRender();
    disposeCommands();
    disposeKeys();
    disposeThemeWatch();
    disposeLongTasks();
    disposeErrors();
  };
}

/**
 * W13 puts this in CodeMirror's extension list. Re-exported from the entry so
 * the editor has exactly one import for "the app's keyboard", rather than
 * reaching into `keys/` and picking the wrong piece.
 */
export { editorKeymapExtension };

const mountPoint = document.getElementById("root");
if (mountPoint !== null) boot(mountPoint);
