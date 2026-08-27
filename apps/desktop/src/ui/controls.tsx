/**
 * The control primitives — 06 §14.4.
 *
 * Every one of these is 3 px radius, one hairline, no shadow and no gradient.
 * They exist so that the answer to "what does a button look like here" is a
 * file rather than a habit; §39's "Electron admin dashboard" is what a codebase
 * looks like when forty components each decided for themselves.
 */

import { For, Index, type JSX, splitProps } from "solid-js";
import { Icon, type IconName } from "./icons";

// ---------------------------------------------------------------------------
// Button
// ---------------------------------------------------------------------------

export type ButtonVariant = "quiet" | "default" | "accent";

export interface ButtonProps extends JSX.ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: ButtonVariant;
  icon?: IconName;
  /** Rendered from `menu_accelerator`, never composed in place. */
  accelerator?: string;
}

export function Button(props: ButtonProps): JSX.Element {
  const [own, rest] = splitProps(props, ["variant", "icon", "accelerator", "children", "class"]);
  return (
    <button
      type="button"
      class={`btn btn--${own.variant ?? "default"} ${own.class ?? ""}`}
      {...rest}
    >
      {own.icon === undefined ? null : <Icon name={own.icon} />}
      <span class="btn__label">{own.children}</span>
      {own.accelerator === undefined ? null : (
        <span class="btn__accel t-micro">{own.accelerator}</span>
      )}
    </button>
  );
}

// ---------------------------------------------------------------------------
// Chip
// ---------------------------------------------------------------------------

export type ChipTone = "neutral" | "ok" | "stale" | "failed" | "accent";

export interface ChipProps {
  tone?: ChipTone;
  /** A glyph as well as a colour: 06 §17 forbids colour as the only channel. */
  icon?: IconName;
  children: JSX.Element;
  title?: string;
}

export function Chip(props: ChipProps): JSX.Element {
  return (
    <span class={`chip chip--${props.tone ?? "neutral"}`} title={props.title}>
      {props.icon === undefined ? null : <Icon name={props.icon} />}
      {props.children}
    </span>
  );
}

// ---------------------------------------------------------------------------
// Field
// ---------------------------------------------------------------------------

export interface FieldProps extends JSX.InputHTMLAttributes<HTMLInputElement> {
  label?: string;
  /** The Stata prompt. `.` in meta ink is the whole affordance (06 §10). */
  prompt?: string;
}

export function Field(props: FieldProps): JSX.Element {
  const [own, rest] = splitProps(props, ["label", "prompt", "class"]);
  return (
    <label class={`field ${own.class ?? ""}`}>
      {own.label === undefined ? null : <span class="field__label t-micro">{own.label}</span>}
      <span class="field__box">
        {own.prompt === undefined ? null : <span class="field__prompt">{own.prompt}</span>}
        <input class="field__input" {...rest} />
      </span>
    </label>
  );
}

// ---------------------------------------------------------------------------
// Segmented
// ---------------------------------------------------------------------------

export interface SegmentedOption<T extends string> {
  value: T;
  label: string;
  icon?: IconName;
}

export interface SegmentedProps<T extends string> {
  options: readonly SegmentedOption<T>[];
  value: T;
  onChange: (value: T) => void;
  label: string;
}

export function Segmented<T extends string>(props: SegmentedProps<T>): JSX.Element {
  return (
    <div class="segmented" role="radiogroup" aria-label={props.label}>
      <For each={props.options}>
        {(option) => (
          <button
            type="button"
            // biome-ignore lint/a11y/useSemanticElements: `<input type="radio">` is void, so it cannot carry the icon and label a segment shows. The `input` + `label` pair that replaces it brings a UA control box that fights 06 §14.4's 22px row, and this button is keyboard-reachable and correctly announced as it stands.
            role="radio"
            aria-checked={props.value === option.value}
            class="segmented__item"
            data-selected={props.value === option.value ? "" : undefined}
            onClick={() => props.onChange(option.value)}
          >
            {option.icon === undefined ? null : <Icon name={option.icon} />}
            {option.label}
          </button>
        )}
      </For>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Rule
// ---------------------------------------------------------------------------

export interface RuleProps {
  /** `edge` is the booktabs top/bottom weight; `mid` is the interior one. */
  weight?: "edge" | "mid" | "hairline";
  vertical?: boolean;
}

export function Rule(props: RuleProps): JSX.Element {
  // `hr` rather than a div with `role="separator"`: it IS the semantic element,
  // it needs no role and no aria-orientation for the horizontal case, and CSS
  // resets its default border away in `ui.css`.
  return (
    <hr
      class={`rule rule--${props.weight ?? "hairline"}`}
      data-vertical={props.vertical === true ? "" : undefined}
      aria-orientation={props.vertical === true ? "vertical" : undefined}
    />
  );
}

// ---------------------------------------------------------------------------
// Sparkline
// ---------------------------------------------------------------------------

export interface SparklineProps {
  /** 06 §11.1: 24 bins. Values are counts; the component normalises. */
  bins: readonly number[];
  width?: number;
  height?: number;
  label: string;
}

/**
 * Hand-rolled inline SVG, per 06 §1: adding a chart library for a 24-bar
 * histogram would import a foreign visual language into the one place the
 * product's own language matters most.
 */
export function Sparkline(props: SparklineProps): JSX.Element {
  const width = (): number => props.width ?? 72;
  const height = (): number => props.height ?? 14;
  const max = (): number => Math.max(1, ...props.bins);
  const barWidth = (): number => width() / Math.max(1, props.bins.length);

  return (
    <svg
      class="sparkline"
      width={width()}
      height={height()}
      viewBox={`0 0 ${width()} ${height()}`}
      role="img"
      aria-label={props.label}
    >
      <Index each={props.bins}>
        {(value, i) => {
          const h = (): number => Math.max(1, Math.round((value() / max()) * height()));
          return (
            <rect
              x={i * barWidth()}
              y={height() - h()}
              width={Math.max(1, barWidth() - 1)}
              height={h()}
              fill="currentColor"
            />
          );
        }}
      </Index>
    </svg>
  );
}
