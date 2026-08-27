/**
 * Model comparison — spec §19, 06 §7.
 *
 * The bullet under test: "Model comparison (§19) refuses to silently compare
 * across samples: mismatched `sample_hash` carries a persistent
 * `Samples differ: 74 vs 69 observations` row."
 */

import { render } from "solid-js/web";
import { afterEach, describe, expect, test } from "vitest";
import type { EstimationPayloadView, ScalarsPayloadView, TermView } from "../../renderers";
import { scenarioAEnvelopes } from "../../renderers/fixtures";
import {
  type CompareModel,
  buildCompareTable,
  comparability,
  esttabCommand,
  sampleWarning,
  stars,
  termOrder,
} from "./compare";
import { ComparePane } from "./index";

const roots: (() => void)[] = [];

function mount(node: () => ReturnType<typeof ComparePane>): HTMLElement {
  const host = document.createElement("div");
  document.body.append(host);
  roots.push(render(node, host));
  return host;
}

afterEach(() => {
  while (roots.length > 0) roots.pop()?.();
  document.body.replaceChildren();
});

/** StataMP 18.5's own `regress price mpg weight foreign`, from the mock. */
const REGRESS = ((): EstimationPayloadView => {
  const payload = scenarioAEnvelopes()[2]?.payloads[0];
  if (payload?.kind !== "estimation") throw new Error("expected the regress payload");
  return payload;
})();

function model(label: string, over: Partial<EstimationPayloadView> = {}): CompareModel {
  return { label, payload: { ...REGRESS, ...over } };
}

describe("comparability (06 §7)", () => {
  test("same depvar and same sample: comparable, no row", () => {
    const c = comparability([model("(1)"), model("(2)")]);
    expect(c.ok).toBe(true);
    expect(sampleWarning(c)).toBeUndefined();
  });

  test("same depvar, DIFFERENT sample: offered, and it says so, persistently", () => {
    const models = [model("(1)"), model("(2)", { n: 69, sample_hash: "1234567890123456789" })];
    const table = buildCompareTable(models);
    expect(table.warning).toBe("Samples differ: 74 vs 69 observations");

    const host = mount(() => <ComparePane models={models} />);
    const row = host.querySelector("[data-compare-warning]");
    expect(row?.textContent).toBe("Samples differ: 74 vs 69 observations");
    // Inside the table's own frame, so a screenshot and an export both carry it.
    expect(row?.closest("table")).not.toBeNull();
    expect(row?.querySelector("button")).toBeNull();
  });

  test("a u64 sample_hash is compared as a key, not as a double", () => {
    // These two differ in the last decimal digit and are indistinguishable as
    // JS numbers. Comparing them as numbers would report "same sample".
    const a = "6004496033318516789";
    const b = "6004496033318516788";
    expect(Number(a)).toBe(Number(b));
    const c = comparability([model("(1)", { sample_hash: a }), model("(2)", { sample_hash: b })]);
    expect(c.ok).toBe(false);
  });

  test("different depvars say that instead", () => {
    const c = comparability([model("(1)"), model("(2)", { depvar: "lwage" })]);
    expect(sampleWarning(c)).toBe("Different dependent variables: price vs lwage");
  });
});

describe("row order is the model's narrative, never the alphabet (06 §7)", () => {
  test("first appearance across models, left to right, `_cons` last", () => {
    const first = model("(1)", {
      terms: [
        { ...(REGRESS.terms[0] as TermView), name: "mpg", display: "mpg" },
        { ...(REGRESS.terms[3] as TermView), name: "_cons", display: "_cons" },
      ],
    });
    const second = model("(2)", {
      terms: [
        { ...(REGRESS.terms[1] as TermView), name: "weight", display: "weight" },
        { ...(REGRESS.terms[0] as TermView), name: "mpg", display: "mpg" },
        { ...(REGRESS.terms[3] as TermView), name: "_cons", display: "_cons" },
      ],
    });
    expect(termOrder([first, second])).toEqual(["mpg", "weight", "_cons"]);
    // Alphabetically this would be _cons, mpg, weight — which is exactly the
    // ordering 06 §7 forbids.
    expect(termOrder([first, second])).not.toEqual(["_cons", "mpg", "weight"]);
  });

  test("a term missing from a model is `—`, never blank", () => {
    const first = model("(1)", { terms: [REGRESS.terms[0] as TermView] });
    const second = model("(2)", { terms: [REGRESS.terms[1] as TermView] });
    const table = buildCompareTable([first, second]);
    expect(table.rows.map((r) => r.term)).toEqual(["mpg", "weight"]);
    expect(table.rows[0]?.cells[1]).toEqual({ b: "—", se: "", stars: "", note: "absent" });
  });
});

describe("cells print `display_num`, and stars come from the raw p", () => {
  test("coefficient over standard error, both verbatim", () => {
    const table = buildCompareTable([model("(1)")]);
    const weight = table.rows.find((r) => r.term === "weight");
    expect(weight?.cells[0]?.b).toBe("3.464706");
    expect(weight?.cells[0]?.se).toBe(".630749");
  });

  test("stars are a comparison, not a rendering of p", () => {
    expect(stars(0.0009)).toBe("***");
    expect(stars(0.009)).toBe("**");
    expect(stars(0.049)).toBe("*");
    expect(stars(0.769)).toBe("");
    const table = buildCompareTable([model("(1)")]);
    expect(table.rows.find((r) => r.term === "mpg")?.cells[0]?.stars).toBe("");
    expect(table.rows.find((r) => r.term === "weight")?.cells[0]?.stars).toBe("***");
  });
});

describe("the footer (06 §7)", () => {
  test("N is always reportable; a scalar with no display string is `—`", () => {
    const table = buildCompareTable([model("(1)"), model("(2)", { n: 69 })]);
    expect(table.footer[0]).toEqual({ name: "N", values: ["74", "69"] });
    // r2 is on the wire as a bare f64, so there is nothing to print.
    expect(table.footer.find((r) => r.name === "r2")?.values).toEqual(["—", "—"]);
    for (const row of table.footer) expect(row.values.every((v) => v !== "")).toBe(true);
  });

  test("a sibling `Scalars` payload fills the footer in", () => {
    const scalars: ScalarsPayloadView = {
      kind: "scalars",
      values: [["r2", { t: "num", value: 0.4996, display: "0.4996" }]],
    };
    const table = buildCompareTable([{ ...model("(1)"), scalars }, model("(2)")]);
    expect(table.footer.find((r) => r.name === "r2")?.values).toEqual(["0.4996", "—"]);
  });

  test("preference order first, then the rest, deterministically", () => {
    const table = buildCompareTable([model("(1)")]);
    expect(table.footer.slice(0, 5).map((r) => r.name)).toEqual(["N", "r2", "r2_a", "F", "rmse"]);
    const rest = table.footer.slice(5).map((r) => r.name);
    expect(rest).toEqual([...rest].sort());
  });
});

describe("the pane", () => {
  test("fewer than two models is an invitation, not an empty table", () => {
    const host = mount(() => <ComparePane models={[model("(1)")]} />);
    expect(host.querySelector("[data-compare-table]")).toBeNull();
    expect(host.textContent).toContain("estimates store");
  });

  test("`Copy as esttab command` appears only when every model was stored", () => {
    expect(esttabCommand([model("(1)"), model("(2)")])).toBeUndefined();
    const stored = [model("(1)", { estimates_name: "m1" }), model("(2)", { estimates_name: "m2" })];
    expect(esttabCommand(stored)).toBe("esttab m1 m2, se star(* 0.05 ** 0.01 *** 0.001) label");
    const copied: string[] = [];
    const host = mount(() => (
      <ComparePane models={stored} onCopy={(_, text) => copied.push(text)} />
    ));
    host.querySelector<HTMLButtonElement>(".cmp__esttab")?.click();
    expect(copied).toEqual(["esttab m1 m2, se star(* 0.05 ** 0.01 *** 0.001) label"]);
  });
});
