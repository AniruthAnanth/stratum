/**
 * The block-state strip and the plan notices — spec §12, §13; 06 §5.2, §5.3.
 *
 * §12 names the exact failure this product exists to fix: *showing old output as
 * if it reflected current code*. The whole design of this file follows from
 * taking that literally.
 *
 * **A stale card keeps its numbers.** Deleting them would hide the very thing a
 * researcher needs to compare against. What changes is that the card stops
 * claiming to be current: a strip appears under the header, in state ink, that
 * says *which* upstream fact moved and *when*. "Downstream of a changed block"
 * is not actionable; "income was modified at E44" is.
 *
 * **Nothing here reruns anything on its own** (§13). Every action is a command
 * id the user presses. `Rerun` is a button, never a side effect of noticing.
 *
 * **All nine states get a sentence.** The strip is not a stale-only widget: a
 * `Broken` block (`✕!`) has to say that re-running would *error* rather than
 * merely differ, and a `CurrentUnverifiable` block (`✓⚠`) has to say that its
 * tick is qualified and why. Those two are where a status UI usually gives up
 * and shows a colour, and they are the two that matter most.
 */

import { For, type JSX, Show } from "solid-js";
import { runCommand } from "../keys/registry";
import type {
  BlockStatusView,
  BrokenReasonView,
  DepKeyView,
  RunPlanView,
  StaleReasonView,
} from "../state/exec";
import { TAINT, hasTaint, taintLabel, taintNames } from "../state/exec";
import { Button, StateGlyph } from "../ui";

import "./exec.css";

// ---------------------------------------------------------------------------
// Naming the thing that moved
// ---------------------------------------------------------------------------

/** `DepKey` → the short phrase CONTRACTS §3 promises the banner renders verbatim. */
export function depKeyLabel(key: DepKeyView): string {
  const qualified = (frame: string, name: string): string =>
    frame === "default" ? name : `${frame}.${name}`;
  switch (key.ns) {
    case "var":
      return qualified(key.frame, key.name);
    case "row_membership":
      return "the set of observations";
    case "row_order":
      return "the sort order";
    case "var_layout":
      return "the variable list";
    case "macro":
      return `macro ${key.name}`;
    case "scalar":
      return `scalar ${key.name}`;
    case "matrix":
      return `matrix ${key.name}`;
    case "program":
      return `program ${key.name}`;
    case "estimates":
      return "the stored estimates";
    case "r_class":
      return "the r() results";
    case "s_class":
      return "the s() results";
    case "rng":
      return "the random-number stream";
    case "setting":
      return `set ${key.name}`;
    case "cwd":
      return "the working directory";
    case "file":
      return key.path;
  }
}

const execLabel = (exec: number | null | undefined): string =>
  exec === null || exec === undefined ? "" : `E${String(exec)}`;

/** The clause after "Stale — ". 06 §5.2 gives two of these verbatim. */
export function staleBecause(reason: StaleReasonView, since: number | null): string {
  switch (reason.why) {
    case "code_changed": {
      const at = execLabel(since);
      return at === "" ? "code changed" : `code changed since ${at}`;
    }
    case "epoch_reset":
      return "the session was reset";
    case "input_changed": {
      const at = execLabel(reason.at);
      const what = depKeyLabel(reason.key);
      return at === "" ? `${what} was modified` : `${what} was modified at ${at}`;
    }
    case "file_changed":
      return `${reason.path} changed on disk`;
    case "upstream_pending":
      return `${depKeyLabel(reason.via)} comes from block ${String(reason.block)}, which has not run in its current form`;
    case "upstream_opaque":
      return `block ${String(reason.block)} above has not run, and we cannot rule out that it changes what this reads`;
    case "rng_shifted":
      return "the random-number stream moved";
  }
}

function brokenBecause(reason: BrokenReasonView): string {
  switch (reason.why) {
    case "unresolved_name":
      return `${reason.name} no longer resolves`;
    case "unknown_command":
      return `${reason.name} is not a command this build knows`;
    case "missing_file":
      return `${reason.path} is not there`;
  }
}

/** The deterministic quick fix a `Broken` block offers, when there is one. */
export function brokenFix(reason: BrokenReasonView): string | undefined {
  switch (reason.why) {
    case "unresolved_name":
    case "unknown_command":
      return reason.suggestion ?? undefined;
    case "missing_file":
      return undefined;
  }
}

// ---------------------------------------------------------------------------
// One description per state
// ---------------------------------------------------------------------------

export interface StateAction {
  /** A command id in `keys/registry.ts`. Never a handler. */
  readonly command: string;
  readonly label: string;
  readonly accent?: boolean;
}

export interface StatusDescription {
  /** "Stale", "Broken", "Current, unverifiable" … */
  readonly headline: string;
  /** The clause that names the specific cause. Empty when there is none. */
  readonly because: string;
  /** Longer prose for the tooltip / accessible description. */
  readonly detail?: string;
  readonly actions: readonly StateAction[];
  /** A deterministic edit the user can accept. `Broken` only. */
  readonly fix?: string;
}

const RERUN: StateAction = { command: "run.block", label: "Rerun" };
const FROM_HERE: StateAction = { command: "run.fromHere", label: "Run from here" };
const BREAK: StateAction = { command: "run.break", label: "Break" };

/**
 * Every one of the nine variants of CONTRACTS §3, with the payload spent.
 *
 * Exhaustive by construction: the switch has no `default`, so adding a tenth
 * variant to the generated union is a type error here rather than a block that
 * silently renders as a blank strip.
 */
export function describeStatus(status: BlockStatusView): StatusDescription {
  switch (status.state) {
    case "never_run":
      return {
        headline: "Never run",
        because: "",
        detail: "This block has not been executed in this session.",
        actions: [{ ...RERUN, label: "Run block" }],
      };
    case "queued":
      return {
        headline: "Queued",
        because: `position ${String(status.position + 1)} in the run`,
        actions: [BREAK],
      };
    case "running":
      return {
        headline: "Running",
        because: execLabel(status.exec),
        actions: [BREAK],
      };
    case "current":
      return {
        headline: "Current",
        because: `${execLabel(status.exec)} · D${String(status.dataset)}`,
        detail: "The engine has confirmed this output reflects the code and the state above it.",
        actions: [],
      };
    case "current_unverifiable": {
      const names = taintNames(status.taint);
      const causes = names.map(taintLabel);
      const external = hasTaint(status.taint, TAINT.EXTERNAL);
      return {
        headline: "Current, unverifiable",
        because: causes.length === 0 ? "unverifiable" : `used ${causes.join(", ")}`,
        // The sentence Taint::EXTERNAL earns. It is the difference between "we
        // checked and it is fine" and "we ran it and nothing complained".
        detail: external
          ? "This block ran cleanly, but it reached outside the engine — shell, Python, Java or a plugin — so we cannot prove that nothing changed underneath it. The tick is qualified rather than withheld: the run happened, the guarantee did not."
          : "This block ran cleanly, but part of what it did could not be tracked exactly, so its output cannot be proven current.",
        actions: [RERUN],
      };
    }
    case "stale":
      return {
        headline: "Stale",
        because: staleBecause(status.reason, status.since),
        detail:
          "The numbers below are the ones this block last produced. They are kept on purpose; nothing is rerun without you.",
        actions:
          status.reason.why === "code_changed"
            ? [RERUN, FROM_HERE, { command: "view.diffCode", label: "Diff code" }]
            : [RERUN, FROM_HERE, { command: "view.showWhatChanged", label: "Show what changed" }],
      };
    case "failed":
      return {
        headline: "Failed",
        because: `r(${String(status.rc)}) at ${execLabel(status.exec)}`,
        actions: [RERUN],
      };
    case "interrupted":
      return {
        headline: "Interrupted",
        // INV-2 made visible. "Rolled back" and "not rolled back" are completely
        // different situations for the dataset in memory and the user has to be
        // told which one happened.
        because: status.rolled_back
          ? "stopped; the dataset was rolled back"
          : "stopped; external effects were NOT rolled back",
        actions: [RERUN],
      };
    case "broken": {
      const fix = brokenFix(status.reason);
      return {
        headline: "Broken",
        because: brokenBecause(status.reason),
        // The distinction Broken exists for, spelled out. Stale means the
        // numbers would differ; Broken means there would be no numbers.
        detail: "Re-running this block would error, not merely produce different numbers.",
        actions: [RERUN],
        ...(fix === undefined ? {} : { fix }),
      };
    }
  }
}

// ---------------------------------------------------------------------------
// The strip
// ---------------------------------------------------------------------------

export interface StaleBannerProps {
  status: BlockStatusView;
  /** Runs a command id. Defaults to the shared registry (06 §5.4). */
  onAction?: (command: string) => void;
  /** Accepts the deterministic quick fix a `Broken` block offers. */
  onFix?: (suggestion: string) => void;
  /**
   * `true` when the block's last execution was part of a clean run (spec §15).
   * Neutral ink instead of teal — 06 §5.3.
   */
  clean?: boolean;
}

/**
 * The 20 px strip of 06 §5.2, generalised to every state.
 *
 * `<output>` rather than a `<p>`: it IS the semantic element — the displayed
 * result of the staleness computation — and it needs no ARIA role to say so.
 *
 * **`aria-live="off"`, deliberately.** `<output>` is a live region by default and
 * that default is wrong here. One edit can turn forty downstream blocks stale at
 * once, and forty polite announcements is not information, it is a wall a screen
 * reader user has to sit through before they can do anything. Per-block state is
 * *reference* information: the card is focusable (06 §17) and the strip is read
 * on arrival, which is when it is wanted. The one announcement worth making —
 * "three blocks are now stale" — is made once, by {@link StaleCountButton}, which
 * is a live region precisely because it is a single summary.
 */
export function StaleBanner(props: StaleBannerProps): JSX.Element {
  const d = (): StatusDescription => describeStatus(props.status);
  const act = (command: string): void => {
    if (props.onAction === undefined) runCommand(command);
    else props.onAction(command);
  };

  return (
    <output
      class="exec-banner"
      data-exec-banner
      data-state={props.status.state}
      data-clean={props.clean === true ? "" : undefined}
      aria-live="off"
      title={d().detail}
    >
      <span class="exec-banner__glyph" aria-hidden="true">
        <StateGlyph state={props.status.state} detail={d().because} />
      </span>
      <span class="exec-banner__headline" data-exec-headline>
        {d().headline}
      </span>
      <Show when={d().because !== ""}>
        <span class="exec-banner__dash" aria-hidden="true">
          —
        </span>
        <span class="exec-banner__because" data-exec-because>
          {d().because}
        </span>
      </Show>

      <Show when={d().fix}>
        {(fix) => (
          <button
            type="button"
            class="exec-banner__fix"
            data-exec-fix
            onClick={() => props.onFix?.(fix())}
          >
            {`Did you mean ${fix()}?`}
          </button>
        )}
      </Show>

      <span class="exec-banner__spacer" />

      <For each={d().actions}>
        {(action) => (
          <Button
            variant="quiet"
            class="exec-banner__action"
            data-exec-action={action.command}
            onClick={() => act(action.command)}
          >
            {action.label}
          </Button>
        )}
      </For>
    </output>
  );
}

// ---------------------------------------------------------------------------
// Plan-level notices (ARCHITECTURE §7.5)
// ---------------------------------------------------------------------------

const SKIP_LABEL: Readonly<Record<string, string>> = {
  unaffected: "unaffected",
  already_current: "already current",
  not_executable: "not executable",
};

export interface PlanNoticeProps {
  plan: RunPlanView | undefined;
  onAction?: (command: string) => void;
}

/**
 * "3 upstream blocks are stale — [Run them first]" and "12 blocks skipped —
 * unaffected".
 *
 * Both are **non-blocking**: running one block whose upstream is stale is a
 * legitimate request and ARCHITECTURE §7.5 says we honour it. Guessing is the
 * Jupyter failure mode inverted. The skipped line exists because silence about a
 * block the planner dropped feels like a bug — the count is reported even though
 * nothing went wrong.
 */
export function PlanNotice(props: PlanNoticeProps): JSX.Element {
  const upstream = (): number => props.plan?.stale_upstream.length ?? 0;
  const skipped = (): readonly (readonly [number, string])[] => props.plan?.skipped ?? [];
  const skippedBy = (): [string, number][] => {
    const counts = new Map<string, number>();
    for (const [, reason] of skipped()) counts.set(reason, (counts.get(reason) ?? 0) + 1);
    return [...counts.entries()].sort((a, b) => a[0].localeCompare(b[0]));
  };
  const act = (command: string): void => {
    if (props.onAction === undefined) runCommand(command);
    else props.onAction(command);
  };

  return (
    <Show when={upstream() > 0 || skipped().length > 0}>
      {/* Live: this one IS a summary of a whole run's plan, and it appears at
          most once per submission. */}
      <output class="exec-notice" data-exec-notice aria-live="polite">
        <Show when={upstream() > 0}>
          <span data-exec-upstream>
            {`${String(upstream())} upstream block${upstream() === 1 ? " is" : "s are"} stale`}
          </span>
          <Button
            variant="quiet"
            class="exec-notice__action"
            data-exec-action="run.allStale"
            onClick={() => act("run.allStale")}
          >
            Run them first
          </Button>
        </Show>
        <For each={skippedBy()}>
          {([reason, n]) => (
            <span data-exec-skipped={reason}>
              {`${String(n)} block${n === 1 ? "" : "s"} skipped — ${SKIP_LABEL[reason] ?? reason}`}
            </span>
          )}
        </For>
      </output>
    </Show>
  );
}
