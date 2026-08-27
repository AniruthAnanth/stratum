/**
 * The overlay `<input>` — the plan's "so IME, autocorrect and dictation work".
 *
 * 06 §15.3's third cost of the canvas ruling, and the one that is easiest to
 * fake convincingly: a keystroke handler that appends characters to a string
 * looks perfect in English and is unusable in Japanese. The rule that matters is
 * that **while a composition is live, Enter belongs to the input method** — it
 * accepts a candidate, and it must not also commit the cell.
 *
 * `keyCode === 229` is checked alongside `isComposing` because Safari and older
 * WebKit report the composition that way, and WebKitGTK is one of our three
 * targets.
 */

import { beforeEach, describe, expect, test } from "vitest";
import { AUTO_VARS } from "../panes/dataeditor/harness";
import { columnsFromVariables, counters, resetGridCounters } from "./engine";
import { CellEditor, type CommitMove } from "./ime";

const COLUMNS = columnsFromVariables(AUTO_VARS);
const make = COLUMNS[0];
const price = COLUMNS[1];
if (make === undefined || price === undefined) throw new Error("auto.dta's columns did not load");

const RECT = { x: 100, y: 200, w: 79, h: 22 };

interface Harness {
  editor: CellEditor;
  commits: { value: string; move: CommitMove }[];
  cancels: number;
}

function editor(): Harness {
  const commits: { value: string; move: CommitMove }[] = [];
  const h: Harness = {
    commits,
    cancels: 0,
    editor: new CellEditor({
      doc: document,
      onCommit: (value, move) => commits.push({ value, move }),
      onCancel: () => {
        h.cancels += 1;
      },
    }),
  };
  document.body.appendChild(h.editor.element);
  return h;
}

const key = (input: HTMLInputElement, init: KeyboardEventInit & { keyCode?: number }): void => {
  const event = new KeyboardEvent("keydown", { ...init, cancelable: true, bubbles: true });
  if (init.keyCode !== undefined) {
    Object.defineProperty(event, "keyCode", { value: init.keyCode });
  }
  input.dispatchEvent(event);
};

beforeEach(() => {
  resetGridCounters();
  document.body.replaceChildren();
});

describe("it is a real text control, not a keystroke handler", () => {
  test("opening seeds the RAW value and positions the overlay over the cell", () => {
    const h = editor();
    h.editor.openAt(RECT, "4099", price, { row: 41, col: 1 });

    expect(h.editor.isOpen).toBe(true);
    expect(h.editor.element.hidden).toBe(false);
    // `4,099` is what the cell DISPLAYS; `replace price = 4,099` is a syntax
    // error, so the overlay is seeded from `RenderMode::Edit`.
    expect(h.editor.element.value).toBe("4099");
    expect(h.editor.element.style.left).toBe("100px");
    expect(h.editor.element.style.top).toBe("200px");
    expect(h.editor.element.style.width).toBe("79px");
    expect(h.editor.element.style.textAlign).toBe("right");
    expect(h.editor.element.getAttribute("aria-label")).toBe("price, observation 42");
    expect(document.activeElement).toBe(h.editor.element);
    expect(counters.editsBegun).toBe(1);
  });

  test("a numeric cell refuses the platform's help; a string cell asks for it", () => {
    const h = editor();
    h.editor.openAt(RECT, "4099", price, { row: 0, col: 1 });
    expect(h.editor.element.spellcheck).toBe(false);
    expect(h.editor.element.getAttribute("autocorrect")).toBe("off");
    expect(h.editor.element.autocapitalize).toBe("off");

    h.editor.close();
    h.editor.openAt(RECT, "AMC Concord", make, { row: 0, col: 0 });
    // Dictation and autocorrect are the point of the overlay for a str18.
    expect(h.editor.element.spellcheck).toBe(true);
    expect(h.editor.element.getAttribute("autocorrect")).toBe("on");
  });

  test("the editor is not aria-hidden: it IS the editing affordance", () => {
    const h = editor();
    expect(h.editor.element.hasAttribute("aria-hidden")).toBe(false);
    expect(h.editor.element.getAttribute("aria-label")).toBe("Cell value");
  });
});

describe("while composing, Enter belongs to the input method", () => {
  test("Enter during a composition does not commit", () => {
    const h = editor();
    h.editor.openAt(RECT, "", make, { row: 0, col: 0 });
    h.editor.element.dispatchEvent(new CompositionEvent("compositionstart"));
    expect(h.editor.isComposing).toBe(true);
    expect(counters.compositions).toBe(1);

    h.editor.element.value = "にほん";
    key(h.editor.element, { key: "Enter" });
    // The candidate was accepted, not the cell.
    expect(h.commits).toEqual([]);
    expect(h.editor.isOpen).toBe(true);

    h.editor.element.dispatchEvent(new CompositionEvent("compositionend"));
    key(h.editor.element, { key: "Enter" });
    expect(h.commits).toEqual([{ value: "にほん", move: "down" }]);
  });

  test("WebKit's keyCode 229 is treated as a live composition too", () => {
    const h = editor();
    h.editor.openAt(RECT, "", make, { row: 0, col: 0 });
    h.editor.element.value = "한글";
    key(h.editor.element, { key: "Enter", keyCode: 229 });
    expect(h.commits).toEqual([]);
    expect(h.editor.isOpen).toBe(true);
  });

  test("the standard isComposing flag is honoured on its own", () => {
    const h = editor();
    h.editor.openAt(RECT, "", make, { row: 0, col: 0 });
    h.editor.element.value = "中文";
    key(h.editor.element, { key: "Enter", isComposing: true });
    expect(h.commits).toEqual([]);
  });

  test("a blur mid-composition does not commit a half-typed candidate", () => {
    const h = editor();
    h.editor.openAt(RECT, "", make, { row: 0, col: 0 });
    h.editor.element.dispatchEvent(new CompositionEvent("compositionstart"));
    h.editor.element.value = "は";
    h.editor.element.dispatchEvent(new FocusEvent("blur"));
    expect(h.commits).toEqual([]);
  });
});

describe("committing", () => {
  test("Enter, Shift+Enter and Tab each say where to go next", () => {
    for (const [init, move] of [
      [{ key: "Enter" }, "down"],
      [{ key: "Enter", shiftKey: true }, "up"],
      [{ key: "Tab" }, "right"],
      [{ key: "Tab", shiftKey: true }, "left"],
    ] as [KeyboardEventInit, CommitMove][]) {
      const h = editor();
      h.editor.openAt(RECT, "old", make, { row: 0, col: 0 });
      h.editor.element.value = "new";
      key(h.editor.element, init);
      expect(h.commits).toEqual([{ value: "new", move }]);
      expect(h.editor.isOpen).toBe(false);
    }
  });

  test("an unchanged value is not an edit", () => {
    const h = editor();
    h.editor.openAt(RECT, "AMC Concord", make, { row: 0, col: 0 });
    key(h.editor.element, { key: "Enter" });
    // Stata answers an unchanged `replace` with "(0 real changes made)", and a
    // log full of those is a log that is no longer a record of the work.
    expect(h.commits).toEqual([]);
    expect(counters.editsCommitted).toBe(0);
    expect(counters.editsBegun).toBe(1);
  });

  test("Escape discards, and losing focus commits", () => {
    const h = editor();
    h.editor.openAt(RECT, "old", make, { row: 0, col: 0 });
    h.editor.element.value = "typed";
    key(h.editor.element, { key: "Escape" });
    expect(h.commits).toEqual([]);
    expect(h.cancels).toBe(1);

    h.editor.openAt(RECT, "old", make, { row: 0, col: 0 });
    h.editor.element.value = "typed";
    h.editor.element.dispatchEvent(new FocusEvent("blur"));
    // An accidental `replace` is undoable; a lost keystroke is not.
    expect(h.commits).toEqual([{ value: "typed", move: "none" }]);
    expect(counters.editsCommitted).toBe(1);
  });

  test("a cell scrolled out of view commits rather than floating", () => {
    const h = editor();
    h.editor.openAt(RECT, "old", make, { row: 0, col: 0 });
    h.editor.element.value = "typed";
    h.editor.reposition(undefined);
    expect(h.commits).toEqual([{ value: "typed", move: "none" }]);
    expect(h.editor.isOpen).toBe(false);
  });

  test("repositioning moves the overlay with the grid", () => {
    const h = editor();
    h.editor.openAt(RECT, "old", make, { row: 0, col: 0 });
    h.editor.reposition({ x: 12, y: 34, w: 79, h: 22 });
    expect(h.editor.element.style.left).toBe("12px");
    expect(h.editor.element.style.top).toBe("34px");
  });
});
