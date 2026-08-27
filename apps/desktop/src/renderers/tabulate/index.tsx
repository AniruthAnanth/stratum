/**
 * `tabulate`, one-way and two-way — spec §17, 06 §6.5.
 *
 * ── TRUNCATION (W14 acceptance) ──
 *
 * "A `tabulate` with > 5 000 cells renders 2 000 plus 'Open in Table Viewer'. We
 * never build 12 k DOM cells inside an editor widget." That is enforced HERE and
 * not merely honoured: [`shownCells`] caps at [`MAX_CELLS`] whatever the payload
 * says, so an engine that sets `truncated` and then sends every count still
 * cannot make this renderer emit a 12 000-cell table. A 12 k-cell table inside a
 * CodeMirror block widget is ~40 ms of layout on every scroll frame, and the
 * shared canvas grid (06 §15.3) already exists for exactly this.
 *
 * ── ESCALATION: only the frequency layer can be rendered ──
 *
 * `TabulatePayload` carries `counts: Vec<u64>`, `row_totals`, `col_totals` and
 * `total` — exact integers, printed as-is, no format decision — and `requested:
 * Vec<CellStat>` naming which layers Stata printed. It carries **no display
 * string for any of them**. Row %, column %, cell % and expected counts are
 * `count / margin * 100` rendered at two decimals; computing that here means
 * writing a number formatter in TypeScript, which is the exact bug A6 exists to
 * prevent and which this unit's own CI grep forbids. The same gap swallows the
 * `Percent` and `Cum.` columns of a plain one-way `tabulate`.
 *
 * A6 gave `Term`, `SummarizeDetail` and `AnovaTable` their display strings and
 * did not reach this payload. So this renderer draws every layer it can express
 * exactly, names the ones it cannot, and leaves them to `Raw ▸` — where they are
 * StataMP's own bytes. The contract change wanted is a display array parallel to
 * `counts` (one per requested `CellStat`); it is reported, not worked around.
 */

import { For, type JSX, Show, createMemo } from "solid-js";
import type { CellStatView, TabulatePayloadView } from "../types";

import "./tabulate.css";

/** The hard DOM budget. 06 §6.5 and this unit's acceptance both say 2 000. */
export const MAX_CELLS = 2_000;
/** Above this the payload is expected to arrive with `truncated` set. */
export const TRUNCATE_ABOVE = 5_000;

const STAT_LABELS: Readonly<Record<CellStatView, string>> = {
  freq: "frequency",
  row_pct: "row percentage",
  col_pct: "column percentage",
  cell_pct: "cell percentage",
  expected: "expected frequency",
};

/** Cells this renderer will build, whatever the payload claims. */
export function shownCells(payload: TabulatePayloadView): number {
  const cols = Math.max(1, payload.col_keys.length);
  const total = payload.row_keys.length * cols;
  if (total <= TRUNCATE_ABOVE && payload.truncated === null) return total;
  const asked = payload.truncated?.shown_cells ?? MAX_CELLS;
  return Math.min(asked, MAX_CELLS, total);
}

/**
 * A level's header. The value label when the variable has one, else the numeric
 * level — which is an identity, not a statistic, and has no display string in
 * the payload because Stata prints the label whenever one exists.
 */
export function levelLabel(key: readonly [number, string | null]): string {
  return key[1] ?? String(key[0]);
}

export interface TabulateCardProps {
  payload: TabulatePayloadView;
  /** Hands the whole table to the shared canvas grid (06 §15.3). */
  onOpenViewer?: () => void;
}

export function TabulateCard(props: TabulateCardProps): JSX.Element {
  const cols = createMemo((): number => Math.max(1, props.payload.col_keys.length));
  const budget = createMemo((): number => shownCells(props.payload));
  const totalCells = createMemo((): number => props.payload.row_keys.length * cols());
  const truncated = createMemo((): boolean => budget() < totalCells());

  /** Rows to draw, each already clipped to the cell budget. */
  const rows = createMemo((): { index: number; cells: number }[] => {
    const out: { index: number; cells: number }[] = [];
    let left = budget();
    for (let r = 0; r < props.payload.row_keys.length && left > 0; r++) {
      const take = Math.min(cols(), left);
      out.push({ index: r, cells: take });
      left -= take;
    }
    return out;
  });

  /** Layers Stata printed that this payload cannot express as display strings. */
  const unrenderable = createMemo((): readonly CellStatView[] =>
    props.payload.requested.filter((s) => s !== "freq"),
  );

  const count = (r: number, c: number): number => props.payload.counts[r * cols() + c] ?? 0;

  return (
    <div class="tabul" data-tabulate>
      <table class="stat-table tabul__table">
        <caption class="tabul__caption">
          {props.payload.row_label ?? props.payload.row_var}
          <Show when={props.payload.col_var}>
            {(col) => ` × ${props.payload.col_label ?? col()}`}
          </Show>
        </caption>
        <thead>
          <tr>
            <th scope="col">{props.payload.row_label ?? props.payload.row_var}</th>
            <Show when={props.payload.col_keys.length > 0} fallback={<th scope="col">Freq.</th>}>
              <For each={props.payload.col_keys}>
                {(key) => (
                  <th scope="col" title={String(key[0])}>
                    {levelLabel(key)}
                  </th>
                )}
              </For>
              <th scope="col">Total</th>
            </Show>
          </tr>
        </thead>
        <tbody>
          <For each={rows()}>
            {(row) => (
              <tr data-tabulate-row={row.index}>
                <th scope="row" title={String(props.payload.row_keys[row.index]?.[0] ?? "")}>
                  {levelLabel(props.payload.row_keys[row.index] ?? [row.index, null])}
                </th>
                <For each={Array.from({ length: row.cells }, (_, c) => c)}>
                  {(c) => (
                    <td data-numeric data-tabulate-cell>
                      {String(count(row.index, c))}
                    </td>
                  )}
                </For>
                <Show when={props.payload.col_keys.length > 0 && row.cells === cols()}>
                  <td data-numeric class="tabul__margin">
                    {String(props.payload.row_totals[row.index] ?? 0)}
                  </td>
                </Show>
              </tr>
            )}
          </For>
        </tbody>
        <Show when={!truncated()}>
          <tfoot>
            <tr class="tabul__totals">
              <th scope="row">Total</th>
              <Show
                when={props.payload.col_keys.length > 0}
                fallback={
                  <td data-numeric class="tabul__margin">
                    {String(props.payload.total)}
                  </td>
                }
              >
                <For each={props.payload.col_totals}>
                  {(t) => (
                    <td data-numeric class="tabul__margin">
                      {String(t)}
                    </td>
                  )}
                </For>
                <td data-numeric class="tabul__margin">
                  {String(props.payload.total)}
                </td>
              </Show>
            </tr>
          </tfoot>
        </Show>
      </table>

      {/* Association tests as a footnote line, from `AssocTest.display`. */}
      <For each={props.payload.tests}>
        {(test) => (
          <p class="tabul__test" data-assoc-test={test.name}>
            {test.display}
          </p>
        )}
      </For>

      <Show when={unrenderable().length > 0}>
        <p class="card__note" data-tabulate-layers>
          {`${unrenderable()
            .map((s) => STAT_LABELS[s])
            .join(", ")} in classic output`}
        </p>
      </Show>

      <Show when={truncated()}>
        <p class="card__note" data-severity="warning" data-tabulate-truncated>
          {`Table has ${String(props.payload.truncated?.total_cells ?? totalCells())} cells — showing ${String(budget())}. `}
          <button type="button" class="tabul__viewer" onClick={props.onOpenViewer}>
            Open in Table Viewer
          </button>
        </p>
      </Show>
    </div>
  );
}
