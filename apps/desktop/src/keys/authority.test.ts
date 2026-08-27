/**
 * One keyboard authority — IMPLEMENTATION_PLAN W12 acceptance.
 *
 * "a single capture-phase listener plus one CM6 compartment, both consulting
 * the same trie." The failure this guards is the one every IDE ships once: a
 * binding fires twice inside the editor and once outside it, because two tables
 * both thought they owned the key.
 */

import { EditorState } from "@codemirror/state";
import { EditorView } from "@codemirror/view";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  currentKeymap,
  dispatchKeydown,
  observePending,
  resetAuthority,
  setKeymap,
} from "./authority";
import { resetKeyContext, setKeyContext } from "./context";
import { editorKeymapExtension } from "./editor";
import { installKeyboardListener } from "./listener";
import { clearCommands, registerCommand } from "./registry";
import { type KeyBinding, KeyTrie } from "./trie";

/**
 * Fixtures spell the control modifier `Control`, never the abbreviation:
 * `parseKeystroke` treats the two as exact synonyms, and the abbreviation
 * followed by `+` is banned anywhere under `apps/desktop/src` by W10's
 * `frontend_accelerator_literals` test, which deliberately carves out neither
 * tests nor comments.
 */
const binding = (command: string, key: string, when?: string): KeyBinding =>
  when === undefined
    ? { command, key, source: "preset" }
    : { command, key, when, source: "preset" };

const press = (
  target: EventTarget,
  init: KeyboardEventInit & { key: string; code: string },
): KeyboardEvent => {
  const event = new KeyboardEvent("keydown", { bubbles: true, cancelable: true, ...init });
  target.dispatchEvent(event);
  return event;
};

let disposeListener: (() => void) | undefined;

beforeEach(() => {
  resetAuthority();
  clearCommands();
  resetKeyContext();
  setKeymap(
    new KeyTrie(
      [
        binding("run.block", "Control+Enter"),
        binding("keymap.edit", "Control+K Control+S"),
        binding("edit.only", "Control+E", "editorFocus"),
      ],
      "linux",
    ),
  );
  disposeListener = installKeyboardListener();
});

afterEach(() => {
  disposeListener?.();
  disposeListener = undefined;
  resetAuthority();
  clearCommands();
  resetKeyContext();
  document.body.replaceChildren();
});

describe("the capture-phase listener", () => {
  it("runs a bound command once and consumes the event", () => {
    const run = vi.fn();
    registerCommand({ id: "run.block", title: "Run block", run });

    const event = press(document.body, { key: "Enter", code: "Enter", ctrlKey: true });

    expect(run).toHaveBeenCalledTimes(1);
    expect(event.defaultPrevented).toBe(true);
  });

  it("leaves an unbound keystroke alone", () => {
    const event = press(document.body, { key: "q", code: "KeyQ" });
    expect(event.defaultPrevented).toBe(false);
  });

  it("falls through when the verb is not registered", () => {
    // A keymap may name a verb whose pane has not landed. That must read as
    // "nothing happened" and let the platform have the key, not as a swallowed
    // keystroke — otherwise Mod+C stops copying the day someone adds a binding.
    const event = press(document.body, { key: "Enter", code: "Enter", ctrlKey: true });
    expect(event.defaultPrevented).toBe(false);
  });

  it("respects `when`", () => {
    const run = vi.fn();
    registerCommand({ id: "edit.only", title: "Editor only", run });

    press(document.body, { key: "e", code: "KeyE", ctrlKey: true });
    expect(run).not.toHaveBeenCalled();

    setKeyContext({ editorFocus: true });
    press(document.body, { key: "e", code: "KeyE", ctrlKey: true });
    expect(run).toHaveBeenCalledTimes(1);
  });

  it("holds a chord and publishes the pending prefix", () => {
    const run = vi.fn();
    registerCommand({ id: "keymap.edit", title: "Keymap", run });
    const seen: string[][] = [];
    const stop = observePending((prefix) => seen.push([...prefix]));

    press(document.body, { key: "k", code: "KeyK", ctrlKey: true });
    expect(run).not.toHaveBeenCalled();
    expect(seen.at(-1)).toHaveLength(1);

    press(document.body, { key: "s", code: "KeyS", ctrlKey: true });
    expect(run).toHaveBeenCalledTimes(1);
    expect(seen.at(-1)).toEqual([]);
    stop();
  });

  it("Escape abandons a pending chord without being consumed", () => {
    registerCommand({ id: "keymap.edit", title: "Keymap", run: vi.fn() });
    press(document.body, { key: "k", code: "KeyK", ctrlKey: true });
    const abandon = press(document.body, { key: "Escape", code: "Escape" });
    // Not consumed: the popover under the chord still needs to close.
    expect(abandon.defaultPrevented).toBe(false);

    // And the prefix is gone, so the next key resolves from the root.
    const run = vi.fn();
    registerCommand({ id: "run.block", title: "Run block", run });
    press(document.body, { key: "Enter", code: "Enter", ctrlKey: true });
    expect(run).toHaveBeenCalledTimes(1);
  });
});

describe("the CM6 compartment", () => {
  it("consults the same trie and does not double-fire", () => {
    const run = vi.fn();
    registerCommand({ id: "run.block", title: "Run block", run });

    const parent = document.createElement("div");
    document.body.appendChild(parent);
    const view = new EditorView({
      state: EditorState.create({
        doc: "summarize price mpg",
        extensions: [editorKeymapExtension()],
      }),
      parent,
    });

    // A keystroke inside the editor travels: window capture listener first, then
    // the editor's own handler. Both call `dispatchKeydown`, and it decides once.
    press(view.contentDOM, { key: "Enter", code: "Enter", ctrlKey: true });
    expect(run).toHaveBeenCalledTimes(1);

    view.destroy();
    parent.remove();
  });

  it("is idempotent per event even with no window listener at all", () => {
    // A detached editor window has no capture listener; the compartment is then
    // the only entry point, and it must still fire exactly once.
    disposeListener?.();
    disposeListener = undefined;

    const run = vi.fn();
    registerCommand({ id: "run.block", title: "Run block", run });

    const event = new KeyboardEvent("keydown", { key: "Enter", code: "Enter", ctrlKey: true });
    expect(dispatchKeydown(event)).toBe("handled");
    expect(dispatchKeydown(event)).toBe("handled");
    expect(run).toHaveBeenCalledTimes(1);
  });

  it("shares one trie instance with the listener", () => {
    const trie = new KeyTrie([binding("x", "Control+X")], "linux");
    setKeymap(trie);
    expect(currentKeymap()).toBe(trie);
  });
});
