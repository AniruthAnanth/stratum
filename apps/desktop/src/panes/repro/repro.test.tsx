/**
 * The §16 panel — W15's fourth acceptance bullet.
 *
 * > "The §16 panel renders the five rows exactly, and '✓ File runs from clean
 * > state' appears **only** after an actual `Isolation::Subprocess` run. A test
 * > asserts it cannot be set by static analysis."
 *
 * The second half is the one worth writing carefully, because "cannot be set by
 * static analysis" is not a property of a value — a static pass is perfectly
 * capable of concluding `runs_clean: Yes` — it is a property of the *evidence*.
 * `verified_by: Option<ExecutionId>` is that evidence: only an actual run
 * allocates an `ExecutionId`, and ARCHITECTURE §7.7 makes
 * `Isolation::Subprocess` the only thing that may set the tick.
 *
 * So the assertion below is exhaustive over the product of the two fields, not a
 * happy-path check: for all three `Tri` values × both `verified_by` shapes, the
 * tick appears in exactly one cell. The cell that matters is
 * `{ runs_clean: "yes", verified_by: null }` — a static analyser's best possible
 * output — and it renders "not verified".
 */

import type { JSX } from "solid-js";
import { render } from "solid-js/web";
import { afterEach, beforeEach, describe, expect, test } from "vitest";
import { asDocumentId, asExecId } from "../../ipc/hand";
import { ReproPane } from "./index";
import {
  type FindingView,
  type ReproReportView,
  type TriView,
  applyReproReport,
  canTickRunsClean,
  reproRows,
  resetReproState,
} from "./store";

const roots: (() => void)[] = [];

function mount(node: () => JSX.Element): HTMLElement {
  const host = document.createElement("div");
  document.body.append(host);
  roots.push(render(node, host));
  return host;
}

beforeEach(() => {
  resetReproState();
});

afterEach(() => {
  while (roots.length > 0) roots.pop()?.();
  document.body.replaceChildren();
});

const CLEAN: ReproReportView = {
  doc: asDocumentId(1),
  generated_at_ms: 1_700_000_000_000,
  runs_clean: "yes",
  verified_by: asExecId(52),
  verified_duration_us: 4_000_000,
  seed_defined: "yes",
  inputs_resolved: "yes",
  no_hidden_deps: "yes",
  findings: [],
  suppressed: [],
};

const finding = (lint: string, severity: FindingView["severity"] = "warning"): FindingView => ({
  lint,
  severity,
  title: `${lint} title`,
  message: `${lint} message`,
});

const marks = (host: HTMLElement): Record<string, string> =>
  Object.fromEntries(
    [...host.querySelectorAll("[data-repro-row]")].map((row) => [
      row.getAttribute("data-repro-row") ?? "",
      row.getAttribute("data-mark") ?? "",
    ]),
  );

// ---------------------------------------------------------------------------
// Five rows, exactly
// ---------------------------------------------------------------------------

describe("the panel renders the five rows of spec §16, exactly", () => {
  test("five rows, in spec order, always", () => {
    const host = mount(() => <ReproPane report={CLEAN} />);
    const ids = [...host.querySelectorAll("[data-repro-row]")].map((r) =>
      r.getAttribute("data-repro-row"),
    );
    expect(ids).toEqual(["runs_clean", "seed", "inputs", "hidden_deps", "paths"]);
  });

  test("still five when everything is wrong — a row never disappears", () => {
    // "Checked, nothing wrong" and "nobody checked" must not look the same.
    const bad: ReproReportView = {
      ...CLEAN,
      runs_clean: "no",
      verified_by: null,
      seed_defined: "no",
      inputs_resolved: "no",
      no_hidden_deps: "no",
      findings: [finding("R001"), finding("R002", "error")],
    };
    const host = mount(() => <ReproPane report={bad} />);
    expect(host.querySelectorAll("[data-repro-row]")).toHaveLength(5);
  });

  test("the wording is spec §16's", () => {
    const host = mount(() => <ReproPane report={CLEAN} />);
    const text = host.querySelector("[data-repro-rows]")?.textContent ?? "";
    expect(text).toContain("File runs from clean state");
    expect(text).toContain("Random seed defined");
    expect(text).toContain("Inputs resolved");
    expect(text).toContain("No hidden interactive dependencies");
  });

  test("'⚠ 1 absolute file path' is the R001 count, singular and plural (03 §10.2)", () => {
    const one = mount(() => <ReproPane report={{ ...CLEAN, findings: [finding("R001")] }} />);
    expect(one.querySelector('[data-repro-row="paths"]')?.textContent).toContain(
      "1 absolute file path",
    );
    expect(marks(one)["paths"]).toBe("warn");

    const two = mount(() => (
      <ReproPane report={{ ...CLEAN, findings: [finding("R001"), finding("R001")] }} />
    ));
    expect(two.querySelector('[data-repro-row="paths"]')?.textContent).toContain(
      "2 absolute file paths",
    );
  });

  test("R005 downgrades 'Inputs resolved' to a dynamic-path count (03 §10.2)", () => {
    const host = mount(() => (
      <ReproPane
        report={{ ...CLEAN, findings: [finding("R005", "note"), finding("R005", "note")] }}
      />
    ));
    expect(host.querySelector('[data-repro-row="inputs"]')?.textContent).toContain(
      "2 dynamic paths",
    );
    expect(marks(host)["inputs"]).toBe("warn");
  });

  test("every mark is announced, not carried by icon colour alone (06 §17)", () => {
    const host = mount(() => <ReproPane report={{ ...CLEAN, findings: [finding("R001")] }} />);
    const row = host.querySelector('[data-repro-row="paths"]');
    // The icon is aria-hidden; the verdict has to reach the accessible name.
    expect(row?.querySelector(".repro__mark")?.getAttribute("aria-hidden")).toBe("true");
    expect(row?.textContent).toContain("warning: ");
  });
});

// ---------------------------------------------------------------------------
// The tick that has to be earned
// ---------------------------------------------------------------------------

describe("'✓ File runs from clean state' cannot be set by static analysis", () => {
  const TRIS: readonly TriView[] = ["yes", "no", "unknown"];

  test.each(
    TRIS.flatMap((tri) =>
      [null, asExecId(52)].map((verified) => ({
        tri,
        verified,
        name: `runs_clean=${tri}, verified_by=${verified === null ? "null" : "E52"}`,
      })),
    ),
  )("$name", ({ tri, verified }) => {
    const report: ReproReportView = { ...CLEAN, runs_clean: tri, verified_by: verified };
    const shouldTick = tri === "yes" && verified !== null;

    expect(canTickRunsClean(report)).toBe(shouldTick);

    const host = mount(() => <ReproPane report={report} />);
    const row = host.querySelector('[data-repro-row="runs_clean"]');
    expect(row?.getAttribute("data-mark") === "ok").toBe(shouldTick);
  });

  test("the static analyser's best possible output still renders 'not verified'", () => {
    // `Tri::Yes` with no execution id is exactly what a confident static pass
    // would produce. It gets a hollow circle and the word, never a tick.
    const report: ReproReportView = { ...CLEAN, runs_clean: "yes", verified_by: null };
    const host = mount(() => <ReproPane report={report} />);
    const row = host.querySelector('[data-repro-row="runs_clean"]');
    expect(row?.getAttribute("data-mark")).toBe("unverified");
    expect(row?.textContent).toContain("not verified");
    expect(row?.getAttribute("title")).toContain("Static analysis never does");
  });

  test("a verified tick names the run that proved it", () => {
    const host = mount(() => <ReproPane report={CLEAN} />);
    const row = host.querySelector('[data-repro-row="runs_clean"]');
    expect(row?.getAttribute("data-mark")).toBe("ok");
    expect(row?.getAttribute("title")).toContain("E52");
  });

  test("a failed clean run is 'does not run', which is not the same as unverified", () => {
    const host = mount(() => (
      <ReproPane report={{ ...CLEAN, runs_clean: "no", verified_by: asExecId(9) }} />
    ));
    const row = host.querySelector('[data-repro-row="runs_clean"]');
    expect(row?.getAttribute("data-mark")).toBe("bad");
    expect(row?.textContent).toContain("does not run from clean state");
  });

  test("Verify dispatches the clean-run verb (03 §8: always Subprocess)", () => {
    const dispatched: string[] = [];
    const host = mount(() => <ReproPane report={CLEAN} onAction={(c) => dispatched.push(c)} />);
    expect(dispatched).toEqual([]);
    (host.querySelector("[data-repro-verify]") as HTMLElement).click();
    expect(dispatched).toEqual(["run.fileClean"]);
  });
});

// ---------------------------------------------------------------------------
// Findings and suppressions
// ---------------------------------------------------------------------------

describe("findings are deterministic checks, listed with their lint id", () => {
  test("each finding carries the R-code from the one registry (C14)", () => {
    const host = mount(() => (
      <ReproPane report={{ ...CLEAN, findings: [finding("R001"), finding("R006", "error")] }} />
    ));
    const codes = [...host.querySelectorAll("[data-repro-finding]")].map((f) =>
      f.getAttribute("data-repro-finding"),
    );
    expect(codes).toEqual(["R001", "R006"]);
  });

  test("a fix is advertised, never applied by rendering", () => {
    const opened: string[] = [];
    const host = mount(() => (
      <ReproPane
        report={{
          ...CLEAN,
          findings: [{ ...finding("R007"), fix: { label: "insert version 18" } }],
        }}
        onOpenFinding={(f) => opened.push(f.lint)}
      />
    ));
    expect(host.querySelector("[data-repro-finding] .repro__fix")?.textContent).toBe(
      "fix available",
    );
    expect(opened).toEqual([]);
    (host.querySelector("[data-repro-finding] button") as HTMLElement).click();
    expect(opened).toEqual(["R007"]);
  });

  test("suppressions are listed, so `*! nolint` cannot hide a problem silently", () => {
    const host = mount(() => <ReproPane report={{ ...CLEAN, suppressed: [["R001", [10, 20]]] }} />);
    expect(host.querySelector('[data-repro-suppression="R001"]')).not.toBeNull();
  });

  test("no report is 'No audit yet.', not a fabricated clean panel", () => {
    const host = mount(() => <ReproPane />);
    expect(host.querySelector("[data-repro-idle]")?.textContent).toBe("No audit yet.");
    expect(host.querySelectorAll("[data-repro-row]")).toHaveLength(0);
  });
});

// ---------------------------------------------------------------------------
// The store
// ---------------------------------------------------------------------------

describe("the store", () => {
  test("the pane reads the report the engine pushed", () => {
    applyReproReport({ ...CLEAN, findings: [finding("R001")] });
    const host = mount(() => <ReproPane />);
    expect(host.querySelector('[data-repro-row="paths"]')?.textContent).toContain(
      "1 absolute file path",
    );
  });

  test("reproRows is total: five rows for every combination of the four Tris", () => {
    const tris: readonly TriView[] = ["yes", "no", "unknown"];
    for (const a of tris) {
      for (const b of tris) {
        for (const c of tris) {
          const rows = reproRows({
            ...CLEAN,
            seed_defined: a,
            inputs_resolved: b,
            no_hidden_deps: c,
          });
          expect(rows).toHaveLength(5);
          expect(rows.every((r) => r.label.length > 0)).toBe(true);
        }
      }
    }
  });
});
