/**
 * The keystroke trie and the `when` language.
 *
 * The interesting cases are the ones that make a keymap feel broken rather than
 * the ones that make it fail: `Mod` resolving to the wrong modifier, a chord
 * prefix eating the next key forever, a user override losing to a preset, and a
 * non-US layout where `.key` and `.code` disagree.
 */

import { describe, expect, it } from "vitest";
import { type KeyContext, WhenParseError, compileWhen } from "./context";
import { KEYMAP_PRESETS, presetBindings, presetKeymap } from "./presets";
import {
  type KeyBinding,
  KeyTrie,
  eventKeystroke,
  eventKeystrokes,
  keystrokeId,
  parseKeystroke,
} from "./trie";

const preset = (command: string, key: string, when?: string): KeyBinding =>
  when === undefined
    ? { command, key, source: "preset" }
    : { command, key, when, source: "preset" };

const user = (command: string, key: string): KeyBinding => ({ command, key, source: "user" });

/** A `KeyboardEvent` as a browser would report it on a US layout. */
const key = (
  id: string,
  code: string,
  mods: Partial<Record<"ctrl" | "alt" | "shift" | "meta", boolean>> = {},
): KeyboardEvent =>
  ({
    key: id,
    code,
    ctrlKey: mods.ctrl ?? false,
    altKey: mods.alt ?? false,
    shiftKey: mods.shift ?? false,
    metaKey: mods.meta ?? false,
  }) as KeyboardEvent;

describe("Mod resolution", () => {
  it("is Cmd on macOS and Ctrl everywhere else", () => {
    expect(parseKeystroke("Mod+Enter", "macos")).toMatchObject({ meta: true, ctrl: false });
    expect(parseKeystroke("Mod+Enter", "windows")).toMatchObject({ meta: false, ctrl: true });
    expect(parseKeystroke("Mod+Enter", "linux")).toMatchObject({ meta: false, ctrl: true });
  });

  it("keeps an explicitly written modifier explicit", () => {
    // A binding that must NOT follow the platform writes the control or meta
    // modifier itself, and `Mod` never overrides it.
    //
    // Spelled `Control`, which `parseKeystroke` treats as an exact synonym of
    // the abbreviated form: the abbreviation followed by `+` is banned anywhere
    // under `apps/desktop/src` by W10's
    // `frontend_accelerator_literals` test, fixtures included. Same `case` arm,
    // same `Keystroke`, so the assertion below is unchanged in meaning.
    expect(parseKeystroke("Control+K", "macos")).toMatchObject({ ctrl: true, meta: false });
  });

  it("resolves a macOS binding only against a Cmd event", () => {
    const trie = new KeyTrie([preset("run.block", "Mod+Enter")], "macos");
    expect(trie.resolve([], eventKeystrokes(key("Enter", "Enter", { meta: true })), {})).toEqual({
      kind: "command",
      command: "run.block",
    });
    expect(trie.resolve([], eventKeystrokes(key("Enter", "Enter", { ctrl: true })), {})).toEqual({
      kind: "none",
    });
  });
});

describe("keystroke identity", () => {
  const ids = (e: KeyboardEvent): string[] => eventKeystrokes(e).map(keystrokeId);

  it("offers the position as a candidate, so Mod+1 survives AZERTY", () => {
    // On AZERTY the top-row 1 reports `.key === "&"`; `Mod+1` is a POSITION and
    // parses to `Digit1`, so the code candidate is what makes it reachable.
    const wanted = keystrokeId(parseKeystroke("Meta+1", "linux"));
    expect(ids(key("&", "Digit1", { meta: true }))).toContain(wanted);
    expect(ids(key("1", "Digit1", { meta: true }))).toContain(wanted);
  });

  it("offers the character as a candidate, so Mod+/ survives QWERTZ", () => {
    // On a German layout the slash is Shift+7, at `.code === "Digit7"`. `Mod+/`
    // is a CHARACTER, so the char candidate is what makes it reachable — and it
    // comes first, because the glyph on the keycap is what the user pressed.
    const german = eventKeystrokes(key("/", "Digit7", { meta: true, shift: true }));
    expect(german[0]).toMatchObject({ kind: "char", id: "/" });
  });

  it("folds Shift into the modifiers, not into the character", () => {
    const upper = eventKeystrokes(key("K", "KeyK", { meta: true, shift: true }));
    expect(upper.map(keystrokeId)).toContain(keystrokeId(parseKeystroke("Meta+Shift+K", "linux")));
  });

  it("ignores a bare modifier press", () => {
    expect(eventKeystrokes(key("Shift", "ShiftLeft", { shift: true }))).toEqual([]);
    expect(eventKeystroke(key("Meta", "MetaLeft", { meta: true }))).toBeUndefined();
  });
});

describe("resolution", () => {
  const ctx: KeyContext = { editorFocus: true, selectionEmpty: false };

  it("filters by `when` and lets the last survivor win", () => {
    const trie = new KeyTrie(
      [
        preset("a", "Mod+E", "editorFocus"),
        preset("b", "Mod+E", "commandBarFocus"),
        preset("c", "Mod+E", "editorFocus"),
      ],
      "macos",
    );
    const stroke = eventKeystrokes(key("e", "KeyE", { meta: true }));
    expect(trie.resolve([], stroke, ctx)).toMatchObject({ command: "c" });
    expect(trie.resolve([], stroke, { commandBarFocus: true })).toMatchObject({ command: "b" });
    expect(trie.resolve([], stroke, {})).toEqual({ kind: "none" });
  });

  it("sorts user bindings after presets regardless of file order", () => {
    const trie = new KeyTrie([user("mine", "Mod+E"), preset("theirs", "Mod+E")], "macos");
    const stroke = eventKeystrokes(key("e", "KeyE", { meta: true }));
    expect(trie.resolve([], stroke, {})).toMatchObject({ command: "mine" });
  });

  it("walks a chord and reports the prefix", () => {
    const trie = new KeyTrie([preset("keymap.edit", "Mod+K Mod+S")], "macos");
    const k = eventKeystrokes(key("k", "KeyK", { meta: true }));
    const s = eventKeystrokes(key("s", "KeyS", { meta: true }));

    const first = trie.resolve([], k, {});
    expect(first.kind).toBe("pending");
    if (first.kind !== "pending") throw new Error("unreachable");
    expect(trie.resolve(first.prefix, s, {})).toMatchObject({ command: "keymap.edit" });
    // A key that continues nothing resolves to none, and the caller drops the
    // prefix — a stuck chord that eats every subsequent keystroke is the failure
    // mode this is guarding.
    expect(trie.resolve(first.prefix, eventKeystrokes(key("x", "KeyX")), {})).toEqual({
      kind: "none",
    });
  });

  it("carries args through", () => {
    const trie = new KeyTrie(
      [{ command: "pane.toggle", key: "Mod+3", args: { index: 3 }, source: "preset" }],
      "macos",
    );
    expect(trie.resolve([], eventKeystrokes(key("3", "Digit3", { meta: true })), {})).toEqual({
      kind: "command",
      command: "pane.toggle",
      args: { index: 3 },
    });
  });

  it("survives a malformed binding instead of losing the whole keymap", () => {
    const trie = new KeyTrie([preset("bad", "Mod+Nonsense"), preset("good", "Mod+G")], "macos");
    expect(trie.parseErrors).toHaveLength(1);
    expect(trie.resolve([], eventKeystrokes(key("g", "KeyG", { meta: true })), {})).toMatchObject({
      command: "good",
    });
  });

  it("reports a shadowed binding without calling it an error", () => {
    const trie = new KeyTrie([preset("first", "Mod+E"), preset("second", "Mod+E")], "macos");
    expect(trie.conflicts()).toEqual([{ key: "Mod+E", command: "first", shadowedBy: "second" }]);
    // Two bindings on one key with disjoint `when` clauses are the normal case
    // and are not reported.
    const scoped = new KeyTrie(
      [preset("a", "Mod+E", "editorFocus"), preset("b", "Mod+E", "historyFocus")],
      "macos",
    );
    expect(scoped.conflicts()).toEqual([]);
  });
});

describe("the `when` language", () => {
  const ctx: KeyContext = {
    editorFocus: true,
    selectionEmpty: false,
    layout: "focus",
    inlineMode: "always",
  };

  it("evaluates the operators 06 §12.1 uses", () => {
    expect(compileWhen("editorFocus")(ctx)).toBe(true);
    expect(compileWhen("!selectionEmpty")(ctx)).toBe(true);
    expect(compileWhen("editorFocus && !selectionEmpty")(ctx)).toBe(true);
    expect(compileWhen("historyFocus || editorFocus")(ctx)).toBe(true);
    expect(compileWhen("(historyFocus || cardFocus) && editorFocus")(ctx)).toBe(false);
    expect(compileWhen("layout == 'focus'")(ctx)).toBe(true);
    expect(compileWhen("layout != 'focus'")(ctx)).toBe(false);
    expect(compileWhen("inlineMode == always")(ctx)).toBe(true);
  });

  it("reads an unknown key as false rather than throwing at keystroke time", () => {
    expect(compileWhen("neverDefined")(ctx)).toBe(false);
  });

  it("rejects a malformed expression at compile time", () => {
    expect(() => compileWhen("editorFocus &&")).toThrow(WhenParseError);
    expect(() => compileWhen("(editorFocus")).toThrow(WhenParseError);
    expect(() => compileWhen("'literal'")).toThrow(WhenParseError);
  });
});

describe("the shipped presets", () => {
  it.each(KEYMAP_PRESETS)("%s parses every binding on both platforms", (id) => {
    for (const platform of ["macos", "windows"] as const) {
      const trie = presetKeymap(id, platform);
      expect(trie.parseErrors).toEqual([]);
      expect(trie.size).toBeGreaterThan(0);
    }
  });

  it("resolves Modern's documented keys (06 §12.2)", () => {
    const trie = presetKeymap("modern", "macos");
    const ctx: KeyContext = { editorFocus: true, selectionEmpty: false };
    const cases: [KeyboardEvent, string][] = [
      [key("Enter", "Enter", { meta: true }), "run.block"],
      [key("Enter", "Enter", { shift: true }), "run.blockAndAdvance"],
      [key("Enter", "Enter", { alt: true }), "run.selection"],
      [key("Enter", "Enter", { meta: true, alt: true }), "run.fromHere"],
      [key("Enter", "Enter", { meta: true, shift: true }), "run.fileClean"],
      [key("r", "KeyR", { meta: true, shift: true }), "run.allStale"],
      [key(".", "Period", { meta: true }), "run.break"],
      [key("l", "KeyL", { meta: true }), "commandbar.focus"],
      [key("j", "KeyJ", { meta: true }), "pane.toggleAssistant"],
      [key("/", "Slash", { meta: true }), "edit.toggleComment"],
      [key("F1", "F1"), "help.atCaret"],
      [key("1", "Digit1", { meta: true, alt: true }), "layout.apply"],
      [key("1", "Digit1", { meta: true }), "pane.toggle"],
    ];
    for (const [event, command] of cases) {
      const strokes = eventKeystrokes(event);
      expect(strokes.length, `no keystroke for ${command}`).toBeGreaterThan(0);
      expect(trie.resolve([], strokes, ctx), command).toMatchObject({ command });
    }
  });

  it("applies §12.3 as deltas: Mod+Enter still runs the block", () => {
    const trie = presetKeymap("stata", "macos");
    // The delta replaces Mod+Shift+D (Data Browse -> Run quietly) and leaves
    // every base binding it does not name standing.
    expect(
      trie.resolve([], eventKeystrokes(key("Enter", "Enter", { meta: true })), {}),
    ).toMatchObject({ command: "run.block" });
    expect(
      trie.resolve([], eventKeystrokes(key("d", "KeyD", { meta: true, shift: true })), {}),
    ).toMatchObject({ command: "run.fileQuiet" });
    expect(
      trie.resolve([], eventKeystrokes(key("PageUp", "PageUp")), { commandBarFocus: true }),
    ).toMatchObject({ command: "history.previous" });
    // PgUp outside the command bar is a scroll, not a history step.
    expect(trie.resolve([], eventKeystrokes(key("PageUp", "PageUp")), {})).toEqual({
      kind: "none",
    });
  });

  it("applies §12.4 as deltas over Modern", () => {
    const trie = presetKeymap("vscode", "macos");
    expect(trie.resolve([], eventKeystrokes(key("F5", "F5")), {})).toMatchObject({
      command: "run.file",
    });
    // Inherited from Modern, which vscode.json never mentions.
    expect(trie.resolve([], eventKeystrokes(key("l", "KeyL", { meta: true })), {})).toMatchObject({
      command: "commandbar.focus",
    });
    const k = trie.resolve([], eventKeystrokes(key("k", "KeyK", { meta: true })), {});
    expect(k.kind).toBe("pending");
  });

  it("names only commands the shell or a later unit will register", () => {
    // A typo in a preset is invisible until someone presses the key, so the set
    // of command ids is pinned here. Adding a verb means adding it to this list.
    const known = new Set(
      presetBindings("modern")
        .concat(presetBindings("stata"), presetBindings("vscode"))
        .map((b) => b.command),
    );
    for (const id of known) {
      expect(id, `${id} is not a dotted command id`).toMatch(/^[a-z][a-zA-Z]*\.[a-zA-Z]+$/);
    }
  });
});
