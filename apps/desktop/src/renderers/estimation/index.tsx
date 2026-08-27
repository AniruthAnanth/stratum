/**
 * `regress` and every other estimation command — spec §§4, 17; 06 §6.4.
 *
 * Two parts, in Stata's own reading order: a model strip, then the coefficient
 * table with a CI strip.
 *
 * **Nothing here formats a number.** Every coefficient cell is
 * `term.display_num[i]`, produced by the same `stratum_core::fmt` call that
 * produced the classic text (A6). The `f64`s `b`, `ci_lo` and `ci_hi` are read
 * for exactly one thing — the pixel geometry of the CI bar — and that value is
 * never shown as text.
 *
 * ── ESCALATION, and why this card's model strip is shorter than 06 §6.4 draws ──
 *
 * 06 §6.4 specifies the strip as
 * `N 74 · F(3,70) 52.25 · Prob>F 0.0000 · R² 0.6913 · Adj R² 0.6781 · Root MSE 3.2827`.
 * Of those, `N` is an exact integer (`payload.n`) and the two degrees of freedom
 * are strings in `AnovaTable.display` (A6). **`F`, `Prob>F`, `R²`, `Adj R²` and
 * `Root MSE` are not.** `EstimationPayload.scalars` is `Vec<(String, f64)>` with
 * no display sibling — A6 added `Term.display_num`, `SummarizeDetail.display_*`
 * and `AnovaTable.display` and did not reach this field. Printing them would
 * mean re-implementing `fmt_g` in TypeScript, which is precisely the bug A6
 * exists to prevent and which this unit's own CI grep forbids.
 *
 * So this renderer prints an `e()` scalar only when a display string for it is
 * in hand, and takes it from a sibling `ResultPayload::Scalars` on the same
 * envelope when the engine sends one (`ScalarValue::Num { value, display }` has
 * exactly the pair that is missing above, and §5.1's `payloads` is a `Vec`
 * precisely so one result can carry several). Absent that, the statistics are
 * one click away in `Sources ▸` and in `Raw ▸`, and no number is invented.
 * `EstimationPayload` wants a `display_scalars: Vec<String>` parallel to
 * `scalars`; that is a contract change and is reported rather than worked around.
 */

import { For, type JSX, Show, createMemo, createSignal } from "solid-js";
import { Chip } from "../../ui";
import type { EstimationPayloadView, ScalarsPayloadView, TermView } from "../types";

import "./estimation.css";

/** The `AnovaTable.display` order, from §5.2. */
const MSS = 0;
const DF_M = 1;
const MS_M = 2;
const RSS = 3;
const DF_R = 4;
const MS_R = 5;
const TSS = 6;
const DF_T = 7;
const MS_T = 8;

/** `Term.display_num` order, from §5.2 (A6). */
const B = 0;
const SE = 1;
const T = 2;
const P = 3;
const CI_LO = 4;
const CI_HI = 5;

/**
 * The `e()` scalars 06 §6.4 wants in the model strip, in the order it lists
 * them. Names are `ereturn` names; the lookup is by name against whatever
 * display strings the envelope actually carries.
 */
const STRIP_SCALARS: readonly (readonly [string, string])[] = [
  ["F", "F"],
  ["p", "Prob > F"],
  ["r2", "R²"],
  ["r2_a", "Adj R²"],
  ["rmse", "Root MSE"],
  ["ll", "Log likelihood"],
  ["chi2", "χ²"],
];

export interface EstimationCardProps {
  payload: EstimationPayloadView;
  /**
   * A sibling `Scalars` payload on the same envelope, if the engine sent one.
   * The ONLY source of display strings for `e()` scalars — see the escalation
   * above.
   */
  scalars?: ScalarsPayloadView;
}

export function EstimationCard(props: EstimationCardProps): JSX.Element {
  const [showSources, setShowSources] = createSignal(false);

  /** name -> display string, from the sibling payload only. */
  const displayScalars = createMemo((): ReadonlyMap<string, string> => {
    const map = new Map<string, string>();
    for (const [name, value] of props.scalars?.values ?? []) {
      if (value.t === "num") map.set(name, value.display);
      else map.set(name, value.value);
    }
    return map;
  });

  const strip = createMemo((): readonly (readonly [string, string])[] => {
    const out: (readonly [string, string])[] = [["N", String(props.payload.n)]];
    const anova = props.payload.anova;
    if (anova !== null && anova !== undefined) {
      const dfM = anova.display[DF_M];
      const dfR = anova.display[DF_R];
      if (dfM !== undefined && dfR !== undefined) out.push(["df", `${dfM} / ${dfR}`]);
    }
    for (const [name, label] of STRIP_SCALARS) {
      const shown = displayScalars().get(name);
      if (shown !== undefined) out.push([label, shown]);
    }
    return out;
  });

  // One shared scale across all terms (06 §6.4). Geometry only: these numbers
  // become SVG coordinates and are never rendered as text.
  const scale = createMemo((): { lo: number; hi: number } => {
    let lo = 0;
    let hi = 0;
    for (const t of props.payload.terms) {
      if (t.omitted || t.base || t.empty) continue;
      if (Number.isFinite(t.ci_lo) && t.ci_lo < lo) lo = t.ci_lo;
      if (Number.isFinite(t.ci_hi) && t.ci_hi > hi) hi = t.ci_hi;
    }
    return lo === hi ? { lo: -1, hi: 1 } : { lo, hi };
  });

  return (
    <div class="estim" data-estimation>
      <div class="estim__strip" data-estimation-strip>
        <For each={strip()}>
          {([label, value]) => (
            <span class="estim__stat" data-stat={label}>
              <span class="estim__stat-label">{label}</span>
              <span class="estim__stat-value">{value}</span>
            </span>
          )}
        </For>
        {/* 06 §6.4: VCE shows as a chip only when it is not the default. */}
        <Show when={props.payload.vce !== "ols" && props.payload.vce !== ""}>
          <Chip tone="neutral" icon="dot">
            {`VCE ${props.payload.vce}`}
          </Chip>
        </Show>
        <Show when={props.payload.estimates_name}>
          {(name) => <Chip tone="accent">{`stored as ${name()}`}</Chip>}
        </Show>
      </div>

      <table class="stat-table estim__table" data-estimation-table>
        <thead>
          <tr>
            <th scope="col">{props.payload.depvar}</th>
            <th scope="col">Coefficient</th>
            <th scope="col">Std. err.</th>
            <th scope="col">t</th>
            <th scope="col">P&gt;|t|</th>
            <th scope="col" colSpan="2">
              [95% conf. interval]
            </th>
            <th scope="col" class="estim__ci-head">
              <span class="visually-hidden">Confidence interval</span>
            </th>
          </tr>
        </thead>
        <tbody>
          <For each={props.payload.terms}>
            {(term) => <CoefRow term={term} lo={scale().lo} hi={scale().hi} />}
          </For>
        </tbody>
      </table>

      {/* Stata prints the ANOVA block, so it must be one click away, not gone. */}
      <Show when={props.payload.anova}>
        {(anova) => (
          <div class="estim__sources">
            <button
              type="button"
              class="estim__disclose"
              data-estimation-sources-toggle
              aria-expanded={showSources() ? "true" : "false"}
              onClick={() => setShowSources((v) => !v)}
            >
              {showSources() ? "Sources ▾" : "Sources ▸"}
            </button>
            <Show when={showSources()}>
              <table class="stat-table estim__anova" data-estimation-anova>
                <thead>
                  <tr>
                    <th scope="col">Source</th>
                    <th scope="col">SS</th>
                    <th scope="col">df</th>
                    <th scope="col">MS</th>
                  </tr>
                </thead>
                <tbody>
                  <tr>
                    <td>Model</td>
                    <td data-numeric>{anova().display[MSS]}</td>
                    <td data-numeric>{anova().display[DF_M]}</td>
                    <td data-numeric>{anova().display[MS_M]}</td>
                  </tr>
                  <tr>
                    <td>Residual</td>
                    <td data-numeric>{anova().display[RSS]}</td>
                    <td data-numeric>{anova().display[DF_R]}</td>
                    <td data-numeric>{anova().display[MS_R]}</td>
                  </tr>
                  <tr>
                    <td>Total</td>
                    <td data-numeric>{anova().display[TSS]}</td>
                    <td data-numeric>{anova().display[DF_T]}</td>
                    <td data-numeric>{anova().display[MS_T]}</td>
                  </tr>
                </tbody>
              </table>
            </Show>
          </div>
        )}
      </Show>

      {/* Deterministic model notes. NEVER AI-generated (§5.2). */}
      <For each={props.payload.diagnostics}>
        {(flag) => (
          <p class="card__note" data-severity={flag.severity} data-model-flag={flag.code}>
            {flag.message}
            <Show when={flag.vars.length > 0}>{`: ${flag.vars.join(", ")}`}</Show>
          </p>
        )}
      </For>
    </div>
  );
}

/** SVG geometry for the CI bar. Pixels, never text. */
const CI_W = 90;
const CI_H = 12;

function CoefRow(props: { term: TermView; lo: number; hi: number }): JSX.Element {
  const inert = (): boolean => props.term.omitted || props.term.base || props.term.empty;
  const x = (v: number): number => {
    const span = props.hi - props.lo;
    if (span === 0 || !Number.isFinite(v)) return 0;
    return Math.round(((v - props.lo) / span) * CI_W);
  };

  return (
    <tr data-term={props.term.name} data-inert={inert() ? "" : undefined}>
      <td>{props.term.display}</td>
      <Show
        when={!inert()}
        fallback={
          <td class="estim__inert" colSpan="7">
            {props.term.omitted ? "0  (omitted)" : props.term.base ? "(base)" : "(empty)"}
          </td>
        }
      >
        <td data-numeric>{props.term.display_num[B]}</td>
        <td data-numeric>{props.term.display_num[SE]}</td>
        <td data-numeric>{props.term.display_num[T]}</td>
        <td data-numeric>{props.term.display_num[P]}</td>
        <td data-numeric>{props.term.display_num[CI_LO]}</td>
        <td data-numeric>{props.term.display_num[CI_HI]}</td>
        <td class="estim__ci">
          <svg
            width={CI_W}
            height={CI_H}
            viewBox={`0 0 ${String(CI_W)} ${String(CI_H)}`}
            role="img"
            aria-label={`95% interval ${props.term.display_num[CI_LO] ?? ""} to ${props.term.display_num[CI_HI] ?? ""}`}
          >
            <line x1={x(0)} x2={x(0)} y1="0" y2={CI_H} stroke="var(--rule-mid)" stroke-width="1" />
            <line
              x1={x(props.term.ci_lo)}
              x2={x(props.term.ci_hi)}
              y1={CI_H / 2}
              y2={CI_H / 2}
              stroke="var(--accent)"
              stroke-width="1"
              stroke-linecap="square"
            />
            <rect
              x={x(props.term.b) - 1}
              y={CI_H / 2 - 2}
              width="2"
              height="4"
              fill="var(--accent)"
            />
          </svg>
        </td>
      </Show>
    </tr>
  );
}
