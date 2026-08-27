/**
 * Turning a typed cell into a Stata command.
 *
 * The plan's bullet, in bold: "**Every edit issues `replace <var> = <val> in <n>`
 * into the log.**" 06 §9.7 gives the reason — "Reproducible by construction". A
 * Data Editor that mutates the frame through a private channel produces a
 * dataset nobody can rebuild from the do-file, which is the exact failure the
 * whole product exists to prevent.
 *
 * So the edit path has no private channel. The pane builds the command text,
 * submits it as `RunIntent::CommandBar`, and the engine's ordinary execution
 * machinery does the rest: the command is echoed into the log, an
 * `ExecutionRecord` lands in the ledger, and the dataset state advances, which
 * invalidates the pages the grid is showing and makes it refetch. One path.
 *
 * The literal is built from the RAW value (`RenderMode::Edit`), never from the
 * displayed one. `price` observation 1 displays as `4,099` under `%8.0gc` and
 * `replace price = 4,099` is a syntax error; `foreign` displays as `Domestic`
 * and its value is `0`.
 */

import type { GridColumn } from "../../grid/engine";

/** A Stata numeric literal, including the exponent forms `di` prints. */
const NUMERIC_LITERAL = /^[-+]?(?:\d+\.?\d*|\.\d+)(?:[eE][-+]?\d+)?$/;

/** `.` and `.a`–`.z`, exactly as `tests/golden/stata18/semantics.log` writes them. */
const MISSING_LITERAL = /^\.[a-z]?$/;

export type EditOutcome =
  | { ok: true; command: string; literal: string }
  | { ok: false; reason: string };

/**
 * The literal for one typed value.
 *
 * A string is `"…"`, or Stata's compound double quotes `` `"…"' `` when the value
 * itself contains a `"` — that is the only way to write such a string in Stata
 * and getting it wrong silently truncates the value at the embedded quote.
 */
export function literalFor(column: GridColumn, raw: string): EditOutcome {
  if (column.isString) {
    if (/[\r\n]/.test(raw)) {
      return { ok: false, reason: "A string value cannot contain a line break." };
    }
    const literal = raw.includes('"') ? `\`"${raw}"'` : `"${raw}"`;
    return { ok: true, command: "", literal };
  }

  const text = raw.trim();
  // An emptied numeric cell is a missing value, which is what Stata does when
  // you clear one in its own Data Editor.
  if (text === "") return { ok: true, command: "", literal: "." };
  if (MISSING_LITERAL.test(text)) return { ok: true, command: "", literal: text };
  if (NUMERIC_LITERAL.test(text)) return { ok: true, command: "", literal: text };
  return {
    ok: false,
    reason:
      column.valueLabel === undefined
        ? `"${raw}" is not a number or a missing value (. or .a–.z).`
        : `"${raw}" is not a number. ${column.name} is labelled by ${column.valueLabel}; type the value, not the label.`,
  };
}

/**
 * `replace <var> = <val> in <n>`, with `n` the 1-based OBSERVATION number.
 *
 * `obs` must be a dataset-order observation index. The pane refuses to edit
 * while a `ViewOrder` is active for exactly this reason: `in` counts
 * observations, a sorted or filtered view counts view rows, and there is no way
 * on the wire to turn one into the other. See `order.ts` and this unit's return.
 */
export function replaceCommand(column: GridColumn, obs: number, raw: string): EditOutcome {
  if (!Number.isInteger(obs) || obs < 1) {
    return { ok: false, reason: `observation ${obs} is not a valid observation number` };
  }
  const literal = literalFor(column, raw);
  if (!literal.ok) return literal;
  return {
    ok: true,
    literal: literal.literal,
    command: `replace ${column.name} = ${literal.literal} in ${obs}`,
  };
}

/**
 * The raw text to seed the overlay input with, from a `RenderMode::Edit` cell.
 *
 * A missing numeric shows as its token (`.`, `.a`) rather than as an empty box:
 * `.a` is a value the user may want to keep or change to `.b`, and an empty
 * field would silently offer to turn it into plain `.`.
 */
export function seedValue(
  cell: { kind: "text"; text: string } | { kind: "num"; value: number; tag: number } | undefined,
  fallback: string,
): string {
  if (cell === undefined) return fallback;
  if (cell.kind === "text") return cell.text;
  if (cell.tag !== 255) return cell.tag === 0 ? "." : `.${String.fromCharCode(96 + cell.tag)}`;
  return String(cell.value);
}
