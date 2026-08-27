/**
 * Turning a Properties edit into a Stata command — 06 §9.5, and [GSM] 2 on this
 * machine, *The Properties window*:
 *
 * > The Properties window allows you to view and edit variable and dataset
 * > properties. … Changing a property in the Properties window will create a
 * > command that appears in the Results and Command windows, so you can see
 * > exactly what you did.
 *
 * That sentence is the whole design. The pane does not "apply a change": it
 * composes the command a user would have typed and submits it through the same
 * `submitCommand` the Command window uses, so the edit lands in Results, in
 * History with its `_rc`, and in the log. 06 §9.5 is explicit that this is a
 * reproducibility feature and not a legacy quirk, and it is the reason this
 * module is pure text: the composition is testable with no pane, no engine and
 * no DOM, and the pane has no private mutation path it could grow later.
 *
 * # Quoting
 *
 * A label is arbitrary user text and it will contain apostrophes ("Driver's
 * seat"), which are harmless, and double quotes, which are not: `label variable
 * x "he said "hi""` is a syntax error. Stata's own answer is the compound double
 * quote — `` `"he said "hi""' `` — and it is what {@link quoteStataString}
 * emits, because the alternatives (dropping the quote, backslash-escaping it)
 * both produce a *different label from the one the user typed* while looking as
 * if they worked.
 *
 * # Deleting rather than blanking
 *
 * `label variable price ""` sets the label to the empty string; `label variable
 * price` **removes** it. They are different states in the `.dta` and `describe`
 * shows them differently, so clearing the field emits the removal form. Same for
 * `label values price .`, which is how the manual spells "no value label".
 */

/** Stata names: 32 chars, letters, digits and `_`, never leading with a digit. */
const NAME = /^[A-Za-z_][A-Za-z0-9_]{0,31}$/u;

export function isStataName(text: string): boolean {
  return NAME.test(text);
}

/** A display format is `%` followed by the spec; `%9.0gc`, `%td`, `%-12s`. */
const FORMAT = /^%-?[0-9]*\.?[0-9]*[a-zA-Z][a-zA-Z0-9_%.]*$/u;

export function isDisplayFormat(text: string): boolean {
  return FORMAT.test(text);
}

/**
 * The literal a user would have typed for this string.
 *
 * Plain `"…"` unless the text contains a double quote, in which case the
 * compound form. A newline cannot survive either form — Stata's command line is
 * a line — so it is folded to a space rather than emitted into a command that
 * would fail with rc 198 for a reason the user cannot see.
 */
export function quoteStataString(text: string): string {
  const flat = text.replace(/[\r\n]+/gu, " ");
  return flat.includes('"') ? `\`"${flat}"'` : `"${flat}"`;
}

/** `rename old new`. */
export function renameCommand(from: string, to: string): string {
  return `rename ${from} ${to}`;
}

/** `label variable price "Price"`, or the removal form for empty text. */
export function labelVariableCommand(name: string, label: string): string {
  return label === ""
    ? `label variable ${name}`
    : `label variable ${name} ${quoteStataString(label)}`;
}

/** `format price %9.0gc`. */
export function formatCommand(name: string, format: string): string {
  return `format ${name} ${format}`;
}

/** `label values price pricelbl`, or `label values price .` to detach one. */
export function valueLabelCommand(name: string, valueLabel: string): string {
  return valueLabel === "" ? `label values ${name} .` : `label values ${name} ${valueLabel}`;
}

/**
 * `note price: text` — a variable note; `note: text` — a dataset note.
 *
 * Notes are appended, never replaced: `note` adds one and `notes drop` removes
 * one. So the pane's Notes control is an *add* affordance rather than a text
 * field that pretends to hold the current value, which is also what stops an
 * edit here from silently discarding notes the user cannot see.
 */
export function noteCommand(name: string | undefined, text: string): string {
  return name === undefined ? `note: ${text}` : `note ${name}: ${text}`;
}

/** `label data "1978 automobile data"`, or the removal form. */
export function labelDataCommand(label: string): string {
  return label === "" ? "label data" : `label data ${quoteStataString(label)}`;
}

/** Which Properties field an edit came from. */
export type PropertyField = "name" | "label" | "format" | "valueLabel" | "note" | "dataLabel";

export interface EditRequest {
  readonly field: PropertyField;
  /** The variable the edit is about; `undefined` for the Data section. */
  readonly variable?: string;
  readonly value: string;
}

export type EditRefusal =
  | { readonly ok: false; readonly reason: string }
  | { readonly ok: true; readonly command: string };

/**
 * The command for one edit, or the reason there is none.
 *
 * Refusals are for edits that cannot *become* a command — a rename to something
 * that is not a name, a format with no `%` — never for edits the engine might
 * reject. `rename price mpg` when `mpg` exists is a real command with a real
 * `r(110)`, and swallowing it here would hide an error the user needs to see in
 * exactly the place 06 §9.5 promises it will appear.
 */
export function commandFor(request: EditRequest): EditRefusal {
  const value = request.value.trim();
  switch (request.field) {
    case "name": {
      const from = request.variable;
      if (from === undefined) return { ok: false, reason: "no variable is selected" };
      if (value === from) return { ok: false, reason: "the name is unchanged" };
      if (!isStataName(value)) {
        return { ok: false, reason: `“${value}” is not a variable name` };
      }
      return { ok: true, command: renameCommand(from, value) };
    }
    case "label": {
      if (request.variable === undefined) return { ok: false, reason: "no variable is selected" };
      return { ok: true, command: labelVariableCommand(request.variable, request.value.trim()) };
    }
    case "format": {
      if (request.variable === undefined) return { ok: false, reason: "no variable is selected" };
      if (!isDisplayFormat(value)) {
        return { ok: false, reason: `“${value}” is not a display format` };
      }
      return { ok: true, command: formatCommand(request.variable, value) };
    }
    case "valueLabel": {
      if (request.variable === undefined) return { ok: false, reason: "no variable is selected" };
      if (value !== "" && !isStataName(value)) {
        return { ok: false, reason: `“${value}” is not a value-label name` };
      }
      return { ok: true, command: valueLabelCommand(request.variable, value) };
    }
    case "note": {
      const text = request.value.replace(/[\r\n]+/gu, " ").trim();
      if (text === "") return { ok: false, reason: "an empty note is not a note" };
      return { ok: true, command: noteCommand(request.variable, text) };
    }
    case "dataLabel":
      return { ok: true, command: labelDataCommand(request.value.trim()) };
    default:
      return { ok: false, reason: "unknown property" };
  }
}
