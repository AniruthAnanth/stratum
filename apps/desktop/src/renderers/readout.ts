/**
 * The state readout — `E41 · D17 · 0.08s` (06 §4.6, spec §13).
 *
 * This is the ONE place in `src/renderers/` that turns a number into a string,
 * and it is allowed to exist for a reason that is worth stating precisely: a
 * duration is not a statistic. It never appears in Stata's classic output, so
 * there is nothing for a card to disagree with, and no `stratum_core::fmt` call
 * produced it. Every number that DOES appear in the classic text arrives
 * pre-formatted (A6) and is printed verbatim.
 *
 * Even here the arithmetic is integral. `duration_us` is microseconds as an
 * integer; the seconds string is assembled from integer centiseconds and
 * `padStart`, never from `toFixed`. That is not pedantry — it is what lets
 * `contract.test.ts` grep this whole directory for `toFixed`, `toPrecision`,
 * `toExponential` and `Intl.NumberFormat` and demand zero hits, with no
 * exemption list to argue about later.
 */

import type { DatasetStateId, ExecId } from "../ipc/hand";

/** Microseconds per centisecond, per second, per minute. */
const US_PER_CS = 10_000;
const CS_PER_S = 100;
const S_PER_MIN = 60;

/**
 * `8_412 µs -> "0.01s"`, `1_204 µs -> "0.00s"`, `95_600_000 µs -> "1m 35.60s"`.
 *
 * Rounds half away from zero at centisecond resolution, matching what a reader
 * expects of a stopwatch; a duration is never compared for equality anywhere, so
 * the tie rule is a display choice rather than a correctness one.
 */
export function durationLabel(durationUs: number): string {
  const cs = Math.round(Math.max(0, durationUs) / US_PER_CS);
  const whole = Math.floor(cs / CS_PER_S);
  const frac = String(cs % CS_PER_S).padStart(2, "0");
  if (whole < S_PER_MIN) return `${whole}.${frac}s`;
  const mins = Math.floor(whole / S_PER_MIN);
  const secs = String(whole % S_PER_MIN).padStart(2, "0");
  return `${mins}m ${secs}.${frac}s`;
}

/**
 * The three fields of spec §13's status line, in the order 06 §4.6 draws them.
 * Returned as parts rather than a joined string so the separator is markup —
 * `·` between spans, so a screen reader does not read "E41 middot D17".
 */
export interface Readout {
  readonly exec: string;
  readonly dataset: string;
  readonly duration: string;
  /** The accessible name for the whole group. */
  readonly label: string;
}

export function readout(exec: ExecId, dataset: DatasetStateId, durationUs: number): Readout {
  const e = `E${String(exec)}`;
  const d = `D${String(dataset)}`;
  const t = durationLabel(durationUs);
  return {
    exec: e,
    dataset: d,
    duration: t,
    label: `execution ${String(exec)}, dataset state ${String(dataset)}, ${t}`,
  };
}

/**
 * Decimal alignment from a Stata display format — 06 §14.3.
 *
 * Returns the number of character widths to reserve to the RIGHT of a column's
 * digits so that the decimal points line up under `text-align: right`. This is
 * geometry, not value formatting: it never looks at a number, only at the format
 * string the variable carries, and the answer lands in CSS as `--decimal-pad`
 * (see `styles/tables.css`, W12) once per column rather than once per cell.
 *
 * `%8.0gc` -> 0, `%9.2f` -> 0, `%12.4f` in a column whose widest sibling is
 * `%12.2f` -> 2. The caller passes the column's formats; the padding is the
 * per-cell shortfall against the widest decimal count in the column.
 */
export function decimalPlaces(format: string): number {
  // `%[-][0-9]*.[0-9]*[fgexs...]`. Anything that does not match has no declared
  // decimal count, which is the honest answer for `%8.0gc` and for `%-9s`.
  const dot = format.indexOf(".");
  if (dot < 0) return 0;
  let i = dot + 1;
  let n = 0;
  while (i < format.length) {
    const c = format.charCodeAt(i);
    if (c < 0x30 || c > 0x39) break;
    n = n * 10 + (c - 0x30);
    i += 1;
  }
  // `%8.0g` declares zero decimals but prints as many as %g wants, so it cannot
  // anchor a column. Treat it as unknown rather than as zero.
  return format.charAt(i) === "g" ? 0 : n;
}

/** The per-cell `--decimal-pad` for a column of formats, in `ch`. */
export function decimalPad(formats: readonly string[]): readonly number[] {
  const places = formats.map(decimalPlaces);
  const widest = places.reduce((a, b) => (a > b ? a : b), 0);
  return places.map((p) => widest - p);
}
