/**
 * The typed renderers themselves — spec §17's four named cards plus the
 * fallbacks, driven by StataMP 18.5's own numbers where the mock provides them.
 */

import { render } from "solid-js/web";
import { afterEach, describe, expect, test } from "vitest";
import { envelopeOf, payloadOfEveryKind, scenarioAEnvelopes } from "./fixtures";
import { decimalPad, decimalPlaces } from "./readout";
import { ResultCard } from "./registry";
import { deltaChips } from "./table";
import { MAX_CELLS, shownCells } from "./tabulate";
import type {
  DataChangedPayloadView,
  ResultEnvelopeView,
  TabulatePayloadView,
  TermView,
} from "./types";

const roots: (() => void)[] = [];

function mount(node: () => ReturnType<typeof ResultCard>): HTMLElement {
  const host = document.createElement("div");
  document.body.append(host);
  roots.push(render(node, host));
  return host;
}

afterEach(() => {
  while (roots.length > 0) roots.pop()?.();
  document.body.replaceChildren();
});

const KINDS = payloadOfEveryKind();
const mock = scenarioAEnvelopes();

describe("summarize (§17, 06 §6.3)", () => {
  test("prints StataMP's own strings, and only those", () => {
    const host = mount(() => <ResultCard envelope={mock[1] as ResultEnvelopeView} />);
    const price = host.querySelector('[data-summarize-row="price"]');
    const cells = [...(price?.querySelectorAll("td") ?? [])].map((td) => td.textContent);
    expect(cells.slice(0, 6)).toEqual([
      "pricePrice",
      "74",
      "6165.257",
      "2949.496",
      "3291",
      "15906",
    ]);
    // The raw f64 6165.256756756757 must appear nowhere on the card.
    expect(host.textContent).not.toContain("6165.2567");
    expect(host.textContent).not.toContain("2949.4958");
  });

  test("the sparkline is drawn from the payload's own 24 bins", () => {
    const host = mount(() => <ResultCard envelope={mock[1] as ResultEnvelopeView} />);
    const bars = host.querySelectorAll('[data-summarize-row="price"] svg rect');
    expect(bars).toHaveLength(24);
  });

  test("a non-zero missing count is shown, in its own element", () => {
    const host = mount(() => <ResultCard envelope={envelopeOf(KINDS.summarize)} />);
    expect(host.querySelector("[data-summarize-missing]")?.textContent).toBe(" +3 missing");
  });

  test("decimal alignment comes from the Stata format, never from the value", () => {
    expect(decimalPlaces("%8.0gc")).toBe(0);
    expect(decimalPlaces("%9.2f")).toBe(2);
    expect(decimalPlaces("%-12s")).toBe(0);
    expect(decimalPlaces("%12.4f")).toBe(4);
    expect(decimalPad(["%12.4f", "%9.2f", "%8.0gc"])).toEqual([0, 2, 4]);
  });
});

describe("regress (§17, 06 §6.4)", () => {
  const regress = () => mount(() => <ResultCard envelope={mock[2] as ResultEnvelopeView} />);

  test("the coefficient table is `display_num`, in `display_num` order", () => {
    const host = regress();
    const row = host.querySelector('[data-term="weight"]');
    const cells = [...(row?.querySelectorAll("td") ?? [])].map((td) => td.textContent);
    expect(cells[0]).toBe("weight");
    expect(cells.slice(1, 7)).toEqual([
      "3.464706",
      ".630749",
      "5.49",
      "0.000",
      "2.206717",
      "4.722695",
    ]);
    // `.630749` and not `0.630749`: Stata's own leading-dot convention survives
    // because nothing here re-formats it.
    expect(host.textContent).toContain(".630749");
    expect(host.textContent).not.toContain("0.630749");
  });

  test("no significance stars on a bare regress (06 §6.4)", () => {
    expect(regress().textContent).not.toContain("*");
  });

  test("the model strip shows what the payload can express exactly, and no more", () => {
    const host = regress();
    const strip = host.querySelector("[data-estimation-strip]");
    expect(strip?.querySelector('[data-stat="N"]')?.textContent).toBe("N74");
    expect(strip?.querySelector('[data-stat="df"]')?.textContent).toBe("df3 / 70");
    // F, Prob>F, R², Adj R² and Root MSE have no display string on the wire
    // (asserted in `fixture.test.ts`), so the strip omits them rather than
    // inventing digits. The escalation is at the head of `estimation/index.tsx`.
    expect(strip?.querySelector('[data-stat="R²"]')).toBeNull();
    expect(host.textContent).not.toContain("23.29");
  });

  test("a sibling `Scalars` payload supplies the strip's missing statistics", () => {
    const base = mock[2] as ResultEnvelopeView;
    const envelope: ResultEnvelopeView = {
      ...base,
      payloads: [
        ...base.payloads,
        {
          kind: "scalars",
          values: [
            ["F", { t: "num", value: 23.2899, display: "23.29" }],
            ["r2", { t: "num", value: 0.4996, display: "0.4996" }],
          ],
        },
      ],
    };
    const host = mount(() => <ResultCard envelope={envelope} />);
    expect(host.querySelector('[data-stat="F"]')?.textContent).toBe("F23.29");
    expect(host.querySelector('[data-stat="R²"]')?.textContent).toBe("R²0.4996");
    // And it is consumed, not drawn twice.
    expect(host.querySelector('[data-payload="scalars"]')).toBeNull();
  });

  test("Sources ▸ is collapsed by default and holds the whole ANOVA block", () => {
    const host = regress();
    expect(host.querySelector("[data-estimation-anova]")).toBeNull();
    host.querySelector<HTMLButtonElement>("[data-estimation-sources-toggle]")?.click();
    const anova = host.querySelector("[data-estimation-anova]");
    const cells = [...(anova?.querySelectorAll("td") ?? [])].map((td) => td.textContent);
    expect(cells).toEqual([
      "Model",
      "317252881",
      "3",
      "105750960",
      "Residual",
      "317812515",
      "70",
      "4540178.78",
      "Total",
      "635065396",
      "73",
      "8699525.97",
    ]);
  });

  test("the CI strip is geometry, and never prints a number", () => {
    const host = regress();
    const svg = host.querySelector('[data-term="mpg"] svg');
    expect(svg?.querySelectorAll("line")).toHaveLength(2);
    expect(svg?.textContent).toBe("");
    expect(svg?.getAttribute("aria-label")).toBe("95% interval -126.1758 to 169.883");
  });

  test("omitted and base levels render as Stata renders them", () => {
    const base = mock[2] as ResultEnvelopeView;
    const estimation = base.payloads[0];
    if (estimation?.kind !== "estimation") throw new Error("expected an estimation payload");
    const envelope: ResultEnvelopeView = {
      ...base,
      payloads: [
        {
          ...estimation,
          terms: [
            { ...(estimation.terms[0] as TermView), name: "age2", display: "age2", omitted: true },
            {
              ...(estimation.terms[1] as TermView),
              name: "1b.year",
              display: "1b.year",
              base: true,
            },
          ],
        },
      ],
    };
    const host = mount(() => <ResultCard envelope={envelope} />);
    expect(host.querySelector('[data-term="age2"]')?.textContent).toBe("age20  (omitted)");
    expect(host.querySelector('[data-term="1b.year"]')?.textContent).toBe("1b.year(base)");
  });
});

describe("tabulate truncation (W14 acceptance, 06 §6.5)", () => {
  /** A 120 x 100 table: 12 000 cells, the number 06 §6.5 names. */
  function bigTable(): TabulatePayloadView {
    const rows = 120;
    const cols = 100;
    return {
      kind: "tabulate",
      row_var: "industry",
      col_var: "occupation",
      row_label: null,
      col_label: null,
      row_keys: Array.from({ length: rows }, (_, i) => [i, null] as const),
      col_keys: Array.from({ length: cols }, (_, i) => [i, null] as const),
      counts: Array.from({ length: rows * cols }, (_, i) => i % 7),
      row_totals: Array.from({ length: rows }, () => 100),
      col_totals: Array.from({ length: cols }, () => 120),
      total: rows * cols,
      requested: ["freq"],
      tests: [],
      truncated: { shown_cells: 2_000, total_cells: rows * cols },
    };
  }

  test("COUNTER: 12 000 cells produce exactly 2 000 DOM cells", () => {
    const payload = bigTable();
    expect(shownCells(payload)).toBe(2_000);
    const host = mount(() => <ResultCard envelope={envelopeOf(payload)} />);
    const cells = host.querySelectorAll("[data-tabulate-cell]");
    expect(cells).toHaveLength(2_000);
    expect(cells.length).toBeLessThanOrEqual(MAX_CELLS);
    // 20 whole rows of 100. Nothing beyond the budget is built at all.
    expect(host.querySelectorAll("[data-tabulate-row]")).toHaveLength(20);
    expect(host.querySelector("[data-tabulate-truncated]")?.textContent).toContain(
      "Open in Table Viewer",
    );
  });

  test("COUNTER: the cap holds even if the engine lies about `shown_cells`", () => {
    const payload: TabulatePayloadView = {
      ...bigTable(),
      truncated: { shown_cells: 12_000, total_cells: 12_000 },
    };
    expect(shownCells(payload)).toBe(MAX_CELLS);
    const host = mount(() => <ResultCard envelope={envelopeOf(payload)} />);
    expect(host.querySelectorAll("[data-tabulate-cell]")).toHaveLength(MAX_CELLS);
  });

  test("COUNTER: the cap holds when the engine forgets `truncated` entirely", () => {
    const payload: TabulatePayloadView = { ...bigTable(), truncated: null };
    expect(shownCells(payload)).toBe(MAX_CELLS);
    const host = mount(() => <ResultCard envelope={envelopeOf(payload)} />);
    expect(host.querySelectorAll("[data-tabulate-cell]")).toHaveLength(MAX_CELLS);
    expect(host.querySelector("[data-tabulate-truncated]")).not.toBeNull();
  });

  test("a table under the threshold renders whole, with its totals row", () => {
    const host = mount(() => <ResultCard envelope={envelopeOf(KINDS.tabulate)} />);
    expect(host.querySelectorAll("[data-tabulate-cell]")).toHaveLength(10);
    expect(host.querySelector("[data-tabulate-truncated]")).toBeNull();
    expect(host.querySelector("tfoot")?.textContent).toBe("Total2830181169");
    expect(host.querySelector('[data-assoc-test="pearson"]')?.textContent).toBe(
      "          Pearson chi2(4) =  27.2640   Pr = 0.000",
    );
  });

  test("a requested layer with no display string is named, not computed", () => {
    const payload: TabulatePayloadView = {
      ...(KINDS.tabulate as TabulatePayloadView),
      requested: ["freq", "row_pct", "col_pct"],
    };
    const host = mount(() => <ResultCard envelope={envelopeOf(payload)} />);
    expect(host.querySelector("[data-tabulate-layers]")?.textContent).toBe(
      "row percentage, column percentage in classic output",
    );
    // The golden's own 4.17 for Domestic/1 is nowhere on the card: it would have
    // had to be computed and rounded here.
    expect(host.textContent).not.toContain("4.17");
  });
});

describe("graph, error, table, scalars, log (§§17, 18)", () => {
  test("the graph box is laid out from `intrinsic_pt` before the bytes arrive", () => {
    const host = mount(() => <ResultCard envelope={envelopeOf(KINDS.graph)} />);
    const figure = host.querySelector<HTMLElement>("[data-graph]");
    expect(figure?.style.aspectRatio).toBe("400 / 300");
    expect(figure?.style.maxWidth).toBe("400px");
    expect(host.querySelector("[data-graph-placeholder]")).not.toBeNull();
  });

  test("the error card names the return code and the offending token", () => {
    const host = mount(() => <ResultCard envelope={envelopeOf(KINDS.error)} />);
    expect(host.querySelector("[data-error-message]")?.textContent).toBe("incme not found");
    expect(host.querySelector("[data-error-rc]")?.textContent).toBe("r(111);");
    expect(host.querySelector("[data-error-token]")?.textContent).toBe("incme");
    expect(host.querySelector("[data-error-suggestion]")?.textContent).toBe(
      "Did you mean `income`?",
    );
    expect(host.querySelector("[data-card]")?.getAttribute("data-state")).toBe("failed");
  });

  test("a `None` cell renders blank, never as `.`", () => {
    const host = mount(() => <ResultCard envelope={envelopeOf(KINDS.table)} />);
    const cells = [...host.querySelectorAll("tbody td")].map((td) => td.textContent);
    expect(cells).toEqual(["1.0000", "", "-0.4686", "1.0000"]);
  });

  test("scalars print their `display`, never their `value`", () => {
    const host = mount(() => <ResultCard envelope={envelopeOf(KINDS.scalars)} />);
    expect(host.querySelector('[data-scalar="r(mean)"]')?.textContent).toBe("r(mean)6165.257");
    expect(host.textContent).not.toContain("6165.2567");
  });

  test("data-change deltas are exact integer counts", () => {
    expect(deltaChips(KINDS.data_changed as DataChangedPayloadView)).toEqual([
      "+74 obs",
      "+12 var",
    ]);
    const host = mount(() => <ResultCard envelope={mock[0] as ResultEnvelopeView} />);
    expect(host.querySelector("[data-data-changed]")?.textContent).toContain("74 obs × 12 vars");
  });

  test("styled runs become spans, and the frontend parses no SMCL", () => {
    const host = mount(() => <ResultCard envelope={envelopeOf(KINDS.log)} />);
    const pre = host.querySelector("[data-log]");
    expect(pre?.textContent).toBe("(1978 automobile data)\n");
    expect(pre?.querySelector(".smcl--text")).not.toBeNull();
  });

  test("Unknown renders through the raw renderer — no apology, no empty state", () => {
    const host = mount(() => <ResultCard envelope={envelopeOf(KINDS.unknown)} />);
    const body = host.querySelector("[data-card-body]");
    expect(body?.querySelector("[data-raw-text]")?.textContent).toBe(
      envelopeOf(KINDS.unknown).raw.head,
    );
    expect(body?.textContent).not.toContain("unsupported");
    expect(body?.textContent).not.toContain("Unknown");
  });
});

describe("one block, one card (06 §4.7)", () => {
  test("several payloads become stacked sections inside one card", () => {
    const envelope: ResultEnvelopeView = {
      ...envelopeOf(KINDS.summarize),
      payloads: [KINDS.summarize, KINDS.graph],
    };
    const host = mount(() => <ResultCard envelope={envelope} />);
    expect(host.querySelectorAll("[data-card]")).toHaveLength(1);
    expect(
      [...host.querySelectorAll("[data-payload]")].map((s) => s.getAttribute("data-payload")),
    ).toEqual(["summarize", "graph"]);
    expect(host.querySelectorAll("[data-card-actions]")).toHaveLength(1);
  });
});
