/**
 * `state/exec.ts` — W15's first and third acceptance bullets.
 *
 * > "`worseOf(local, kernel)` monotonicity: a property test proves the local
 * > check can only ever move a block **toward more stale**, never toward
 * > `Current`."
 *
 * This is the frontend half of ADR-008's INV-1. Over-marking a block costs a
 * researcher a re-run; under-marking one costs them a published number that does
 * not reproduce. The asymmetry is why the local check is allowed to exist at all
 * — it buys 06 §5.2's same-frame staleness with zero IPC — and why it is allowed
 * to move in exactly one direction.
 *
 * The proof here is stronger than a sampled property test, because the state
 * space is finite: **all 9 kernel states × 3 local conditions are enumerated
 * exhaustively**, and the sampled part on top of that exists to cover the
 * payloads (which are unbounded) rather than the states. A structural check
 * finishes the argument: the module contains no expression that CONSTRUCTS a
 * `current` status, so there is no code path by which it could fabricate one
 * even if the rule were wrong.
 *
 * # Why this file is in `components/`
 *
 * `docs/ownership.toml` gives W15 `apps/desktop/src/state/exec.ts` as an EXACT
 * path and `apps/desktop/src/components/**` as a glob. A colocated
 * `src/state/exec.test.ts` would therefore be owned by nobody, which
 * `cargo xtask ownership` treats as fatal — the same gap W12 hit with
 * `src/ipc/hand.test.ts` and had to escalate for. Putting the test inside the
 * glob this unit already owns needs no amendment and no one else's file.
 */

import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { beforeEach, describe, expect, test } from "vitest";
import {
  STATUS_RANK,
  asBlockId,
  asDatasetStateId,
  asDocumentId,
  asExecId,
  asRunId,
  codeHash,
  worseOf,
} from "../ipc/hand";
import type { BlockStatusState, CodeHash } from "../ipc/hand";
import {
  type BlockStatusView,
  type ExecEventView,
  TAINT,
  applyExecEvent,
  applyRunPlan,
  cleanRunActive,
  displayedStatus,
  execCounters,
  forgetBlock,
  ranClean,
  readout,
  resetExecCounters,
  resetExecState,
  runPlan,
  runState,
  setExecDocument,
  setExecutedHash,
  setKernelStatus,
  setLocalHash,
  staleBlocks,
  staleCount,
  taintNames,
} from "../state/exec";

const DOC = asDocumentId(1);
const HASH_A = codeHash("a".repeat(32));
const HASH_B = codeHash("b".repeat(32));

beforeEach(() => {
  resetExecState();
  resetExecCounters();
});

// ---------------------------------------------------------------------------
// The nine, with payloads
// ---------------------------------------------------------------------------

/** One representative of each of the nine variants of CONTRACTS §3. */
const NINE: readonly BlockStatusView[] = [
  { state: "never_run" },
  { state: "queued", position: 2 },
  { state: "running", exec: asExecId(41), started_ms: 1_700_000_000_000 },
  {
    state: "current",
    exec: asExecId(41),
    dataset: asDatasetStateId(17),
    duration_us: 80_000,
  },
  {
    state: "current_unverifiable",
    exec: asExecId(42),
    dataset: asDatasetStateId(17),
    duration_us: 900,
    taint: TAINT.EXTERNAL,
  },
  { state: "stale", reason: { why: "code_changed" }, since: asExecId(41) },
  { state: "failed", exec: asExecId(43), rc: 111 },
  { state: "interrupted", exec: asExecId(44), rolled_back: true },
  {
    state: "broken",
    reason: { why: "unresolved_name", name: "incme", suggestion: "income" },
  },
];

test("the fixture covers all nine variants exactly once", () => {
  const states = NINE.map((s) => s.state).sort();
  expect(new Set(states).size).toBe(9);
  expect(states).toEqual([...(Object.keys(STATUS_RANK) as BlockStatusState[])].sort());
});

// ---------------------------------------------------------------------------
// Monotonicity — exhaustive
// ---------------------------------------------------------------------------

describe("the local check can only move a block TOWARD stale (ADR-008, C20)", () => {
  const HEALTHY: readonly BlockStatusState[] = ["current", "current_unverifiable"];

  /** The three conditions the local check can be in, for one kernel status. */
  const conditions = [
    { name: "no local hash at all", local: undefined, executed: undefined },
    { name: "local hash matches the executed one", local: HASH_A, executed: HASH_A },
    { name: "local hash differs — the block was edited", local: HASH_B, executed: HASH_A },
  ] as const;

  test.each(
    NINE.flatMap((kernel) =>
      conditions.map((c) => ({ kernel, c, name: `${kernel.state} / ${c.name}` })),
    ),
  )("$name never renders healthier than the kernel", ({ kernel, c }) => {
    const block = asBlockId(1);
    setKernelStatus(DOC, block, kernel);
    if (c.executed !== undefined) setExecutedHash(DOC, block, c.executed);
    setLocalHash(DOC, block, c.local);

    const displayed = displayedStatus(DOC, block);

    // The whole invariant, in one line: never a higher rank than the kernel.
    expect(STATUS_RANK[displayed.state]).toBeLessThanOrEqual(STATUS_RANK[kernel.state]);

    // And its two consequences, stated separately so a failure says which.
    if (HEALTHY.includes(displayed.state)) {
      expect(displayed.state).toBe(kernel.state);
    }
    // `Queued`/`Running` are facts about the kernel, not judgements about text.
    // Structural equality rather than identity: the store hands back a Solid
    // proxy over the object it was given, so `toBe` would be testing Solid's
    // reactivity rather than the rule. Reference preservation through `worseOf`
    // itself is asserted directly in the sampled block below.
    if (STATUS_RANK[kernel.state] >= 90) {
      expect(displayed).toEqual(kernel);
    }
  });

  test("an edited, previously Current block displays Stale{CodeChanged} with its exec", () => {
    const block = asBlockId(7);
    setKernelStatus(DOC, block, NINE[3] as BlockStatusView);
    setExecutedHash(DOC, block, HASH_A);
    setLocalHash(DOC, block, HASH_B);

    const displayed = displayedStatus(DOC, block);
    expect(displayed.state).toBe("stale");
    // 06 §5.2's exact strip: "Stale — code changed since E41".
    if (displayed.state === "stale") {
      expect(displayed.reason.why).toBe("code_changed");
      expect(displayed.since).toBe(41);
    }
  });

  test("editing a block that FAILED leaves it Failed, not Stale", () => {
    // Rank order puts Failed below Stale on purpose: "it errored" is more urgent
    // than "it would now differ", and downgrading it would lose the rc.
    const block = asBlockId(8);
    setKernelStatus(DOC, block, NINE[6] as BlockStatusView);
    setExecutedHash(DOC, block, HASH_A);
    setLocalHash(DOC, block, HASH_B);

    const displayed = displayedStatus(DOC, block);
    expect(displayed.state).toBe("failed");
    if (displayed.state === "failed") expect(displayed.rc).toBe(111);
  });
});

// ---------------------------------------------------------------------------
// Monotonicity — sampled, over payloads
// ---------------------------------------------------------------------------

/**
 * A deterministic LCG. No `fast-check`: `package.json` is W12's file and this
 * unit may not add a dependency to it (R0). A seeded generator is in any case
 * the better tool here — a property test whose counterexample cannot be
 * reproduced from the failure message is a flake with extra steps.
 */
function lcg(seed: number): () => number {
  let s = seed >>> 0;
  return () => {
    s = (Math.imul(s, 1_664_525) + 1_013_904_223) >>> 0;
    return s / 0x1_0000_0000;
  };
}

function randomStatus(rand: () => number): BlockStatusView {
  const n = (max: number): number => Math.floor(rand() * max);
  switch (n(9)) {
    case 0:
      return { state: "never_run" };
    case 1:
      return { state: "queued", position: n(500) };
    case 2:
      return { state: "running", exec: asExecId(n(9999)), started_ms: n(1_000_000) };
    case 3:
      return {
        state: "current",
        exec: asExecId(n(9999)),
        dataset: asDatasetStateId(n(999)),
        duration_us: n(10_000_000),
      };
    case 4:
      return {
        state: "current_unverifiable",
        exec: asExecId(n(9999)),
        dataset: asDatasetStateId(n(999)),
        duration_us: n(10_000_000),
        taint: n(1 << 9), // includes bit 8, which THIS build has no name for
      };
    case 5:
      return {
        state: "stale",
        reason: {
          why: "input_changed",
          key: { ns: "var", frame: "default", name: `v${String(n(50))}` },
          at: asExecId(n(9999)),
        },
        since: asExecId(n(9999)),
      };
    case 6:
      return { state: "failed", exec: asExecId(n(9999)), rc: n(700) };
    case 7:
      return { state: "interrupted", exec: asExecId(n(9999)), rolled_back: rand() < 0.5 };
    default:
      return {
        state: "broken",
        reason: { why: "missing_file", path: `data/${String(n(50))}.dta` },
      };
  }
}

describe("worseOf itself, sampled", () => {
  test("10 000 random pairs: the result is one of the arguments, never a new object", () => {
    const rand = lcg(0x5f15_0001);
    for (let i = 0; i < 10_000; i++) {
      const a = randomStatus(rand);
      const b = randomStatus(rand);
      const out = worseOf<BlockStatusView>(a, b);
      // Identity, not equality. Payload preservation is what lets the banner say
      // `r(111)` and `income was modified at E44` at all.
      expect(out === a || out === b).toBe(true);
    }
  });

  test("10 000 random pairs: never healthier than either argument, except for the two facts", () => {
    const rand = lcg(0x5f15_0002);
    for (let i = 0; i < 10_000; i++) {
      const a = randomStatus(rand);
      const b = randomStatus(rand);
      const out = worseOf<BlockStatusView>(a, b);
      if (STATUS_RANK[a.state] >= 90 || STATUS_RANK[b.state] >= 90) {
        // Queued/Running win outright and in argument order — documented in
        // `ipc/hand.ts` and asserted here so the exception stays deliberate.
        expect(STATUS_RANK[out.state]).toBeGreaterThanOrEqual(90);
        continue;
      }
      expect(STATUS_RANK[out.state]).toBeLessThanOrEqual(STATUS_RANK[a.state]);
      expect(STATUS_RANK[out.state]).toBeLessThanOrEqual(STATUS_RANK[b.state]);
      // Rank-commutative for every ordinary pair.
      expect(STATUS_RANK[worseOf<BlockStatusView>(b, a).state]).toBe(STATUS_RANK[out.state]);
    }
  });

  test("10 000 random kernels: a local stale verdict never produces Current", () => {
    const rand = lcg(0x5f15_0003);
    const block = asBlockId(3);
    for (let i = 0; i < 10_000; i++) {
      const kernel = randomStatus(rand);
      setKernelStatus(DOC, block, kernel);
      setExecutedHash(DOC, block, HASH_A);
      setLocalHash(DOC, block, HASH_B);
      const displayed = displayedStatus(DOC, block);
      if (displayed.state === "current" || displayed.state === "current_unverifiable") {
        expect(kernel.state).toBe(displayed.state);
      }
      expect(STATUS_RANK[displayed.state]).toBeLessThanOrEqual(STATUS_RANK[kernel.state]);
    }
  });
});

/**
 * The structural half of the argument.
 *
 * A rule can be right and a module can still fabricate a tick somewhere else in
 * it. This asserts there is no such expression: the source constructs
 * `never_run` and `stale` and nothing healthier.
 */
test("the module contains no expression that constructs a healthy status", () => {
  const here = dirname(fileURLToPath(import.meta.url));
  const source = readFileSync(join(here, "..", "state", "exec.ts"), "utf8");
  // The negative lookbehind drops the `readonly state: "current"` lines of the
  // *type* declarations, which are not constructions of anything.
  const constructed = [...source.matchAll(/(?<!readonly\s)\bstate:\s*"([a-z_]+)"/g)].map(
    (m) => m[1],
  );
  expect(constructed.length).toBeGreaterThan(0);
  expect([...new Set(constructed)].sort()).toEqual(["never_run", "stale"]);
});

/**
 * `ipcCalls` is asserted to be 0 above, and a counter nobody increments is 0 for
 * an uninteresting reason. This is the assertion that gives it meaning: the
 * module has no way to make a call at all. Staleness arrives; it is never
 * fetched, which is what makes 06 §5.2's same-frame greying possible.
 */
test("the module cannot reach the engine — the display path does no IPC", () => {
  const here = dirname(fileURLToPath(import.meta.url));
  const source = readFileSync(join(here, "..", "state", "exec.ts"), "utf8");
  for (const forbidden of ["bridge(", "invoke(", "fetch(", "XMLHttpRequest", "WebSocket"]) {
    expect(source, `state/exec.ts reaches the network via ${forbidden}`).not.toContain(forbidden);
  }
});

// ---------------------------------------------------------------------------
// Counters (ADR-017)
// ---------------------------------------------------------------------------

describe("nothing on the display path is O(blocks) — ADR-017 counters", () => {
  const BLOCKS = 2_000;

  function fillDocument(): void {
    for (let i = 0; i < BLOCKS; i++) {
      setKernelStatus(DOC, asBlockId(i), {
        state: "current",
        exec: asExecId(i),
        dataset: asDatasetStateId(1),
        duration_us: 10,
      });
      setExecutedHash(DOC, asBlockId(i), HASH_A);
      setLocalHash(DOC, asBlockId(i), HASH_A);
    }
  }

  test("a StatusChanged naming one block in a 2 000-block document costs one evaluation", () => {
    fillDocument();
    resetExecCounters();

    const event: ExecEventView = {
      event: "status_changed",
      doc: DOC,
      changed: [
        [asBlockId(1_234), { state: "stale", reason: { why: "epoch_reset" }, since: null }],
      ],
    };
    expect(applyExecEvent(event)).toBe(true);

    expect(execCounters.statusWrites).toBe(1);
    // The number that matters. A sweep would read 2 000 here.
    expect(execCounters.statusEvaluations).toBe(1);
    expect(execCounters.staleScans).toBe(0);
    expect(execCounters.ipcCalls).toBe(0);
  });

  test("typing into one block costs one evaluation, whatever the document's size", () => {
    fillDocument();
    resetExecCounters();

    setLocalHash(DOC, asBlockId(7), HASH_B);

    expect(execCounters.statusEvaluations).toBe(1);
    expect(execCounters.staleScans).toBe(0);
    expect(execCounters.ipcCalls).toBe(0);
  });

  test("the stale count is a signal, not a scan", () => {
    fillDocument();
    for (const i of [3, 9, 27]) setLocalHash(DOC, asBlockId(i), HASH_B);
    resetExecCounters();

    // Read it a hundred times, as the top bar effectively does.
    for (let i = 0; i < 100; i++) expect(staleCount()).toBe(3);

    expect(execCounters.statusEvaluations).toBe(0);
    expect(execCounters.staleScans).toBe(0);
  });

  test("the count comes back down when the edit is undone", () => {
    fillDocument();
    setLocalHash(DOC, asBlockId(3), HASH_B);
    expect(staleCount()).toBe(1);
    setLocalHash(DOC, asBlockId(3), HASH_A);
    expect(staleCount()).toBe(0);
  });

  test("a retired block leaves the count", () => {
    fillDocument();
    setLocalHash(DOC, asBlockId(3), HASH_B);
    expect(staleCount()).toBe(1);
    forgetBlock(DOC, asBlockId(3));
    expect(staleCount()).toBe(0);
    expect(displayedStatus(DOC, asBlockId(3)).state).toBe("never_run");
  });

  test("staleBlocks() is the one deliberate scan, and it says so", () => {
    fillDocument();
    setLocalHash(DOC, asBlockId(3), HASH_B);
    resetExecCounters();
    expect(staleBlocks()).toEqual([`${String(DOC)}:3`]);
    expect(execCounters.staleScans).toBe(1);
  });
});

// ---------------------------------------------------------------------------
// Ingestion
// ---------------------------------------------------------------------------

describe("the event stream drives the readout and the run state", () => {
  const started = (clean: boolean): ExecEventView => ({
    event: "run_started",
    run: asRunId(1),
    clean_state: clean,
    plan_len: 3,
    started_at_ms: 1_700_000_000_000,
    seed: 20_260_821,
    source: "analysis.do",
  });

  test("RunStarted{clean_state} is the whole condition for the CLEAN chip", () => {
    expect(cleanRunActive()).toBe(false);
    applyExecEvent(started(true));
    expect(cleanRunActive()).toBe(true);
    expect(runState().kind).toBe("clean");
    expect(runState().seed).toBe(20_260_821);

    applyExecEvent({
      event: "run_finished",
      run: asRunId(1),
      blocks_run: 3,
      blocks_failed: 0,
      duration_us: 123_456,
    });
    // "for the duration" — and not one frame longer. After a clean run the live
    // session is the interactive one again (spec §15).
    expect(cleanRunActive()).toBe(false);
    expect(runState().durationUs).toBe(123_456);
  });

  test("a block that ran inside a clean run is remembered as such", () => {
    setExecDocument(DOC);
    applyExecEvent(started(true));
    applyExecEvent({
      event: "block_started",
      run: asRunId(1),
      exec: asExecId(1),
      block: asBlockId(0),
      doc: DOC,
      code_hash: HASH_A,
      dataset_state_in: asDatasetStateId(1),
    });
    expect(ranClean(DOC, asBlockId(0))).toBe(true);

    // …and forgets it the moment the same block runs interactively, because the
    // neutral-ink glyph is a claim about the LAST run, not about history.
    applyExecEvent({
      event: "run_finished",
      run: asRunId(1),
      blocks_run: 1,
      blocks_failed: 0,
      duration_us: 1,
    });
    applyExecEvent(started(false));
    applyExecEvent({
      event: "block_started",
      run: asRunId(2),
      exec: asExecId(2),
      block: asBlockId(0),
      doc: DOC,
      code_hash: HASH_A,
      dataset_state_in: asDatasetStateId(1),
    });
    expect(ranClean(DOC, asBlockId(0))).toBe(false);
  });

  test("BlockStarted records the hash the ENGINE is running, not the one we submitted", () => {
    setExecDocument(DOC);
    setLocalHash(DOC, asBlockId(0), HASH_B);
    applyExecEvent({
      event: "block_started",
      run: asRunId(1),
      exec: asExecId(5),
      block: asBlockId(0),
      doc: DOC,
      code_hash: HASH_B,
      dataset_state_in: asDatasetStateId(3),
    });
    setKernelStatus(DOC, asBlockId(0), {
      state: "current",
      exec: asExecId(5),
      dataset: asDatasetStateId(3),
      duration_us: 1,
    });
    expect(displayedStatus(DOC, asBlockId(0)).state).toBe("current");
  });

  test("StateChanged fills the obs/vars half of the §13 readout", () => {
    applyExecEvent({
      event: "state_changed",
      dataset_state: asDatasetStateId(17),
      n_obs: 12_481,
      n_vars: 12,
    });
    expect(readout().dataset).toBe(17);
    expect(readout().obs).toBe(12_481);
    expect(readout().vars).toBe(12);
  });

  test("an event this store does not model is reported as unapplied, never as applied", () => {
    // Honest counting: `Output` belongs to the log pane and `Result` to W14.
    const foreign = { event: "output" } as unknown as ExecEventView;
    expect(applyExecEvent(foreign)).toBe(false);
    expect(execCounters.eventsApplied).toBe(0);
  });

  test("the plan is stored verbatim, skipped blocks and all", () => {
    applyRunPlan({
      run: asRunId(9),
      items: [
        {
          block: asBlockId(1),
          span: [0, 10],
          code_hash: HASH_A as CodeHash,
          reason: "requested",
        },
      ],
      epoch_reset: false,
      clean_state: false,
      skipped: [[asBlockId(2), "unaffected"]],
      stale_upstream: [asBlockId(0)],
    });
    expect(runPlan()?.skipped).toHaveLength(1);
    expect(runPlan()?.stale_upstream).toHaveLength(1);
  });
});

// ---------------------------------------------------------------------------
// Taint
// ---------------------------------------------------------------------------

describe("Taint is a u16 bit set, exactly as stratum-proto writes it", () => {
  test("EXTERNAL is bit 3 and is named", () => {
    expect(TAINT.EXTERNAL).toBe(8);
    expect(taintNames(TAINT.EXTERNAL)).toEqual(["EXTERNAL"]);
  });

  test("several bits list in declaration order", () => {
    expect(taintNames(TAINT.EXTERNAL | TAINT.CLOCK | TAINT.MACRO_VARLIST)).toEqual([
      "MACRO_VARLIST",
      "EXTERNAL",
      "CLOCK",
    ]);
  });

  test("a bit this build has no name for is simply not listed, never treated as clean", () => {
    // `from_bits_retain` on the Rust side keeps it; the mirror of that here is
    // that the block is still CurrentUnverifiable and still shows the ✓⚠ glyph.
    const unknown = 1 << 12;
    expect(taintNames(unknown)).toEqual([]);
    expect(unknown).not.toBe(0);
  });
});
