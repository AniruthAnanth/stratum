/**
 * The Variables window's columns — 06 §9.4 and [GSM] 2:
 *
 * > By default, it shows all the variables and their variable labels. You can
 * > change what properties get displayed by right-clicking on the header of any
 * > column of the Variables window.
 *
 * Name and Label are on; Type, Format and Value label are one right-click away.
 * The set is deliberately small and every member of it is answerable from the
 * cheap tier of `variables_list` (06 §11.1) — a column that has to wait for
 * `variable_stats` would make scrolling the list issue requests, which is the
 * one thing §11.1 exists to prevent.
 *
 * # Why there is no Notes column
 *
 * 06 §9.4 lists Notes among the addable columns and Stata has one. The wire does
 * not: `stratum_proto::data::VariableInfo` (crates/stratum-proto/src/data.rs:35)
 * carries `name`, `ty`, `label`, `format`, `value_label`, `n_missing` and
 * `provenance` — no notes. A column that renders `—` for every variable in
 * every dataset is worse than an absent one: it teaches the user that this
 * dataset has no notes. Escalated in W16's return; when `VariableInfo` grows the
 * field this file grows one entry and nothing else changes.
 */

import type { VariableRow } from "../../state/vars";

export type VarColumnId = "name" | "label" | "type" | "format" | "valueLabel";

export interface VarColumn {
  readonly id: VarColumnId;
  /** Header text, and what the header context menu calls it. */
  readonly header: string;
  /** On by default? [GSM] 2: "all the variables and their variable labels". */
  readonly defaultVisible: boolean;
  /** Name is the identity column and cannot be hidden. */
  readonly required?: boolean;
  /** CSS grid track. Name and Label carry the width; the rest are snug. */
  readonly track: string;
  readonly value: (row: VariableRow) => string;
}

export const VAR_COLUMNS: readonly VarColumn[] = [
  {
    id: "name",
    header: "Name",
    defaultVisible: true,
    required: true,
    track: "minmax(96px, 1.2fr)",
    value: (row) => row.name,
  },
  {
    id: "label",
    header: "Label",
    defaultVisible: true,
    track: "minmax(120px, 2fr)",
    value: (row) => row.label ?? "",
  },
  {
    id: "type",
    header: "Type",
    defaultVisible: false,
    track: "minmax(56px, max-content)",
    value: (row) => row.storage,
  },
  {
    id: "format",
    header: "Format",
    defaultVisible: false,
    track: "minmax(64px, max-content)",
    value: (row) => row.format,
  },
  {
    id: "valueLabel",
    header: "Value label",
    defaultVisible: false,
    track: "minmax(88px, max-content)",
    value: (row) => row.valueLabel ?? "",
  },
];

export const DEFAULT_VISIBLE_COLUMNS: readonly VarColumnId[] = VAR_COLUMNS.filter(
  (c) => c.defaultVisible,
).map((c) => c.id);

export function columnById(id: VarColumnId): VarColumn {
  const found = VAR_COLUMNS.find((c) => c.id === id);
  // Unreachable through the type, and cheaper to prove than to narrow at every
  // call site: `VarColumnId` is the union of the ids in the table above.
  if (found === undefined) throw new Error(`no such variables column: ${id}`);
  return found;
}

/** The visible columns, always in table order regardless of toggle order. */
export function visibleColumns(shown: ReadonlySet<VarColumnId>): VarColumn[] {
  return VAR_COLUMNS.filter((c) => c.required === true || shown.has(c.id));
}

/**
 * Every visible column's text for one row, lowercased.
 *
 * [GSM] 2: "The filter is applied to all visible columns … By default, the
 * filter will ignore case". Precomputed once per (rows × visible columns) change
 * rather than per keystroke: filtering 5 000 variables through five
 * `toLowerCase()` calls each is 25 000 allocations on every character typed,
 * which is precisely the interaction-path work PRODUCT_SPEC §0a forbids.
 */
export function haystackOf(row: VariableRow, columns: readonly VarColumn[]): string[] {
  return columns.map((c) => c.value(row).toLowerCase());
}
