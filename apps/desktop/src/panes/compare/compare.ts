/**
 * Model comparison, the deterministic half — spec §19, 06 §7.
 *
 * Everything here is a pure function over estimation payloads, because every
 * rule §19 states is a rule about ORDER and MEMBERSHIP, and those are exactly
 * the things a table built ad hoc in a component gets subtly wrong:
 *
 *  * **Comparability**: same `depvar`, and `sample_hash` equal — or the user
 *    explicitly opted into different samples. A same-depvar, different-sample
 *    pair is offered and carries a persistent warning row, because silently
 *    comparing across samples is a real methodological error, not a nit.
 *  * **Row order** is order of first appearance across models, left to right,
 *    with `_cons` always last. No alphabetisation: it destroys the model's
 *    narrative.
 *  * **Footer scalars** come from the union of the models' `scalars` keys,
 *    ordered by a fixed preference list, with `—` for a model that does not
 *    report one. Never blank.
 *
 * No number is formatted here either. A coefficient cell is `display_num[0]`
 * over `display_num[1]`; the raw `p` is read for one thing only — comparing
 * against a star threshold — and is never printed.
 */

import type { EstimationPayloadView, ScalarsPayloadView } from "../../renderers";

/** One model in the comparison, with whatever display strings came with it. */
export interface CompareModel {
  readonly label: string;
  readonly payload: EstimationPayloadView;
  /** The sibling `Scalars` payload, the only source of `e()` display strings. */
  readonly scalars?: ScalarsPayloadView;
}

/** 06 §7: stars ON in this view by default, with a configurable threshold set. */
export const DEFAULT_STARS: readonly (readonly [number, string])[] = [
  [0.001, "***"],
  [0.01, "**"],
  [0.05, "*"],
];

export function stars(
  p: number,
  thresholds: readonly (readonly [number, string])[] = DEFAULT_STARS,
): string {
  if (!Number.isFinite(p)) return "";
  for (const [cut, mark] of thresholds) if (p < cut) return mark;
  return "";
}

/** The footer's fixed preference order (06 §7). Unknown keys follow, in order. */
export const SCALAR_PREFERENCE: readonly string[] = [
  "N",
  "r2",
  "r2_a",
  "r2_w",
  "F",
  "chi2",
  "ll",
  "aic",
  "bic",
  "rmse",
];

export type Comparability =
  | { readonly ok: true }
  | { readonly ok: false; readonly reason: "depvar"; readonly depvars: readonly string[] }
  | {
      readonly ok: false;
      readonly reason: "sample";
      /** Observation counts, in model order, for the warning row's own words. */
      readonly obs: readonly number[];
    };

/**
 * Two or more estimations are comparable when every `depvar` matches and every
 * `sample_hash` matches. A sample mismatch is a *warning*, not a refusal — §19
 * says the pair is offered — so the caller renders the table AND the row.
 */
export function comparability(models: readonly CompareModel[]): Comparability {
  const first = models[0];
  if (first === undefined) return { ok: true };
  const depvars = [...new Set(models.map((m) => m.payload.depvar))];
  if (depvars.length > 1) return { ok: false, reason: "depvar", depvars };
  // Compared as strings: the key is a u64 and `Number` cannot hold one (see
  // `EstimationPayloadView.sample_hash`).
  const hashes = new Set(models.map((m) => String(m.payload.sample_hash)));
  if (hashes.size > 1) {
    return { ok: false, reason: "sample", obs: models.map((m) => m.payload.n) };
  }
  return { ok: true };
}

/** The persistent row §19 requires: `Samples differ: 74 vs 69 observations`. */
export function sampleWarning(c: Comparability): string | undefined {
  if (c.ok) return undefined;
  if (c.reason === "depvar") {
    return `Different dependent variables: ${c.depvars.join(" vs ")}`;
  }
  return `Samples differ: ${c.obs.map(String).join(" vs ")} observations`;
}

export interface CompareCell {
  /** `display_num[0]` — the coefficient, verbatim. */
  readonly b: string;
  /** `display_num[1]` — the standard error, verbatim. */
  readonly se: string;
  readonly stars: string;
  /** `omitted` / `base` / not in this model. */
  readonly note?: "omitted" | "base" | "absent";
}

export interface CompareRow {
  readonly term: string;
  readonly cells: readonly (CompareCell | undefined)[];
}

export interface CompareFooterRow {
  readonly name: string;
  /** `—` where a model does not report it. Never blank (06 §7). */
  readonly values: readonly string[];
}

export interface CompareTable {
  readonly labels: readonly string[];
  readonly depvars: readonly string[];
  readonly rows: readonly CompareRow[];
  readonly footer: readonly CompareFooterRow[];
  readonly warning: string | undefined;
}

const CONS = "_cons";
const MISSING = "—";

/**
 * Row order: first appearance across models, left to right; `_cons` last.
 * Deterministic and independent of `Map` iteration luck — insertion order is
 * exactly what is wanted here and is what `Map` guarantees.
 */
export function termOrder(models: readonly CompareModel[]): readonly string[] {
  const seen = new Set<string>();
  const order: string[] = [];
  for (const model of models) {
    for (const term of model.payload.terms) {
      if (term.display === CONS || seen.has(term.display)) continue;
      seen.add(term.display);
      order.push(term.display);
    }
  }
  if (models.some((m) => m.payload.terms.some((t) => t.display === CONS))) order.push(CONS);
  return order;
}

export function buildCompareTable(
  models: readonly CompareModel[],
  thresholds: readonly (readonly [number, string])[] = DEFAULT_STARS,
): CompareTable {
  const byTerm = models.map((m) => new Map(m.payload.terms.map((t) => [t.display, t])));

  const rows: CompareRow[] = termOrder(models).map((term) => ({
    term,
    cells: byTerm.map((index): CompareCell | undefined => {
      const t = index.get(term);
      if (t === undefined) return { b: MISSING, se: "", stars: "", note: "absent" };
      if (t.omitted) return { b: "0", se: "", stars: "", note: "omitted" };
      if (t.base) return { b: "", se: "", stars: "", note: "base" };
      return {
        b: t.display_num[0] ?? "",
        se: t.display_num[1] ?? "",
        stars: stars(t.p, thresholds),
      };
    }),
  }));

  // The union of scalar keys, preference-ordered, plus `N` which is always
  // reportable because `payload.n` is an exact integer.
  const keys = new Set<string>(["N"]);
  for (const model of models) for (const [name] of model.payload.scalars) keys.add(name);
  const ordered = [
    ...SCALAR_PREFERENCE.filter((k) => keys.has(k)),
    ...[...keys].filter((k) => !SCALAR_PREFERENCE.includes(k)).sort(),
  ];

  const footer: CompareFooterRow[] = ordered.map((name) => ({
    name,
    values: models.map((m) => {
      if (name === "N") return String(m.payload.n);
      const display = m.scalars?.values.find(([k]) => k === name)?.[1];
      if (display === undefined) return MISSING;
      return display.t === "num" ? display.display : display.value;
    }),
  }));

  return {
    labels: models.map((m) => m.label),
    depvars: models.map((m) => m.payload.depvar),
    rows,
    footer,
    warning: sampleWarning(comparability(models)),
  };
}

/**
 * "Copy as `esttab` command" (06 §7) — the action a real user wants, because it
 * makes the comparison reproducible in the `.do` file rather than only on screen.
 * Emitted from `estimates_name` where one exists; a model that was never stored
 * cannot be named, and inventing a name would produce a command that fails.
 */
export function esttabCommand(models: readonly CompareModel[]): string | undefined {
  const names = models.map((m) => m.payload.estimates_name);
  if (names.some((n) => n === null || n === undefined)) return undefined;
  return `esttab ${names.join(" ")}, se star(* 0.05 ** 0.01 *** 0.001) label`;
}
