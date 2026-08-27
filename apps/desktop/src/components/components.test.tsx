/**
 * The execution-state components — W15's second and third acceptance bullets.
 *
 * > "All nine `BlockStatus` variants render, including `CurrentUnverifiable`
 * > (✓⚠ with a tooltip explaining `Taint::EXTERNAL`) and `Broken` (✕! with the
 * > quick fix)."
 *
 * > "Clean runs are visually unmistakable: neutral-ink glyphs and a `CLEAN` chip
 * > in the top bar for the duration. Confusing an interactive run with a clean
 * > run is the most expensive mistake in this product."
 *
 * The clean-run bullet is asserted on both of its halves, because only one of
 * them is observable in jsdom. The chip and the `data-clean` scope are DOM and
 * are asserted as DOM; the neutral ink is a stylesheet rule, and vitest does not
 * process the CSS import, so the rule is asserted against the stylesheet source
 * instead of against a `getComputedStyle` that would return the empty string and
 * pass. A test that cannot see the thing it claims to check is worse than no
 * test, so it says which half it is looking at.
 */

import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import type { JSX } from "solid-js";
import { render } from "solid-js/web";
import { afterEach, beforeEach, describe, expect, test } from "vitest";
import { asBlockId, asDatasetStateId, asExecId, asRunId, codeHash } from "../ipc/hand";
import type { BlockStatusState } from "../ipc/hand";
import {
  type BlockStatusView,
  type RunPlanView,
  TAINT,
  applyExecEvent,
  resetExecCounters,
  resetExecState,
} from "../state/exec";
import { CleanChip, CleanRunButton, CleanScope } from "./CleanChip";
import { RUN_VERBS, RunQueue, RunVerbs } from "./RunQueue";
import { PlanNotice, StaleBanner, describeStatus } from "./StaleBanner";
import { StaleCountButton, StateReadout, readoutSentence } from "./StateReadout";

const roots: (() => void)[] = [];

function mount(node: () => JSX.Element): HTMLElement {
  const host = document.createElement("div");
  document.body.append(host);
  roots.push(render(node, host));
  return host;
}

beforeEach(() => {
  resetExecState();
  resetExecCounters();
});

afterEach(() => {
  while (roots.length > 0) roots.pop()?.();
  document.body.replaceChildren();
});

const HASH = codeHash("c".repeat(32));

// ---------------------------------------------------------------------------
// All nine
// ---------------------------------------------------------------------------

const NINE: Readonly<Record<BlockStatusState, BlockStatusView>> = {
  never_run: { state: "never_run" },
  queued: { state: "queued", position: 2 },
  running: { state: "running", exec: asExecId(41), started_ms: 1 },
  current: {
    state: "current",
    exec: asExecId(41),
    dataset: asDatasetStateId(17),
    duration_us: 80_000,
  },
  current_unverifiable: {
    state: "current_unverifiable",
    exec: asExecId(42),
    dataset: asDatasetStateId(17),
    duration_us: 900,
    taint: TAINT.EXTERNAL,
  },
  stale: { state: "stale", reason: { why: "code_changed" }, since: asExecId(41) },
  failed: { state: "failed", exec: asExecId(43), rc: 111 },
  interrupted: { state: "interrupted", exec: asExecId(44), rolled_back: true },
  broken: {
    state: "broken",
    reason: { why: "unresolved_name", name: "incme", suggestion: "income" },
  },
};

const ALL_NINE = Object.keys(NINE) as BlockStatusState[];

describe("all nine BlockStatus variants render", () => {
  test("the fixture is the whole union, not a selection", () => {
    expect(ALL_NINE).toHaveLength(9);
  });

  test.each(ALL_NINE)("%s renders a strip with its own glyph and a headline", (state) => {
    const host = mount(() => <StaleBanner status={NINE[state]} />);
    const banner = host.querySelector("[data-exec-banner]");
    expect(banner).not.toBeNull();
    expect(banner?.getAttribute("data-state")).toBe(state);

    // 06 §17: the glyph shape is the non-colour channel, and it is per-state.
    const glyph = host.querySelector(".state-glyph");
    expect(glyph?.getAttribute("data-state")).toBe(state);

    const headline = host.querySelector("[data-exec-headline]")?.textContent ?? "";
    expect(headline.length).toBeGreaterThan(0);
  });

  test("no two states produce the same headline+cause sentence", () => {
    // A status UI that renders nine states as three sentences has nine states in
    // the type system and three in the product.
    const sentences = ALL_NINE.map((state) => {
      const d = describeStatus(NINE[state]);
      return `${d.headline} — ${d.because}`;
    });
    expect(new Set(sentences).size).toBe(9);
  });

  test("CurrentUnverifiable (✓⚠) explains Taint::EXTERNAL in its tooltip", () => {
    const host = mount(() => <StaleBanner status={NINE.current_unverifiable} />);
    const banner = host.querySelector("[data-exec-banner]");
    const title = banner?.getAttribute("title") ?? "";

    // The claim the tooltip has to make: it RAN, and we cannot PROVE it.
    expect(title).toMatch(/ran cleanly/i);
    expect(title).toMatch(/cannot prove/i);
    expect(title).toMatch(/shell, Python, Java or a plugin/i);

    // And the cause is named on the face of the strip, not only in the tooltip.
    expect(host.querySelector("[data-exec-because]")?.textContent).toContain(
      "shell, Python, Java or a plugin",
    );
  });

  test("a CurrentUnverifiable carrying an unnamed taint bit still says it is unverifiable", () => {
    const host = mount(() => (
      <StaleBanner
        status={{
          state: "current_unverifiable",
          exec: asExecId(1),
          dataset: asDatasetStateId(1),
          duration_us: 1,
          taint: 1 << 12,
        }}
      />
    ));
    expect(host.querySelector("[data-exec-headline]")?.textContent).toBe("Current, unverifiable");
    expect(host.querySelector(".state-glyph")?.getAttribute("data-state")).toBe(
      "current_unverifiable",
    );
  });

  test("Broken (✕!) offers the quick fix, and offering is all it does", () => {
    const applied: string[] = [];
    const host = mount(() => <StaleBanner status={NINE.broken} onFix={(s) => applied.push(s)} />);

    const fix = host.querySelector("[data-exec-fix]");
    expect(fix?.textContent).toBe("Did you mean income?");
    // Nothing is edited by rendering. A15: the write gate is the only path that
    // changes a document, and it is reached by a click.
    expect(applied).toEqual([]);

    (fix as HTMLButtonElement).click();
    expect(applied).toEqual(["income"]);
  });

  test("Broken says re-running would ERROR, which is what separates it from Stale", () => {
    const host = mount(() => <StaleBanner status={NINE.broken} />);
    expect(host.querySelector("[data-exec-banner]")?.getAttribute("title")).toMatch(
      /would error, not merely produce different numbers/i,
    );
  });

  test("a MissingFile Broken has no fix button, because there is no deterministic edit", () => {
    const host = mount(() => (
      <StaleBanner status={{ state: "broken", reason: { why: "missing_file", path: "a.dta" } }} />
    ));
    expect(host.querySelector("[data-exec-fix]")).toBeNull();
    expect(host.querySelector("[data-exec-because]")?.textContent).toContain("a.dta");
  });

  test("Interrupted distinguishes rolled back from NOT rolled back (INV-2)", () => {
    const back = mount(() => <StaleBanner status={NINE.interrupted} />);
    expect(back.querySelector("[data-exec-because]")?.textContent).toContain("rolled back");

    const not = mount(() => (
      <StaleBanner status={{ state: "interrupted", exec: asExecId(1), rolled_back: false }} />
    ));
    expect(not.querySelector("[data-exec-because]")?.textContent).toContain("NOT rolled back");
  });
});

// ---------------------------------------------------------------------------
// Stale: the sentence, and the refusal to act on its own
// ---------------------------------------------------------------------------

describe("a stale block names what moved (spec §13, 06 §5.2)", () => {
  test("code changed since E41 — 06 §5.2 verbatim", () => {
    const host = mount(() => <StaleBanner status={NINE.stale} />);
    expect(host.querySelector("[data-exec-because]")?.textContent).toBe("code changed since E41");
  });

  test("income was modified at E44 — 06 §5.2's other example, verbatim", () => {
    const host = mount(() => (
      <StaleBanner
        status={{
          state: "stale",
          reason: {
            why: "input_changed",
            key: { ns: "var", frame: "default", name: "income" },
            at: asExecId(44),
          },
          since: asExecId(41),
        }}
      />
    ));
    expect(host.querySelector("[data-exec-because]")?.textContent).toBe(
      "income was modified at E44",
    );
  });

  test("a non-default frame is qualified, because two frames can hold the same name", () => {
    const host = mount(() => (
      <StaleBanner
        status={{
          state: "stale",
          reason: {
            why: "input_changed",
            key: { ns: "var", frame: "wide", name: "income" },
            at: null,
          },
          since: null,
        }}
      />
    ));
    expect(host.querySelector("[data-exec-because]")?.textContent).toBe("wide.income was modified");
  });

  test("nothing runs on render; the actions are commands the user presses (§13)", () => {
    const dispatched: string[] = [];
    const host = mount(() => (
      <StaleBanner status={NINE.stale} onAction={(c) => dispatched.push(c)} />
    ));
    expect(dispatched).toEqual([]);

    const buttons = [...host.querySelectorAll("[data-exec-action]")].map((b) =>
      b.getAttribute("data-exec-action"),
    );
    // 06 §5.2: Rerun, Run from here, Diff code.
    expect(buttons).toEqual(["run.block", "run.fromHere", "view.diffCode"]);

    (host.querySelector('[data-exec-action="run.fromHere"]') as HTMLElement).click();
    expect(dispatched).toEqual(["run.fromHere"]);
  });

  test("an upstream-caused stale offers 'Show what changed' instead of 'Diff code'", () => {
    const host = mount(() => (
      <StaleBanner
        status={{
          state: "stale",
          reason: { why: "upstream_opaque", block: asBlockId(3) },
          since: null,
        }}
      />
    ));
    const buttons = [...host.querySelectorAll("[data-exec-action]")].map((b) =>
      b.getAttribute("data-exec-action"),
    );
    expect(buttons).toContain("view.showWhatChanged");
    expect(buttons).not.toContain("view.diffCode");
  });
});

// ---------------------------------------------------------------------------
// Clean runs (spec §15)
// ---------------------------------------------------------------------------

describe("clean runs are unmistakable (spec §15, 06 §5.3)", () => {
  const runStarted = (clean: boolean): void => {
    applyExecEvent({
      event: "run_started",
      run: asRunId(1),
      clean_state: clean,
      plan_len: 2,
      started_at_ms: 1,
      seed: 123_456_789,
      source: "analysis.do",
    });
  };

  test("an interactive run shows NO chip", () => {
    runStarted(false);
    const host = mount(() => <CleanChip />);
    expect(host.querySelector("[data-clean-chip]")).toBeNull();
  });

  test("a clean run shows the CLEAN chip, for the duration and no longer", () => {
    runStarted(true);
    const host = mount(() => <CleanChip />);
    const chip = host.querySelector("[data-clean-chip]");
    expect(chip?.textContent).toContain("CLEAN");
    // The tooltip names the fresh environment: "clean" is a claim about a
    // specific seed and entry point, not an adjective.
    expect(chip?.getAttribute("title")).toContain("123456789");
    expect(chip?.getAttribute("title")).toContain("analysis.do");

    applyExecEvent({
      event: "run_finished",
      run: asRunId(1),
      blocks_run: 2,
      blocks_failed: 0,
      duration_us: 1,
    });
    expect(host.querySelector("[data-clean-chip]")).toBeNull();
  });

  test("the readout carries the mode, so the ids are read in the right frame", () => {
    runStarted(true);
    const host = mount(() => <StateReadout />);
    expect(host.querySelector("[data-exec-readout]")?.getAttribute("data-mode")).toBe("clean");
    // The chip precedes the ids in document order — a qualifier that trails the
    // number is a qualifier the eye reaches second.
    const children = [...(host.querySelector("[data-exec-readout]")?.children ?? [])];
    expect(children.findIndex((c) => c.hasAttribute("data-clean-chip"))).toBe(0);
    expect(children.findIndex((c) => c.querySelector(".state-readout") !== null)).toBe(1);
  });

  test("the readout spells spec §13's sentence, minus the hash it will not fake", () => {
    expect(readoutSentence({ exec: "E41", dataset: "D17", obs: 12_481 }, "R41")).toBe(
      "Execution 41 / Dataset state: D17 / Result: R41 / 12,481 observations",
    );
    // No execution yet is a sentence too, not an empty string.
    expect(readoutSentence({}, undefined)).toBe("No execution yet");
  });

  test("CleanScope marks the subtree, which is what the neutral-ink rule keys on", () => {
    const host = mount(() => (
      <CleanScope clean={true}>
        <StaleBanner status={NINE.current} />
      </CleanScope>
    ));
    expect(host.querySelector("[data-clean]")).not.toBeNull();

    const interactive = mount(() => (
      <CleanScope clean={false}>
        <StaleBanner status={NINE.current} />
      </CleanScope>
    ));
    expect(interactive.querySelector("[data-clean]")).toBeNull();
  });

  test("the stylesheet turns every glyph in a clean scope to neutral ink", () => {
    // The half jsdom cannot see. Asserted against the source because vitest does
    // not process the CSS import and `getComputedStyle` would agree with
    // anything. Both channels of the mechanism are checked: the attribute above,
    // the rule here.
    const here = dirname(fileURLToPath(import.meta.url));
    const css = readFileSync(join(here, "exec.css"), "utf8");
    expect(css).toContain('[data-clean] .state-glyph path:not([stroke="none"])');
    expect(css).toContain('[data-clean] .state-glyph path:not([fill="none"])');
    // Neutral ink, from a token. Never teal, never a hex.
    const rule = css.slice(css.indexOf("[data-clean] .state-glyph"));
    expect(rule.slice(0, 300)).toContain("var(--text-body)");
    expect(rule.slice(0, 300)).not.toContain("--accent");
  });

  test("this unit's stylesheets re-declare no colour", () => {
    const here = dirname(fileURLToPath(import.meta.url));
    for (const path of [join(here, "exec.css"), join(here, "..", "panes", "repro", "repro.css")]) {
      expect(readFileSync(path, "utf8")).not.toMatch(/#[0-9a-fA-F]{3,8}\b/);
    }
  });

  test("'Run do-file from clean state' is a labelled button, not a buried verb (§15)", () => {
    const dispatched: string[] = [];
    const host = mount(() => <CleanRunButton onAction={(c) => dispatched.push(c)} />);
    const button = host.querySelector("[data-clean-run]");
    expect(button?.textContent).toContain("Run from clean state");
    expect(button?.getAttribute("data-exec-action")).toBe("run.fileClean");
    (button as HTMLElement).click();
    expect(dispatched).toEqual(["run.fileClean"]);
  });

  test("the project-scoped variant dispatches run.entryPoint (A23)", () => {
    const dispatched: string[] = [];
    const host = mount(() => <CleanRunButton entryPoint onAction={(c) => dispatched.push(c)} />);
    (host.querySelector("[data-clean-run]") as HTMLElement).click();
    expect(dispatched).toEqual(["run.entryPoint"]);
  });
});

// ---------------------------------------------------------------------------
// The run verbs and the queue (spec §14, ARCHITECTURE §7.5)
// ---------------------------------------------------------------------------

describe("spec §14's verbs are all reachable", () => {
  test("the five §14 verbs, by command id", () => {
    expect(RUN_VERBS.map((v) => v.command)).toEqual([
      "run.fromHere",
      "run.above",
      "run.toCursor",
      "run.section",
      "run.allStale",
    ]);
  });

  test("each is a button carrying its command id", () => {
    const dispatched: string[] = [];
    const host = mount(() => <RunVerbs onAction={(c) => dispatched.push(c)} />);
    for (const verb of RUN_VERBS) {
      const button = host.querySelector(`[data-exec-action="${verb.command}"]`);
      expect(button, verb.command).not.toBeNull();
      expect(button?.textContent).toContain(verb.label);
    }
    (host.querySelector('[data-exec-action="run.toCursor"]') as HTMLElement).click();
    expect(dispatched).toEqual(["run.toCursor"]);
  });

  test("Run all stale blocks is disabled while nothing is stale", () => {
    const host = mount(() => <RunVerbs />);
    const button = host.querySelector('[data-exec-action="run.allStale"]');
    expect((button as HTMLButtonElement).disabled).toBe(true);
  });
});

describe("the run queue reports the plan, including what it did not run", () => {
  const PLAN: RunPlanView = {
    run: asRunId(4),
    items: [
      { block: asBlockId(1), span: [0, 10], code_hash: HASH, reason: "requested" },
      { block: asBlockId(2), span: [11, 30], code_hash: HASH, reason: "dependency_of" },
      { block: asBlockId(3), span: [31, 40], code_hash: HASH, reason: "stale" },
    ],
    epoch_reset: false,
    clean_state: false,
    skipped: [
      [asBlockId(4), "unaffected"],
      [asBlockId(5), "unaffected"],
      [asBlockId(6), "already_current"],
    ],
    stale_upstream: [asBlockId(0), asBlockId(9), asBlockId(11)],
  };

  test("items appear in execution order with their reason", () => {
    const host = mount(() => <RunQueue plan={PLAN} />);
    const items = [...host.querySelectorAll("[data-run-item]")];
    expect(items).toHaveLength(3);
    expect(items.map((i) => i.getAttribute("data-reason"))).toEqual([
      "requested",
      "dependency_of",
      "stale",
    ]);
  });

  test("skipped blocks are reported, never silently dropped (ARCHITECTURE §7.5)", () => {
    const host = mount(() => <RunQueue plan={PLAN} />);
    const skipped = host.querySelector("[data-run-skipped]");
    expect(skipped?.children).toHaveLength(3);
    expect(skipped?.textContent).toContain("skipped — unaffected");
    expect(skipped?.textContent).toContain("skipped — already current");
  });

  test("'3 upstream blocks are stale — [Run them first]', non-blocking", () => {
    const dispatched: string[] = [];
    const host = mount(() => <PlanNotice plan={PLAN} onAction={(c) => dispatched.push(c)} />);
    expect(host.querySelector("[data-exec-upstream]")?.textContent).toBe(
      "3 upstream blocks are stale",
    );
    // Nothing happens until it is pressed. §13: we never auto-rerun.
    expect(dispatched).toEqual([]);
    (host.querySelector('[data-exec-action="run.allStale"]') as HTMLElement).click();
    expect(dispatched).toEqual(["run.allStale"]);
  });

  test("the skipped counts are rolled up per reason", () => {
    const host = mount(() => <PlanNotice plan={PLAN} />);
    expect(host.querySelector('[data-exec-skipped="unaffected"]')?.textContent).toBe(
      "2 blocks skipped — unaffected",
    );
    expect(host.querySelector('[data-exec-skipped="already_current"]')?.textContent).toBe(
      "1 block skipped — already current",
    );
  });

  test("no plan is an empty queue and one sentence, not an empty state illustration", () => {
    const host = mount(() => <RunQueue />);
    expect(host.querySelector("[data-run-idle]")?.textContent).toBe("Nothing queued.");
    expect(host.querySelector("[data-run-list]")).toBeNull();
  });

  test("the progress indicator is a <progress>, and there is no spinner anywhere", () => {
    applyExecEvent({
      event: "run_started",
      run: asRunId(4),
      clean_state: false,
      plan_len: 3,
      started_at_ms: 1,
    });
    const host = mount(() => <RunQueue plan={PLAN} />);
    const bar = host.querySelector("[data-run-progress]");
    expect(bar?.tagName.toLowerCase()).toBe("progress");
    expect(bar?.getAttribute("max")).toBe("3");

    // 06 §14.6, checked rather than promised. Comments are stripped first —
    // the sentence "no spinner anywhere in the product" is in this stylesheet
    // and matching it would make the check pass for the wrong reason. What a
    // spinner actually needs is a keyframe animation, so that is what is
    // forbidden.
    const here = dirname(fileURLToPath(import.meta.url));
    const css = readFileSync(join(here, "exec.css"), "utf8").replace(/\/\*[\s\S]*?\*\//g, "");
    expect(css).not.toMatch(/@keyframes|animation\s*:/i);
  });
});

// ---------------------------------------------------------------------------
// The count
// ---------------------------------------------------------------------------

describe("the ⟲ n stale affordance (06 §5.3)", () => {
  test("the summary is the ONE live region; the per-block strips are silent", () => {
    // One edit can turn forty downstream blocks stale. Forty polite
    // announcements is a wall; one summary is information.
    const count = mount(() => <StaleCountButton count={0} />);
    const live = count.querySelector('[aria-live="polite"]');
    expect(live).not.toBeNull();
    // Present at zero, so the transition to three has a region to announce in.
    expect(live?.querySelector("[data-exec-stale-count]")).toBeNull();

    const banner = mount(() => <StaleBanner status={NINE.stale} />);
    expect(banner.querySelector("[data-exec-banner]")?.getAttribute("aria-live")).toBe("off");
  });

  test("absent at zero, present above it, and it dispatches run.allStale", () => {
    const zero = mount(() => <StaleCountButton count={0} />);
    expect(zero.querySelector("[data-exec-stale-count]")).toBeNull();

    const dispatched: string[] = [];
    const host = mount(() => <StaleCountButton count={3} onAction={(c) => dispatched.push(c)} />);
    const button = host.querySelector("[data-exec-stale-count]");
    expect(button?.textContent).toContain("3 stale");
    (button as HTMLElement).click();
    expect(dispatched).toEqual(["run.allStale"]);
  });
});
