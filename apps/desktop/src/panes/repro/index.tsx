/**
 * The Reproducibility pane — spec §16; 03 §10.2; 06 §5.3.
 *
 * ```text
 * Reproducibility
 * ✓ File runs from clean state
 * ✓ Random seed defined
 * ✓ Inputs resolved
 * ✓ No hidden interactive dependencies
 * ⚠ 1 absolute file path
 * ```
 *
 * Five rows, always five, in that order. A row that disappeared when it was
 * clean would make "checked, nothing wrong" and "nobody checked" look the same,
 * which is the failure this whole panel exists to prevent in miniature.
 *
 * # The one rule
 *
 * **Deterministic checks power the panel; AI explains, and never marks.** Spec
 * §16's last sentence permits AI to explain issues; 03 §10.2 spells out the
 * boundary — "AI explains findings; it never produces them and never flips an
 * indicator". So every mark here comes from a `Tri` or a lint count in the
 * `ReproReport`, and the first row additionally demands the `ExecutionId` of the
 * clean run that proved it ({@link canTickRunsClean}). There is no code path in
 * this file that can draw a tick from anything else.
 *
 * The **Verify** action runs `run.fileClean`, which is `Isolation::Subprocess`
 * (03 §8: "the reproducibility report's Verify action always uses Subprocess,
 * because that is the claim being made"). `Isolation::InProcess` is the default
 * for the ordinary button and cannot tick this panel.
 */

import { For, type JSX, Show } from "solid-js";
import { render } from "solid-js/web";
import { CleanRunButton } from "../../components";
import { registerPane } from "../../dock/panes";
import { runCommand } from "../../keys/registry";
import { Icon, PaneHeader, Rule } from "../../ui";
import {
  type FindingView,
  type ReproReportView,
  type ReproRow,
  type RowMark,
  reproReport,
  reproRows,
} from "./store";

import "./repro.css";

/**
 * The mark's icon. Never colour alone (06 §17): `check`, `warn` and `error` are
 * three distinct shapes, and `unverified` is a hollow circle — the same shape
 * language as the `NeverRun` block glyph, which is exactly what it means here.
 */
const MARK_ICON: Readonly<Record<RowMark, "check" | "warn" | "error" | "circle">> = {
  ok: "check",
  warn: "warn",
  bad: "error",
  unverified: "circle",
};

const MARK_LABEL: Readonly<Record<RowMark, string>> = {
  ok: "yes",
  warn: "warning",
  bad: "no",
  unverified: "not verified",
};

function Row(props: { row: ReproRow }): JSX.Element {
  return (
    <li
      class="repro__row"
      data-repro-row={props.row.id}
      data-mark={props.row.mark}
      title={props.row.detail}
    >
      <span class="repro__mark" aria-hidden="true">
        <Icon name={MARK_ICON[props.row.mark]} />
      </span>
      {/* The mark is also on the accessible name, because the icon is the only
          thing distinguishing two otherwise identical rows. */}
      <span class="repro__label">
        <span class="repro__sr">{`${MARK_LABEL[props.row.mark]}: `}</span>
        {props.row.label}
      </span>
    </li>
  );
}

const SEVERITY_ICON = { error: "error", warning: "warn", note: "dot", help: "dot" } as const;

function Finding(props: {
  finding: FindingView;
  onOpen?: (finding: FindingView) => void;
}): JSX.Element {
  return (
    <li class="repro__finding" data-repro-finding={props.finding.lint}>
      <button
        type="button"
        class="repro__finding-button"
        onClick={() => props.onOpen?.(props.finding)}
        title={props.finding.detail ?? props.finding.message}
      >
        <span class="repro__mark" aria-hidden="true">
          <Icon name={SEVERITY_ICON[props.finding.severity]} />
        </span>
        <code class="repro__lint">{props.finding.lint}</code>
        <span class="repro__finding-title">{props.finding.title}</span>
        {/* A fix exists or it does not. It is never applied by looking at it:
            A15's gate is the only path that edits a document. */}
        <Show when={props.finding.fix}>
          <span class="repro__fix t-micro">fix available</span>
        </Show>
      </button>
    </li>
  );
}

export interface ReproPaneProps {
  /** Overrides the store, for previews and tests. */
  report?: ReproReportView;
  onOpenFinding?: (finding: FindingView) => void;
  onAction?: (command: string) => void;
}

export function ReproPane(props: ReproPaneProps): JSX.Element {
  const report = (): ReproReportView | undefined => props.report ?? reproReport();
  const rows = (): ReproRow[] => {
    const r = report();
    return r === undefined ? [] : reproRows(r);
  };
  const act = (command: string): void => {
    if (props.onAction === undefined) runCommand(command);
    else props.onAction(command);
  };

  return (
    <section class="repro" data-pane="repro">
      <PaneHeader
        title="Reproducibility"
        actions={
          <CleanRunButton {...(props.onAction === undefined ? {} : { onAction: props.onAction })} />
        }
      />

      <Show
        when={report() !== undefined}
        fallback={
          <p class="repro__idle t-small" data-repro-idle>
            No audit yet.
          </p>
        }
      >
        <ul class="repro__rows" data-repro-rows>
          <For each={rows()}>{(row) => <Row row={row} />}</For>
        </ul>

        <Show when={(report()?.findings.length ?? 0) > 0}>
          <Rule weight="hairline" />
          <ul class="repro__findings" data-repro-findings>
            <For each={report()?.findings ?? []}>
              {(finding) => (
                <Finding
                  finding={finding}
                  {...(props.onOpenFinding === undefined ? {} : { onOpen: props.onOpenFinding })}
                />
              )}
            </For>
          </ul>
        </Show>

        {/* Suppressions are LISTED (CONTRACTS §9) precisely so that
            `*! nolint(R001)` cannot quietly turn a warning into a clean panel. */}
        <Show when={(report()?.suppressed.length ?? 0) > 0}>
          <Rule weight="hairline" />
          <ul class="repro__suppressed" data-repro-suppressed>
            <For each={report()?.suppressed ?? []}>
              {([lint]) => (
                <li class="repro__finding t-small" data-repro-suppression={lint}>
                  <code class="repro__lint">{lint}</code>
                  <span class="repro__finding-title">suppressed</span>
                </li>
              )}
            </For>
          </ul>
        </Show>
      </Show>

      <div class="repro__foot">
        <button
          type="button"
          class="repro__verify"
          data-repro-verify
          data-exec-action="run.fileClean"
          // 03 §8: Verify is always Subprocess isolation, because that is the
          // claim being made. The ordinary run button is InProcess and cannot
          // tick the first row.
          title="Run this file in a fresh process from a clean state, and record the result"
          onClick={() => act("run.fileClean")}
        >
          Verify from clean state
        </button>
      </div>
    </section>
  );
}

/** Registers the pane with W12's dock. Returns the disposer. */
export function registerReproPane(): () => void {
  return registerPane("repro", (host, register) => {
    register(render(() => <ReproPane />, host));
  });
}
