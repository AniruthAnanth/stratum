/**
 * Top bar, status bar and pane header — 06 §8.2, §14.2, §14.4.
 *
 * The top bar's centre-right is a **state readout**, not a toolbar. §14.2:
 * "`E41 · D17 · 12,481 obs` … is a thing no code editor has, and it is the first
 * thing you see." Everything else on the bar is subordinate to it, which is why
 * the run verbs are quiet buttons and the readout is the only element with a
 * fixed position in the middle.
 */

import { For, type JSX, Show } from "solid-js";
import { Button, Chip } from "./controls";

// ---------------------------------------------------------------------------
// The state readout
// ---------------------------------------------------------------------------

export interface StateReadout {
  /** The execution id of the last run, `E41`. */
  exec?: string;
  /** The dataset state, `D17` (spec §13). */
  dataset?: string;
  obs?: number;
  vars?: number;
  bytes?: number;
}

/** Thousands separators, because Stata's own `%8.0gc` has them and the eye needs them. */
const group = (n: number): string => n.toLocaleString("en-US");

const humanBytes = (bytes: number): string => {
  const units = ["B", "KB", "MB", "GB", "TB"];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit++;
  }
  return `${value < 10 && unit > 0 ? value.toFixed(1) : Math.round(value)} ${units[unit]}`;
};

export function StateReadoutView(props: { readout: StateReadout }): JSX.Element {
  const parts = (): string[] => {
    const r = props.readout;
    const out: string[] = [];
    if (r.exec !== undefined) out.push(r.exec);
    if (r.dataset !== undefined) out.push(r.dataset);
    if (r.obs !== undefined) out.push(`${group(r.obs)} obs`);
    if (r.vars !== undefined) out.push(`${group(r.vars)} vars`);
    if (r.bytes !== undefined) out.push(humanBytes(r.bytes));
    return out;
  };

  return (
    <div class="state-readout" aria-label="session state">
      <For each={parts()}>
        {(part, i) => (
          <>
            <Show when={i() > 0}>
              <span class="state-readout__sep">·</span>
            </Show>
            <span>{part}</span>
          </>
        )}
      </For>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Top bar
// ---------------------------------------------------------------------------

export interface TopBarProps {
  mode: "full" | "compact" | "auto-hide";
  revealed?: boolean;
  /** macOS ships `titleBarStyle: Overlay`, so the traffic lights sit in our bar. */
  trafficLightInset?: boolean;
  title?: string;
  readout: StateReadout;
  staleCount?: number;
  running?: boolean;
  onRun?: () => void;
  onRunFrom?: () => void;
  onRunStale?: () => void;
  onBreak?: () => void;
  trailing?: JSX.Element;
}

export function TopBar(props: TopBarProps): JSX.Element {
  return (
    <header
      class={`top-bar ${props.mode === "compact" ? "top-bar--compact" : ""} ${
        props.mode === "auto-hide" ? "top-bar--auto-hide" : ""
      }`}
      data-revealed={props.revealed === true ? "true" : "false"}
    >
      <Show when={props.trafficLightInset === true}>
        <div class="top-bar__traffic-inset" />
      </Show>

      <Show when={props.title !== undefined}>
        <span class="top-bar__title t-small w-medium">{props.title}</span>
      </Show>

      <Button variant="quiet" icon="run" onClick={() => props.onRun?.()}>
        Run block
      </Button>
      <Button variant="quiet" icon="run-from" onClick={() => props.onRunFrom?.()}>
        From here
      </Button>

      <Show when={(props.staleCount ?? 0) > 0}>
        <Button variant="quiet" icon="rerun" onClick={() => props.onRunStale?.()}>
          {`${props.staleCount} stale`}
        </Button>
      </Show>

      <Show when={props.running === true}>
        <Button variant="quiet" icon="stop" onClick={() => props.onBreak?.()}>
          Break
        </Button>
      </Show>

      <div class="top-bar__spacer" />
      <StateReadoutView readout={props.readout} />
      {props.trailing}
    </header>
  );
}

// ---------------------------------------------------------------------------
// Status bar
// ---------------------------------------------------------------------------

export interface StatusBarProps {
  cwd?: string;
  frame?: string;
  encoding?: string;
  caret?: { line: number; col: number };
  /** Segmentation timing, the 06 §15.1 budget made visible in dev builds. */
  segmentationMs?: number;
  logStatus?: string;
  /**
   * A half-entered chord — the first stroke of a binding like `Mod+K Mod+S`,
   * already rendered by the caller. Shown so a stuck prefix is never a mystery.
   * Named in keymap syntax rather than in platform glyphs: the glyphs are the
   * host's to spell (`keys/accelerator.ts`), and a doc comment that spells one
   * out is the second source of truth arriving early.
   */
  pendingChord?: string;
  notice?: string;
}

export function StatusBar(props: StatusBarProps): JSX.Element {
  return (
    // No `role="status"` on the bar itself: it would make Ln/Col a live region
    // and announce the caret on every keystroke. Only the notice slot announces
    // (06 §17: completion polite, errors assertive).
    <footer class="status-bar">
      <Show when={props.cwd !== undefined}>
        <span class="status-bar__cwd">{props.cwd}</span>
      </Show>
      <Show when={props.frame !== undefined}>
        <span>{props.frame}</span>
      </Show>
      <Show when={props.caret !== undefined}>
        <span>{`Ln ${props.caret?.line ?? 1}, Col ${props.caret?.col ?? 1}`}</span>
      </Show>
      <Show when={props.encoding !== undefined}>
        <span>{props.encoding}</span>
      </Show>
      <Show when={props.segmentationMs !== undefined}>
        <span title="segmentation, last transaction">{`seg ${props.segmentationMs?.toFixed(1)} ms`}</span>
      </Show>
      <div class="top-bar__spacer" />
      <Show when={props.pendingChord !== undefined}>
        <Chip tone="accent">{props.pendingChord}</Chip>
      </Show>
      <Show when={props.notice !== undefined}>
        <span aria-live="polite">
          <Chip tone="stale" icon="warn">
            {props.notice}
          </Chip>
        </span>
      </Show>
      <Show when={props.logStatus !== undefined}>
        <span>{props.logStatus}</span>
      </Show>
    </footer>
  );
}

// ---------------------------------------------------------------------------
// Pane header
// ---------------------------------------------------------------------------

export interface PaneHeaderProps {
  title: string;
  actions?: JSX.Element;
}

export function PaneHeader(props: PaneHeaderProps): JSX.Element {
  return (
    <div class="pane-header">
      <span class="pane-header__title">{props.title}</span>
      <div class="top-bar__spacer" />
      {props.actions}
    </div>
  );
}
