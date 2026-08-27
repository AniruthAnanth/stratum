/**
 * Entry point 1 of the keyboard authority: one window-level, capture-phase
 * `keydown` listener (06 §12.1).
 *
 * Capture phase, not bubble: the listener must see the event before CodeMirror,
 * the data grid, a dockview tab strip or a native `<input>` gets to act on it,
 * and `stopPropagation()` from capture is what keeps a bound keystroke from
 * being handled twice by two components that each think they own it.
 */

import { dispatchKeydown } from "./authority";

export interface KeyboardListenerOptions {
  /** Defaults to `window`. A detached pane window passes its own. */
  target?: Window;
}

export function installKeyboardListener(options: KeyboardListenerOptions = {}): () => void {
  const target = options.target ?? window;

  const handler = (event: KeyboardEvent): void => {
    const outcome = dispatchKeydown(event);
    if (outcome === "ignored") return;
    // `preventDefault` was already called by the authority; stopping propagation
    // here is what makes this listener the only consumer of a bound keystroke.
    event.stopPropagation();
  };

  target.addEventListener("keydown", handler, true);
  return () => target.removeEventListener("keydown", handler, true);
}
