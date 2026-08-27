/**
 * `summarize` — spec §17, 06 §6.3.
 *
 * Variable · Obs · Mean · Std. dev. · Min · Max, decimal-aligned, tabular
 * figures, booktabs rules, no vertical rules and no zebra striping. A 24-bin
 * sparkline sits at the right: it is deterministic, it is sent with the payload
 * (`SummarizeRow.sparkline`), and it costs nothing.
 *
 * **Every printed number is `row.display.*`.** The `f64`s are not even declared
 * on this renderer's view of the row (see `types.ts`), so there is no way to
 * print one by accident and no way for this card to disagree with the classic
 * text it sits next to — the strings came out of the same `stratum_core::fmt`
 * call. `row.missing` is the exception and is not an exception: an integer count
 * of missing values is exact, has no format, and is what 06 §6.3 asks be shown
 * in amber when it is non-zero.
 *
 * `, detail` adds the percentile block. Same rule: `display_percentiles`,
 * `display_smallest4`, `display_largest4`, `display_stats` — all A6 strings.
 */

import { For, type JSX, Show } from "solid-js";
import { Sparkline } from "../../ui";
import { decimalPad } from "../readout";
import type { SummarizePayloadView, SummarizeRowView } from "../types";

import "./summarize.css";

/** The nine percentiles `SummarizeDetail` carries, in its own order. */
const PERCENTILE_LABELS = ["1%", "5%", "10%", "25%", "50%", "75%", "90%", "95%", "99%"] as const;
const MOMENT_LABELS = ["Skewness", "Kurtosis", "Variance"] as const;

export interface SummarizeCardProps {
  payload: SummarizePayloadView;
  /** A row's `Distribution` / `Missingness` action, routed by the host. */
  onSelectVar?: (name: string) => void;
}

export function SummarizeCard(props: SummarizeCardProps): JSX.Element {
  // Decimal alignment is per COLUMN and computed once (06 §14.3): the padding
  // lands in CSS as `--decimal-pad`, never as a per-cell measurement.
  const pads = (): readonly number[] => decimalPad(props.payload.rows.map((r) => r.format));

  return (
    <div class="summarize" data-summarize>
      <Show when={props.payload.qualifier ?? props.payload.weight}>
        <p class="card__note" data-summarize-qualifier>
          {[props.payload.qualifier, props.payload.weight].filter(Boolean).join(" · ")}
        </p>
      </Show>

      <table class="stat-table summarize__table">
        <thead>
          <tr>
            <th scope="col">Variable</th>
            <th scope="col">Obs</th>
            <th scope="col">Mean</th>
            <th scope="col">Std. dev.</th>
            <th scope="col">Min</th>
            <th scope="col">Max</th>
            <th scope="col" class="summarize__spark-head">
              <span class="visually-hidden">Distribution</span>
            </th>
          </tr>
        </thead>
        <tbody>
          <For each={props.payload.rows}>
            {(row, i) => <Row row={row} pad={pads()[i()] ?? 0} onSelect={props.onSelectVar} />}
          </For>
        </tbody>
      </table>

      <For each={props.payload.rows}>
        {(row) => (
          <Show when={row.detail}>
            {(detail) => (
              <section class="summarize__detail" data-summarize-detail={row.var}>
                <h4 class="summarize__detail-title">{row.var}</h4>
                <div class="summarize__detail-grid">
                  <table class="stat-table">
                    <thead>
                      <tr>
                        <th scope="col">Percentile</th>
                        <th scope="col">Value</th>
                        <th scope="col">Smallest</th>
                        <th scope="col">Largest</th>
                      </tr>
                    </thead>
                    <tbody>
                      <For each={PERCENTILE_LABELS}>
                        {(label, i) => (
                          <tr>
                            <td>{label}</td>
                            <td data-numeric>{detail().display_percentiles[i()] ?? ""}</td>
                            <td data-numeric>{detail().display_smallest4[i()] ?? ""}</td>
                            <td data-numeric>{detail().display_largest4[i()] ?? ""}</td>
                          </tr>
                        )}
                      </For>
                    </tbody>
                  </table>
                  <table class="stat-table">
                    <thead>
                      <tr>
                        <th scope="col">Moment</th>
                        <th scope="col">Value</th>
                      </tr>
                    </thead>
                    <tbody>
                      <For each={MOMENT_LABELS}>
                        {(label, i) => (
                          <tr>
                            <td>{label}</td>
                            <td data-numeric>{detail().display_stats[i()] ?? ""}</td>
                          </tr>
                        )}
                      </For>
                    </tbody>
                  </table>
                </div>
              </section>
            )}
          </Show>
        )}
      </For>
    </div>
  );
}

function Row(props: {
  row: SummarizeRowView;
  pad: number;
  onSelect?: (name: string) => void;
}): JSX.Element {
  return (
    <tr data-summarize-row={props.row.var} data-var-kind={props.row.var_kind}>
      <td>
        {/* A button, not a clickable row: 06 §17 requires a keyboard equivalent
            for every action, and a `<tr onClick>` has none. */}
        <button
          type="button"
          class="summarize__var"
          onClick={() => props.onSelect?.(props.row.var)}
        >
          {props.row.var}
        </button>
        <Show when={props.row.label}>
          {(label) => <span class="summarize__label">{label()}</span>}
        </Show>
      </td>
      <td data-numeric>
        {props.row.display.obs}
        <Show when={props.row.missing > 0}>
          <span class="summarize__missing" data-summarize-missing>
            {` +${String(props.row.missing)} missing`}
          </span>
        </Show>
      </td>
      <td data-numeric style={{ "--decimal-pad": `${String(props.pad)}ch` }}>
        {props.row.display.mean}
      </td>
      <td data-numeric style={{ "--decimal-pad": `${String(props.pad)}ch` }}>
        {props.row.display.sd}
      </td>
      <td data-numeric>{props.row.display.min}</td>
      <td data-numeric>{props.row.display.max}</td>
      <td class="summarize__spark">
        <Show when={props.row.sparkline}>
          {(bins) => (
            <Sparkline bins={bins()} width={60} height={14} label={`${props.row.var} histogram`} />
          )}
        </Show>
      </td>
    </tr>
  );
}
