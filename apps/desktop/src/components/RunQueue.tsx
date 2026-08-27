/**
 * The run queue and the run verbs — spec §14; 06 §5.3, §5.4; ARCHITECTURE §7.5.
 *
 * A `RunPlan` is the engine's answer to "what did you take my keystroke to
 * mean", and until it is on screen the answer is a guess. `Mod+Alt+Enter` on a
 * file of forty blocks might run three of them or thirty-one; which it was
 * decides whether the next number the researcher reads is trustworthy.
 *
 * So this pane shows the whole plan, in execution order, including:
 *
 *  * **why** each block is in it — `Requested`, `DependencyOf`, `Stale`,
 *    `Prefix`. `DependencyOf` is the interesting one: it is the product saying
 *    "you asked for block 12, and block 7 writes something 12 reads".
 *  * **what it decided not to run**, with the reason. ARCHITECTURE §7.5:
 *    "Skipped blocks are reported … never silently dropped", because "12 blocks
 *    skipped — unaffected" is reassurance and silence is a bug report.
 *
 * Progress is a **hairline, never a spinner** (06 §14.6). A spinner is the "web
 * app" tell §39 rules out, and it carries less information: the hairline says
 * both "running" and "this far through the plan".
 */

import { For, type JSX, Show } from "solid-js";
import type { BlockId } from "../ipc/hand";
import { runCommand } from "../keys/registry";
import type { PlanReasonView, RunPlanView, SkipReasonView } from "../state/exec";
import { runPlan, runState, staleCount } from "../state/exec";
import { Button, PaneHeader, StateGlyph } from "../ui";
import { PlanNotice } from "./StaleBanner";

import "./exec.css";

const PLAN_REASON: Readonly<Record<PlanReasonView, string>> = {
  requested: "requested",
  dependency_of: "dependency",
  stale: "stale",
  prefix: "prefix",
};

const SKIP_REASON: Readonly<Record<SkipReasonView, string>> = {
  unaffected: "unaffected",
  already_current: "already current",
  not_executable: "not executable",
};

// ---------------------------------------------------------------------------
// The verbs (spec §14)
// ---------------------------------------------------------------------------

interface Verb {
  readonly command: string;
  readonly label: string;
}

/**
 * Spec §14, in its own order and its own words.
 *
 * These are command ids, resolved through the shared registry, so the button,
 * the palette entry, the native menu item and the keybinding are one thing (06
 * §5.4). `run.allStale` is included here as well as in the top bar because §14
 * lists it among the verbs a user should be able to reach deliberately, not only
 * by noticing a count.
 */
export const RUN_VERBS: readonly Verb[] = [
  { command: "run.fromHere", label: "Run from here" },
  { command: "run.above", label: "Run everything above" },
  { command: "run.toCursor", label: "Run to cursor" },
  { command: "run.section", label: "Run current section" },
  { command: "run.allStale", label: "Run all stale blocks" },
];

export interface RunVerbsProps {
  onAction?: (command: string) => void;
}

export function RunVerbs(props: RunVerbsProps): JSX.Element {
  const act = (command: string): void => {
    if (props.onAction === undefined) runCommand(command);
    else props.onAction(command);
  };
  return (
    <div class="run-verbs" data-run-verbs>
      <For each={RUN_VERBS}>
        {(verb) => (
          <Button
            variant="quiet"
            class="run-verbs__item"
            data-exec-action={verb.command}
            // `run.allStale` with nothing stale is a no-op the user should not
            // be invited to press; the others are always meaningful.
            disabled={verb.command === "run.allStale" && staleCount() === 0}
            onClick={() => act(verb.command)}
          >
            {verb.label}
          </Button>
        )}
      </For>
    </div>
  );
}

// ---------------------------------------------------------------------------
// The queue
// ---------------------------------------------------------------------------

export interface RunQueueProps {
  /** Overrides the store, for previews and tests. */
  plan?: RunPlanView;
  /** The first line of a block's code, when the host can resolve it. */
  labelOf?: (block: BlockId) => string | undefined;
  onAction?: (command: string) => void;
}

export function RunQueue(props: RunQueueProps): JSX.Element {
  const plan = (): RunPlanView | undefined => props.plan ?? runPlan();
  const run = (): ReturnType<typeof runState> => runState();
  const total = (): number => plan()?.items.length ?? 0;
  const done = (): number => Math.min(run().finished, total());
  const label = (block: BlockId): string => props.labelOf?.(block) ?? `Block ${String(block)}`;

  return (
    <section
      class="run-queue"
      data-pane="run-queue"
      data-clean={run().kind === "clean" ? "" : undefined}
    >
      <PaneHeader
        title="Run"
        actions={
          <Show when={run().active}>
            <Button
              variant="quiet"
              icon="stop"
              data-exec-action="run.break"
              onClick={() => {
                if (props.onAction === undefined) runCommand("run.break");
                else props.onAction("run.break");
              }}
            >
              Break
            </Button>
          </Show>
        }
      />

      <RunVerbs {...(props.onAction === undefined ? {} : { onAction: props.onAction })} />

      {/* 06 §14.6: a hairline that fills, not a spinner. `<progress>` because it
          IS the semantic element — same reasoning as `Rule`'s `<hr>` — so the
          bar needs no ARIA role and no tabindex to be announced correctly. It is
          styled down to one hairline in `exec.css`. Not a live region: a
          progressbar that announced every block would talk over the completion
          announcement the card makes (06 §17). */}
      <Show when={run().active && total() > 0}>
        <progress
          class="run-queue__progress"
          data-run-progress={`${String(done())}/${String(total())}`}
          value={done()}
          max={total()}
        >
          {`${String(done())} of ${String(total())} blocks`}
        </progress>
      </Show>

      <PlanNotice
        plan={plan()}
        {...(props.onAction === undefined ? {} : { onAction: props.onAction })}
      />

      <Show
        when={plan() !== undefined && total() > 0}
        fallback={
          // No apology, no illustration, no "get started". 06 §14.8's fourth
          // rule applies to every surface: one sentence, meta ink.
          <p class="run-queue__idle t-small" data-run-idle>
            Nothing queued.
          </p>
        }
      >
        <ol class="run-queue__list" data-run-list>
          <For each={plan()?.items ?? []}>
            {(item, index) => (
              <li
                class="run-queue__item"
                data-run-item={String(index())}
                data-reason={item.reason}
                data-run-state={
                  index() < done()
                    ? "done"
                    : index() === done() && run().active
                      ? "running"
                      : "queued"
                }
              >
                <span class="run-queue__glyph" aria-hidden="true">
                  <StateGlyph
                    state={
                      index() < done()
                        ? "current"
                        : index() === done() && run().active
                          ? "running"
                          : "queued"
                    }
                  />
                </span>
                <code class="run-queue__label">{label(item.block)}</code>
                <span class="run-queue__reason t-micro">{PLAN_REASON[item.reason]}</span>
              </li>
            )}
          </For>
        </ol>
      </Show>

      <Show when={(plan()?.skipped.length ?? 0) > 0}>
        <ol class="run-queue__skipped" data-run-skipped>
          <For each={plan()?.skipped ?? []}>
            {([block, reason]) => (
              <li class="run-queue__item" data-skip-reason={reason}>
                <span class="run-queue__glyph" aria-hidden="true">
                  <StateGlyph state="never_run" detail={SKIP_REASON[reason]} />
                </span>
                <code class="run-queue__label">{label(block)}</code>
                <span class="run-queue__reason t-micro">{`skipped — ${SKIP_REASON[reason]}`}</span>
              </li>
            )}
          </For>
        </ol>
      </Show>
    </section>
  );
}
