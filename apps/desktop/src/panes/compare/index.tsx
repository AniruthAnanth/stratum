/**
 * The model-comparison pane — spec §19, 06 §7.
 *
 * Detachable, `Mod+Shift+M`. Every rule the table obeys lives in `compare.ts` as
 * a pure function; this file draws the result of those functions and holds no
 * ordering, membership or comparability logic of its own.
 *
 * The one behaviour worth stating in the component: **the warning row is
 * persistent.** It is not a toast, not a tooltip and not dismissible. Comparing
 * two models fitted on different samples is a methodological error the reader of
 * a screenshot must also be able to see, so the row sits inside the table's own
 * frame and is exported with it.
 */

import { For, type JSX, Show, createMemo } from "solid-js";
import { PaneHeader } from "../../ui";
import { type CompareModel, buildCompareTable, esttabCommand } from "./compare";

import "./compare.css";

export interface ComparePaneProps {
  models: readonly CompareModel[];
  /** Copy actions are host-owned; the pane reports the request. */
  onCopy?: (format: "latex" | "markdown" | "csv" | "esttab", text: string) => void;
}

export function ComparePane(props: ComparePaneProps): JSX.Element {
  const table = createMemo(() => buildCompareTable(props.models));
  const esttab = createMemo(() => esttabCommand(props.models));

  return (
    <section class="cmp" data-pane="compare">
      <PaneHeader title="Compare models" />

      <Show
        when={props.models.length > 1}
        fallback={
          <p class="cmp__empty">
            Select two or more estimation results to compare. Anything named by
            <code> estimates store </code> appears here.
          </p>
        }
      >
        <div class="cmp__scroll">
          <table class="stat-table cmp__table" data-compare-table>
            <thead>
              <tr>
                <th scope="col" />
                <For each={table().labels}>
                  {(label, i) => (
                    <th scope="col">
                      <span class="cmp__label">{`(${String(i() + 1)})`}</span>
                      <span class="cmp__model">{label}</span>
                      <span class="cmp__depvar">{table().depvars[i()]}</span>
                    </th>
                  )}
                </For>
              </tr>
            </thead>

            <tbody>
              <For each={table().rows}>
                {(row) => (
                  <tr data-compare-term={row.term}>
                    <th scope="row">{row.term}</th>
                    <For each={row.cells}>
                      {(cell) => (
                        <td data-numeric data-note={cell?.note}>
                          <Show when={cell}>
                            {(c) => (
                              <>
                                <span class="cmp__b">{`${c().b}${c().stars}`}</span>
                                <Show when={c().se !== ""}>
                                  <span class="cmp__se">{`(${c().se})`}</span>
                                </Show>
                              </>
                            )}
                          </Show>
                        </td>
                      )}
                    </For>
                  </tr>
                )}
              </For>
            </tbody>

            <tfoot>
              <For each={table().footer}>
                {(row) => (
                  <tr class="cmp__footer-row" data-compare-scalar={row.name}>
                    <th scope="row">{row.name}</th>
                    <For each={row.values}>{(value) => <td data-numeric>{value}</td>}</For>
                  </tr>
                )}
              </For>
              <Show when={table().warning}>
                {(warning) => (
                  <tr class="cmp__warning" data-compare-warning>
                    <td colSpan={table().labels.length + 1}>{warning()}</td>
                  </tr>
                )}
              </Show>
            </tfoot>
          </table>
        </div>

        <p class="cmp__legend">
          <span>* p&lt;.05 ** p&lt;.01 *** p&lt;.001</span>
          <Show when={esttab()}>
            {(command) => (
              <button
                type="button"
                class="cmp__esttab"
                onClick={() => props.onCopy?.("esttab", command())}
              >
                Copy as esttab command
              </button>
            )}
          </Show>
        </p>
      </Show>
    </section>
  );
}
