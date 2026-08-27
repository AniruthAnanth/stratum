/**
 * The block-status glyph — 06 §4.5, §5.1, §17.
 *
 * Nine states, nine distinct SHAPES. 06 §17: "Colour is never the only channel:
 * every state has a distinct glyph shape." A user with deuteranopia and a user
 * looking at a greyscale screenshot in a bug report must both be able to tell a
 * stale block from a failed one, so the colour is confirmation, not information.
 *
 * The glyphs are drawn rather than typed. A `✓` from a text font is whatever
 * that font decided; these are on the same 14-unit grid, with the same 1.25px
 * square-capped stroke, as every other icon in the product (§14.7).
 */

import type { JSX } from "solid-js";
import type { BlockStatusState } from "../ipc/hand";

interface Glyph {
  /** SVG path on the 14x14 grid. */
  d: string;
  filled: boolean;
  /** The colour token. `--state-*`, never a raw hex. */
  token: string;
  label: string;
}

const GLYPHS: Readonly<Record<BlockStatusState, Glyph>> = {
  // A quiet open ring: the absence of a state, not a warning about one.
  never_run: {
    d: "M7 2.5 A4.5 4.5 0 1 1 7 11.5 A4.5 4.5 0 1 1 7 2.5",
    filled: false,
    token: "var(--state-never-run)",
    label: "never run",
  },
  // Queued: the ring with a bar, so a queue of forty blocks reads as a column.
  queued: {
    d: "M7 2.5 A4.5 4.5 0 1 1 7 11.5 A4.5 4.5 0 1 1 7 2.5 M5 7 H9",
    filled: false,
    token: "var(--state-never-run)",
    label: "queued",
  },
  running: {
    d: "M4.5 2.5 L11.5 7 L4.5 11.5 Z",
    filled: true,
    token: "var(--accent)",
    label: "running",
  },
  current: {
    d: "M2.5 7.5 L5.5 10.5 L11.5 3.5",
    filled: false,
    token: "var(--state-ok)",
    label: "current",
  },
  // The tick plus a raised dot: ran cleanly, but INV-1 is unprovable (Taint).
  current_unverifiable: {
    d: "M2.5 8.5 L5 11 L10 4.5 M12 2.5 L12 4.5",
    filled: false,
    token: "var(--state-ok)",
    label: "current, unverifiable",
  },
  // A broken ring: the shape of "was current, is not any more".
  stale: {
    d: "M4 3.2 A4.5 4.5 0 1 0 10.8 5.2 M9 2.6 L11.5 3.5 L10.6 6",
    filled: false,
    token: "var(--state-stale)",
    label: "stale",
  },
  failed: {
    d: "M3.5 3.5 L10.5 10.5 M10.5 3.5 L3.5 10.5",
    filled: false,
    token: "var(--state-failed)",
    label: "failed",
  },
  // The cross with a bar: re-running would ERROR, not merely differ.
  broken: {
    d: "M3 3 L9 9 M9 3 L3 9 M12 2.5 V8 M12 10 V11",
    filled: false,
    token: "var(--state-failed)",
    label: "broken",
  },
  interrupted: {
    d: "M3.5 3.5 H6 V10.5 H3.5 Z M8 3.5 H10.5 V10.5 H8 Z",
    filled: false,
    token: "var(--state-interrupted)",
    label: "interrupted",
  },
};

export interface StateGlyphProps {
  state: BlockStatusState;
  /** Extra detail for the accessible name, e.g. "code changed since E41". */
  detail?: string;
}

export function StateGlyph(props: StateGlyphProps): JSX.Element {
  const glyph = (): Glyph => GLYPHS[props.state];
  const label = (): string =>
    props.detail === undefined ? glyph().label : `${glyph().label} — ${props.detail}`;

  return (
    <svg
      class="state-glyph"
      width="14"
      height="14"
      viewBox="0 0 14 14"
      role="img"
      aria-label={label()}
      data-state={props.state}
    >
      <title>{label()}</title>
      <path
        d={glyph().d}
        stroke={glyph().filled ? "none" : glyph().token}
        fill={glyph().filled ? glyph().token : "none"}
        stroke-width="var(--icon-stroke)"
        stroke-linecap="square"
        stroke-linejoin="miter"
      />
    </svg>
  );
}

/** The `--state-*` token for a state, for the rail and the gutter. */
export function stateToken(state: BlockStatusState): string {
  return GLYPHS[state].token;
}

export function stateLabel(state: BlockStatusState): string {
  return GLYPHS[state].label;
}
