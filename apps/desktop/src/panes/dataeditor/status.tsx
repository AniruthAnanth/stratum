/**
 * The Data Editor's status bar.
 *
 * 06 §9.7 and the plan both give it verbatim, and both give it as a SEQUENCE:
 *
 *     Vars: 12    Order: Dataset    Obs: 74    Length: 18    Filter: Off
 *
 * Same fields, same order, same names. That is not nostalgia — it is the one
 * surface in the product a Stata user reads without looking, and a status bar
 * that reorders those five fields costs them a glance every time. §39's "must not
 * feel like old Stata with rounded corners" is about the visual language, which
 * is this repository's type scale, ink and 22 px rows; the FACTS are Stata's.
 *
 * `statusLine` exists so that the requirement can be asserted as the literal
 * string it was written as. The DOM version renders the same fields as separate
 * elements, because four literal spaces are not a layout.
 */

import { For, type JSX, Show } from "solid-js";
import { Chip } from "../../ui";
import type { GridStatus } from "./controller";

export interface StatusField {
  label: string;
  value: string;
}

/** Stata's own thousands separators; `Obs:` on a 10 M-row frame is unreadable without. */
const num = (n: number): string => n.toLocaleString("en-US");

export function statusFields(status: GridStatus): StatusField[] {
  return [
    { label: "Vars", value: num(status.vars) },
    { label: "Order", value: status.order },
    { label: "Obs", value: num(status.obs) },
    { label: "Length", value: String(status.length) },
    { label: "Filter", value: status.filter },
  ];
}

/**
 * The line exactly as §9.7 writes it, four spaces between fields.
 *
 * Used by the acceptance test and by `Copy status` — never for layout.
 */
export function statusLine(status: GridStatus): string {
  return statusFields(status)
    .map((f) => `${f.label}: ${f.value}`)
    .join("    ");
}

export interface DataStatusBarProps {
  status: GridStatus;
  notice?: string;
}

export function DataStatusBar(props: DataStatusBarProps): JSX.Element {
  return (
    <footer class="dataeditor__status" data-status-bar>
      <For each={statusFields(props.status)}>
        {(field) => (
          <span class="dataeditor__status-field">
            <span class="dataeditor__status-label">{field.label}:</span>
            <span class="dataeditor__status-value">{field.value}</span>
          </span>
        )}
      </For>

      <div class="dataeditor__status-spacer" />

      <Show when={props.status.capped}>
        {/* Q8's fallback, said out loud. A grid that silently stopped at
            1 000 000 of 10 000 000 observations would be a wrong answer. */}
        <Chip
          tone="stale"
          icon="warn"
          title="Canvas throughput is too low on this platform; the DOM fallback caps the view at 1,000,000 observations (Q8)."
        >
          DOM fallback · first {num(1_000_000)}
        </Chip>
      </Show>

      <Show when={props.notice !== undefined}>
        {/* Assertive rather than polite: everything routed here is a refused
            edit or a truncated copy, i.e. something the user believes happened
            and did not. 06 §17. */}
        <span aria-live="assertive" class="dataeditor__notice">
          <Chip tone="stale" icon="warn">
            {props.notice}
          </Chip>
        </span>
      </Show>

      <span class="dataeditor__status-mode">
        {props.status.mode === "edit" ? "Edit" : "Browse"}
      </span>
    </footer>
  );
}
