/**
 * Entry point 2 of the keyboard authority: one CodeMirror 6 compartment
 * (06 §12.1).
 *
 * The compartment holds `Prec.highest(keymap.of(...))` and its handler calls the
 * SAME `dispatchKeydown` the window listener calls, over the same trie, against
 * the same context. It is not a second keymap; it exists for two reasons the
 * window listener cannot cover:
 *
 *  1. **Precedence inside CM6.** `Prec.highest` puts our decision above every
 *     extension's own keymap, so a binding we own can never be pre-empted by a
 *     default that happens to be registered later.
 *  2. **Editors with no window listener.** A detached editor window, or an
 *     editor mounted inside a host page we do not own, still gets the full
 *     keymap because the extension travels with the `EditorView`.
 *
 * `dispatchKeydown` is idempotent per event, so wiring both is safe: whichever
 * runs first decides, and the other reports that decision instead of repeating
 * the command.
 */

import { Compartment, type Extension, Prec } from "@codemirror/state";
import { keymap } from "@codemirror/view";
import { dispatchKeydown } from "./authority";

/** Reconfigured — never re-created — when the keymap preset or overlay changes. */
export const keymapCompartment = new Compartment();

function authorityKeymap(): Extension {
  return Prec.highest(
    keymap.of([
      {
        // `any` sees every keystroke before CM6's own bindings resolve.
        any: (_view, event) => dispatchKeydown(event) !== "ignored",
      },
    ]),
  );
}

/** The extension W13 puts in its extension list. One entry, no arguments. */
export function editorKeymapExtension(): Extension {
  return keymapCompartment.of(authorityKeymap());
}

/**
 * The reconfiguration effect for a preset switch. The compartment's CONTENTS
 * never change — the trie behind `dispatchKeydown` does — so this exists so that
 * CM6 re-runs its keymap ordering after `setKeymap`, not to install new keys.
 */
export function reconfigureEditorKeymap(): ReturnType<Compartment["reconfigure"]> {
  return keymapCompartment.reconfigure(authorityKeymap());
}
