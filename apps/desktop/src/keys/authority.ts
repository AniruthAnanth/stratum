/**
 * The one keyboard authority — 06 §12.1.
 *
 * "Bindings are compiled into a trie by keystroke … Editor bindings go into a
 * CM6 `Compartment` holding `Prec.highest(keymap.of(...))`; everything else is
 * served by one window-level capture-phase `keydown` listener consulting the
 * same trie, so there is exactly one keyboard authority in the app."
 *
 * This module IS that authority. `listener.ts` and `editor.ts` are two entry
 * points into `dispatchKeydown` below; neither carries a table of its own. The
 * pending-chord state lives here for the same reason — a chord half-entered in
 * the editor and completed in the command bar must still fire.
 */

import { keyContext } from "./context";
import { runCommand } from "./registry";
import { type KeyTrie, type Keystroke, eventKeystrokes } from "./trie";

let trie: KeyTrie | undefined;
let pending: string[] = [];
let onPendingChange: ((prefix: readonly string[]) => void) | undefined;

/**
 * Events this authority has already decided about, and what it decided.
 *
 * CM6 sees an event only when the capture listener declined it, but a detached
 * editor window may have no window listener at all — so the decision is recorded
 * per event rather than inferred from who ran first.
 *
 * It records the OUTCOME rather than reading `event.defaultPrevented` back:
 * a non-cancelable event silently ignores `preventDefault`, so that reading
 * would report "ignored" for a command that had already run and the second
 * entry point would run it again.
 */
const decided = new WeakMap<KeyboardEvent, DispatchOutcome>();

export function setKeymap(next: KeyTrie): void {
  trie = next;
  clearPending();
}

export function currentKeymap(): KeyTrie | undefined {
  return trie;
}

/** The status bar renders the half-entered chord, as VS Code and Emacs both do. */
export function observePending(fn: (prefix: readonly string[]) => void): () => void {
  onPendingChange = fn;
  return () => {
    onPendingChange = undefined;
  };
}

function clearPending(): void {
  if (pending.length === 0) return;
  pending = [];
  onPendingChange?.(pending);
}

export type DispatchOutcome = "handled" | "pending" | "ignored";

function decide(event: KeyboardEvent, outcome: DispatchOutcome): DispatchOutcome {
  decided.set(event, outcome);
  return outcome;
}

/**
 * The single resolution path. Idempotent per event: a second call for an event
 * already decided reports what was decided the first time, so two entry points
 * can both be wired without double-firing a command.
 */
export function dispatchKeydown(event: KeyboardEvent): DispatchOutcome {
  const already = decided.get(event);
  if (already !== undefined) return already;
  if (trie === undefined) return "ignored";

  const strokes: Keystroke[] = eventKeystrokes(event);
  if (strokes.length === 0) return "ignored"; // a bare modifier is not a keystroke

  // Escape abandons a half-entered chord and is not otherwise consumed, so it
  // still closes the popover underneath.
  if (pending.length > 0 && strokes.some((s) => s.kind === "code" && s.id === "Escape")) {
    clearPending();
    return decide(event, "ignored");
  }

  const resolution = trie.resolve(pending, strokes, keyContext());
  switch (resolution.kind) {
    case "pending":
      pending = [...resolution.prefix];
      onPendingChange?.(pending);
      event.preventDefault();
      return decide(event, "pending");

    case "command": {
      clearPending();
      const outcome = runCommand(resolution.command, resolution.args, keyContext());
      if (outcome === "ran") {
        event.preventDefault();
        return decide(event, "handled");
      }
      // An unregistered or disabled verb must fall through to the platform, or
      // Mod+C stops copying the moment a pane forgets to register a command.
      return decide(event, "ignored");
    }

    default:
      // A keystroke that continues nothing and matches nothing also abandons a
      // pending chord: Emacs behaviour, and it stops a stuck prefix eating keys.
      clearPending();
      return "ignored";
  }
}

/** Test seam. */
export function resetAuthority(): void {
  trie = undefined;
  pending = [];
  onPendingChange = undefined;
}
