/**
 * The cell editor — a real `<input>` positioned over the canvas.
 *
 * 06 §15.3, third cost of the canvas ruling: "**IME and editing** → the edited
 * cell gets a real `<input>` positioned over the canvas, so composition,
 * autocorrect and dictation work."
 *
 * That sentence is a hard requirement and it is why this is not a keystroke
 * handler that appends characters to a string. A CJK input method, macOS
 * dictation, Android's autocorrect and every accessibility keyboard on all three
 * platforms drive a focused text control through composition events. Anything
 * that reads `keydown` and paints the character itself is broken for those
 * users and works fine in the tests of whoever wrote it.
 *
 * The one rule that matters: **while `isComposing` is true, Enter belongs to the
 * input method**, not to us. Pressing Enter to accept a candidate must not
 * commit the cell — that is the bug that makes an editor unusable in Japanese
 * while looking perfect in English.
 */

import { type GridColumn, counters } from "./engine";

export type CommitMove = "none" | "down" | "up" | "right" | "left";

export interface CellEditorOptions {
  doc?: Document;
  /** The user finished. `value` is raw text; the caller turns it into `replace`. */
  onCommit: (value: string, move: CommitMove) => void;
  onCancel: () => void;
}

export interface EditorRect {
  x: number;
  y: number;
  w: number;
  h: number;
}

export class CellEditor {
  readonly element: HTMLInputElement;
  private readonly options: CellEditorOptions;
  private open_ = false;
  private composing_ = false;
  private original = "";
  private cell: { row: number; col: number } | undefined;

  constructor(options: CellEditorOptions) {
    this.options = options;
    const doc = options.doc ?? document;
    const input = doc.createElement("input");
    input.type = "text";
    input.className = "grid__editor";
    input.hidden = true;
    // Not `aria-hidden`: this IS the editing affordance and a screen reader must
    // see it. The mirror's `aria-activedescendant` points at the cell behind it.
    input.setAttribute("aria-label", "Cell value");
    this.element = input;

    input.addEventListener("compositionstart", () => {
      this.composing_ = true;
      counters.compositions += 1;
    });
    input.addEventListener("compositionend", () => {
      this.composing_ = false;
    });

    input.addEventListener("keydown", (event: KeyboardEvent) => {
      // `isComposing` is the standard flag; `keyCode === 229` is what Safari and
      // older WebKit report instead, and WebKitGTK is one of our three targets.
      if (this.composing_ || event.isComposing || event.keyCode === 229) return;
      switch (event.key) {
        case "Enter":
          event.preventDefault();
          this.commit(event.shiftKey ? "up" : "down");
          break;
        case "Tab":
          event.preventDefault();
          this.commit(event.shiftKey ? "left" : "right");
          break;
        case "Escape":
          event.preventDefault();
          this.cancel();
          break;
        default:
          // Every other key belongs to the input, including the arrows: moving
          // the caret inside a half-typed value is not a grid navigation.
          event.stopPropagation();
      }
    });

    input.addEventListener("blur", () => {
      // Losing focus commits, as Stata's Data Editor does. Discarding instead
      // would lose a typed value to a stray click, and an accidental `replace`
      // is undoable while a lost keystroke is not.
      if (this.open_ && !this.composing_) this.commit("none");
    });
  }

  get isOpen(): boolean {
    return this.open_;
  }

  get isComposing(): boolean {
    return this.composing_;
  }

  get editing(): { row: number; col: number } | undefined {
    return this.cell;
  }

  /**
   * Opens the editor over a cell.
   *
   * `value` is the RAW value — `RenderMode::Edit`'s f64 rendered plainly, or the
   * string variable's stored bytes — not the display text. Editing `4,099` and
   * sending `replace price = 4,099` would be a syntax error, and editing the
   * value label `Domestic` instead of `0` would be a worse one.
   */
  openAt(
    rect: EditorRect,
    value: string,
    column: GridColumn,
    at: { row: number; col: number },
  ): void {
    this.open_ = true;
    this.original = value;
    this.cell = at;
    counters.editsBegun += 1;

    const input = this.element;
    input.hidden = false;
    input.value = value;
    input.style.left = `${rect.x}px`;
    input.style.top = `${rect.y}px`;
    input.style.width = `${rect.w}px`;
    input.style.height = `${rect.h}px`;
    input.style.textAlign = column.align;
    // A string cell wants the platform's autocorrect and dictation; a numeric
    // one wants neither, and `.a` must survive whatever the keyboard thinks of it.
    input.spellcheck = column.isString;
    input.autocapitalize = column.isString ? "sentences" : "off";
    input.setAttribute("autocomplete", "off");
    input.setAttribute("autocorrect", column.isString ? "on" : "off");
    input.setAttribute("inputmode", "text");
    input.setAttribute("aria-label", `${column.name}, observation ${at.row + 1}`);
    input.focus();
    input.select();
  }

  close(): void {
    this.open_ = false;
    this.composing_ = false;
    this.cell = undefined;
    this.element.hidden = true;
    this.element.value = "";
  }

  /** Moves the overlay with the grid; a scrolled-away cell closes the editor. */
  reposition(rect: EditorRect | undefined): void {
    if (!this.open_) return;
    if (rect === undefined) {
      this.commit("none");
      return;
    }
    this.element.style.left = `${rect.x}px`;
    this.element.style.top = `${rect.y}px`;
  }

  private commit(move: CommitMove): void {
    if (!this.open_) return;
    const value = this.element.value;
    this.close();
    // An unchanged value is not an edit. Stata answers `replace` with
    // "(0 real changes made)", and writing that line into the log for every cell
    // a user clicked through would make the log useless as a record of the work.
    if (value === this.original) return;
    counters.editsCommitted += 1;
    this.options.onCommit(value, move);
  }

  private cancel(): void {
    if (!this.open_) return;
    this.close();
    this.options.onCancel();
  }

  dispose(): void {
    this.element.remove();
  }
}
