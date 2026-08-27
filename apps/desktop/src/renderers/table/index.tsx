/**
 * The matrix-shaped results — `Table`, `Scalars` and `DataChanged` (§5.2).
 *
 * `GenericTable` is the fallback for any command whose result is a matrix we do
 * not have a bespoke renderer for. It is not a lesser card: it uses the same
 * `.stat-table` rules, the same numerals and the same alignment as `summarize`
 * and `regress`, because 06 §14.8 rule 1 is that every result surface in the
 * product shares them.
 *
 * `Cell::Num` and `ScalarValue::Num` both carry `{ value, display }`. `display`
 * is what is printed; `value` exists for sorting and export and is not read
 * here. `Cell` of `None` renders as BLANK and never as "." — a dot is Stata's
 * missing-value marker and an absent cell is not a missing value.
 *
 * `DataChanged` is the "✓ 0.08s · +1 var" chip's payload, and every number in it
 * is an exact count.
 */

import { For, type JSX, Show } from "solid-js";
import { Chip } from "../../ui";
import type {
  AlignView,
  CellView,
  DataChangedPayloadView,
  ScalarsPayloadView,
  TablePayloadView,
} from "../types";

import "./table.css";

function cellText(cell: CellView | null | undefined): string {
  if (cell === null || cell === undefined) return "";
  return cell.t === "num" ? cell.display : cell.value;
}

export interface TableCardProps {
  payload: TablePayloadView;
}

export function TableCard(props: TableCardProps): JSX.Element {
  const cols = (): number => props.payload.colnames.length;
  const align = (c: number): AlignView => props.payload.col_align[c] ?? "right";

  return (
    <div class="gtab" data-table>
      <table class="stat-table">
        <Show when={props.payload.title}>
          {(title) => <caption class="gtab__caption">{title()}</caption>}
        </Show>
        <thead>
          <tr>
            <th scope="col" />
            <For each={props.payload.colnames}>
              {(name, c) => (
                <th scope="col" data-align={align(c())}>
                  {name}
                </th>
              )}
            </For>
          </tr>
        </thead>
        <tbody>
          <For each={props.payload.rownames}>
            {(rowname, r) => (
              <tr data-table-row={rowname}>
                <th scope="row">{rowname}</th>
                <For each={Array.from({ length: cols() }, (_, c) => c)}>
                  {(c) => {
                    const cell = (): CellView | null =>
                      props.payload.cells[r() * cols() + c] ?? null;
                    return (
                      <td data-align={align(c)} data-numeric={cell()?.t === "num" ? "" : undefined}>
                        {cellText(cell())}
                      </td>
                    );
                  }}
                </For>
              </tr>
            )}
          </For>
        </tbody>
      </table>
    </div>
  );
}

export interface ScalarsCardProps {
  payload: ScalarsPayloadView;
}

export function ScalarsCard(props: ScalarsCardProps): JSX.Element {
  return (
    <dl class="scalars" data-scalars>
      <For each={props.payload.values}>
        {([name, value]) => (
          <div class="scalars__pair" data-scalar={name}>
            <dt>{name}</dt>
            <dd>{cellText(value)}</dd>
          </div>
        )}
      </For>
    </dl>
  );
}

export interface DataChangedCardProps {
  payload: DataChangedPayloadView;
}

/** `+1 var`, `-5 obs` — exact integer deltas, assembled, never formatted. */
export function deltaChips(payload: DataChangedPayloadView): readonly string[] {
  const out: string[] = [];
  const obs = payload.obs_after - payload.obs_before;
  const vars = payload.vars_after - payload.vars_before;
  if (obs !== 0) out.push(`${obs > 0 ? "+" : "−"}${String(Math.abs(obs))} obs`);
  if (vars !== 0) out.push(`${vars > 0 ? "+" : "−"}${String(Math.abs(vars))} var`);
  return out;
}

export function DataChangedCard(props: DataChangedCardProps): JSX.Element {
  return (
    <div class="dchg" data-data-changed>
      <div class="dchg__chips">
        <Chip tone="neutral">{props.payload.frame}</Chip>
        <For each={deltaChips(props.payload)}>
          {(text) => (
            <Chip tone="ok" icon="dot">
              {text}
            </Chip>
          )}
        </For>
        <span class="dchg__shape">
          {`${String(props.payload.obs_after)} obs × ${String(props.payload.vars_after)} vars`}
        </span>
      </div>

      <For
        each={
          [
            ["created", props.payload.created],
            ["modified", props.payload.modified],
            ["dropped", props.payload.dropped],
          ] as const
        }
      >
        {([label, names]) => (
          <Show when={names.length > 0}>
            <p class="card__note" data-change={label}>
              {`${label}: ${names.join(", ")}`}
            </p>
          </Show>
        )}
      </For>

      <Show when={props.payload.renamed.length > 0}>
        <p class="card__note" data-change="renamed">
          {`renamed: ${props.payload.renamed.map(([from, to]) => `${from} → ${to}`).join(", ")}`}
        </p>
      </Show>

      <For each={props.payload.notes}>{(note) => <p class="card__note">{note}</p>}</For>
    </div>
  );
}
