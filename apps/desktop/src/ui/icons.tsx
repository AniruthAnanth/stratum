/**
 * The icon set — 06 §14.7.
 *
 * 14x14 grid, 1.25px strokes, **square caps and square joins**, geometric, drawn
 * on a half-pixel grid so they are crisp at 1x. The square caps are the
 * deliberate difference from Feather and Lucide, whose rounded caps read as
 * friendly; these have to read as technical. No icon font, no emoji anywhere in
 * the product UI, and no colour inside an icon except the six state colours.
 *
 * Coordinates sit on `.5` so a 1.25px stroke lands on a device pixel boundary at
 * 1x. Moving one of these to a whole number is what makes an icon look soft.
 */

import type { JSX } from "solid-js";

export type IconName =
  | "run"
  | "run-from"
  | "stop"
  | "rerun"
  | "chevron-down"
  | "chevron-right"
  | "close"
  | "search"
  | "layout"
  | "pane-left"
  | "pane-right"
  | "pane-bottom"
  | "detach"
  | "check"
  | "warn"
  | "error"
  | "dot"
  | "circle";

/** Path data on the 14-unit grid. Stroked unless the name is listed in `FILLED`. */
const PATHS: Readonly<Record<IconName, string>> = {
  run: "M4.5 2.5 L11.5 7 L4.5 11.5 Z",
  "run-from": "M2.5 2.5 L7.5 7 L2.5 11.5 M8.5 2.5 L13.5 7 L8.5 11.5",
  stop: "M3.5 3.5 H10.5 V10.5 H3.5 Z",
  rerun: "M11.5 4.5 A5 5 0 1 0 12.5 7 M11.5 1.5 V4.5 H8.5",
  "chevron-down": "M3.5 5.5 L7 9 L10.5 5.5",
  "chevron-right": "M5.5 3.5 L9 7 L5.5 10.5",
  close: "M3.5 3.5 L10.5 10.5 M10.5 3.5 L3.5 10.5",
  search: "M6.5 2.5 A4 4 0 1 1 6.5 10.5 A4 4 0 1 1 6.5 2.5 M9.5 9.5 L12.5 12.5",
  layout: "M1.5 2.5 H12.5 V11.5 H1.5 Z M5.5 2.5 V11.5 M5.5 7 H12.5",
  "pane-left": "M1.5 2.5 H12.5 V11.5 H1.5 Z M5.5 2.5 V11.5",
  "pane-right": "M1.5 2.5 H12.5 V11.5 H1.5 Z M8.5 2.5 V11.5",
  "pane-bottom": "M1.5 2.5 H12.5 V11.5 H1.5 Z M1.5 8.5 H12.5",
  detach: "M7.5 1.5 H12.5 V6.5 M12.5 1.5 L7 7 M10.5 9.5 V12.5 H1.5 V3.5 H4.5",
  check: "M2.5 7.5 L5.5 10.5 L11.5 3.5",
  warn: "M7 1.5 L13 12.5 H1 Z M7 5.5 V8.5 M7 10.5 V10.5",
  error: "M3.5 3.5 L10.5 10.5 M10.5 3.5 L3.5 10.5",
  dot: "M7 4.5 A2.5 2.5 0 1 1 7 9.5 A2.5 2.5 0 1 1 7 4.5",
  circle: "M7 1.5 A5.5 5.5 0 1 1 7 12.5 A5.5 5.5 0 1 1 7 1.5",
};

const FILLED = new Set<IconName>(["run", "stop", "dot"]);

export interface IconProps {
  name: IconName;
  /** Defaults to `currentColor`; only the six state colours may be passed. */
  color?: string;
  title?: string;
  class?: string;
}

export function Icon(props: IconProps): JSX.Element {
  const filled = (): boolean => FILLED.has(props.name);
  return (
    <svg
      class={`icon ${props.class ?? ""}`}
      width="14"
      height="14"
      viewBox="0 0 14 14"
      fill="none"
      aria-hidden={props.title === undefined ? "true" : undefined}
      role={props.title === undefined ? undefined : "img"}
    >
      {props.title === undefined ? null : <title>{props.title}</title>}
      <path
        d={PATHS[props.name]}
        stroke={filled() ? "none" : (props.color ?? "currentColor")}
        fill={filled() ? (props.color ?? "currentColor") : "none"}
        stroke-width="var(--icon-stroke)"
        stroke-linecap="square"
        stroke-linejoin="miter"
      />
    </svg>
  );
}
