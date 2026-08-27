/**
 * The Variables pane's acceptance bullet — plan W16, 06 §9.4:
 *
 * > Variables: 3-state header sort (asc → desc → dataset order), display-only,
 * > never reorders the dataset; **one-click paste column** (`→` on row hover);
 * > double-click inserts at the Command caret.
 *
 * "Display-only" is the hard half, and it is asserted the only honest way: three
 * header clicks and the submission recorder is still empty. A sort that reached
 * the engine would have issued `sort` or `order`, and there is exactly one path
 * from this pane to the engine (`submitCommand`), so an empty recorder is a
 * proof rather than a sample.
 */

import { render } from "solid-js/web";
import { afterEach, beforeEach, describe, expect, test } from "vitest";
import { commandBar, resetCommandBarHandle } from "../../commandbar/handle";
import { recordedSubmissions, resetSubmitState } from "../../commandbar/submit";
import { type VariableRow, resetVarState } from "../../state/vars";
import { DEFAULT_VISIBLE_COLUMNS, VAR_COLUMNS, haystackOf, visibleColumns } from "./columns";
import { VariablesPane, resetVariablesCounters, variablesCounters } from "./index";
import {
  extendSelection,
  resetSelectionState,
  selectOnly,
  stepSelection,
  variableSelection,
} from "./selection";
import { DATASET_ORDER, ariaSort, compareCells, displayOrder, nextSort } from "./sort";
import { dropCommand, keepCommand, keepLabel, varlistText } from "./varlist";

const roots: (() => void)[] = [];

/** `sysuse auto` in dataset order — the golden's own first five variables. */
const AUTO: VariableRow[] = [
  { name: "make", storage: "str18", format: "%-18s", label: "Make and model" },
  { name: "price", storage: "int", format: "%8.0gc", label: "Price" },
  { name: "mpg", storage: "int", format: "%8.0g", label: "Mileage (mpg)" },
  { name: "rep78", storage: "int", format: "%8.0g", label: "Repair record 1978" },
  { name: "headroom", storage: "float", format: "%6.1f", label: "Headroom (in.)" },
];

function mount(rows: readonly VariableRow[] = AUTO): HTMLElement {
  const host = document.createElement("div");
  document.body.append(host);
  roots.push(render(() => <VariablesPane rows={rows} confirm={() => true} />, host));
  return host;
}

const names = (host: HTMLElement): string[] =>
  Array.from(host.querySelectorAll<HTMLElement>("[data-variables-row]")).map(
    (row) => row.dataset["variablesRow"] ?? "",
  );

/** The `columnheader` cell. It is what carries `aria-sort`. */
const header = (host: HTMLElement, id: string): HTMLElement => {
  const cell = host.querySelector<HTMLElement>(`[data-variables-header="${id}"]`);
  if (cell === null) throw new Error(`no ${id} header`);
  return cell;
};

/** The button inside it. The cell states the order; the button changes it. */
const sortButton = (host: HTMLElement, id: string): HTMLElement => {
  const button = host.querySelector<HTMLElement>(`[data-variables-sort="${id}"]`);
  if (button === null) throw new Error(`no ${id} sort button`);
  return button;
};

beforeEach(() => {
  resetVarState();
  resetSubmitState();
  resetSelectionState();
  resetVariablesCounters();
});

afterEach(() => {
  while (roots.length > 0) roots.pop()?.();
  document.body.replaceChildren();
  resetCommandBarHandle();
  resetSelectionState();
});

// ---------------------------------------------------------------------------
// Three-state sort, display-only
// ---------------------------------------------------------------------------

describe("3-state header sort ([GSM] 2, 06 §9.4)", () => {
  test("asc → desc → dataset order, and a different column restarts at asc", () => {
    let state = DATASET_ORDER;
    state = nextSort(state, "name");
    expect(state).toEqual({ column: "name", direction: "asc" });
    state = nextSort(state, "name");
    expect(state).toEqual({ column: "name", direction: "desc" });
    state = nextSort(state, "name");
    expect(state).toEqual(DATASET_ORDER);

    state = nextSort({ column: "name", direction: "desc" }, "label");
    expect(state).toEqual({ column: "label", direction: "asc" });
  });

  test("three clicks on the header walk the pane back to dataset order", () => {
    const host = mount();
    expect(names(host)).toEqual(["make", "price", "mpg", "rep78", "headroom"]);

    sortButton(host, "name").click();
    expect(names(host)).toEqual(["headroom", "make", "mpg", "price", "rep78"]);
    expect(header(host, "name").getAttribute("aria-sort")).toBe("ascending");

    sortButton(host, "name").click();
    expect(names(host)).toEqual(["rep78", "price", "mpg", "make", "headroom"]);
    expect(header(host, "name").getAttribute("aria-sort")).toBe("descending");

    sortButton(host, "name").click();
    expect(names(host)).toEqual(["make", "price", "mpg", "rep78", "headroom"]);
    expect(header(host, "name").getAttribute("aria-sort")).toBe("none");
  });

  test("sorting is display-only: three clicks issue no command at all", () => {
    const host = mount();
    sortButton(host, "name").click();
    sortButton(host, "name").click();
    sortButton(host, "name").click();
    sortButton(host, "label").click();
    expect(recordedSubmissions()).toHaveLength(0);
    expect(variablesCounters.commands).toBe(0);
  });

  test("the order is a permutation of indices, so a relabel re-sorts live", () => {
    const rows = [{ label: "b" }, { label: "a" }, { label: "c" }];
    const order = displayOrder(rows, { column: "label", direction: "asc" }, (r) => r.label);
    expect(order).toEqual([1, 0, 2]);
    // Nothing anywhere holds a sorted copy of the rows.
    expect(rows.map((r) => r.label)).toEqual(["b", "a", "c"]);
  });

  test("empty sorts last in both directions, and the comparator is platform-stable", () => {
    expect(compareCells("", "price")).toBe(1);
    expect(compareCells("price", "")).toBe(-1);
    // Not `localeCompare`: `_merge` vs `make` must not depend on the webview's
    // ICU build, because Scenario E compares three platforms.
    expect(compareCells("_merge", "make")).toBe(-1);
    expect(ariaSort({ column: "name", direction: "desc" }, "name")).toBe("descending");
    expect(ariaSort({ column: "name", direction: "desc" }, "label")).toBe("none");
  });
});

// ---------------------------------------------------------------------------
// The paste column and the double click
// ---------------------------------------------------------------------------

describe("one-click paste and double-click insert (06 §9.4)", () => {
  test("the paste column sends that one variable to the Command window", () => {
    const host = mount();
    commandBar().replace("summarize");
    const arrow = host.querySelector<HTMLElement>('[data-variables-paste="mpg"]');
    arrow?.click();
    // Inserted at the caret with the separating space the manual's F-key advice
    // is about, so `summarize` does not become `summarizempg`.
    expect(commandBar().text()).toBe("summarize mpg");
    expect(variablesCounters.pastes).toBe(1);
  });

  test("the paste column does not change the selection", () => {
    const host = mount();
    selectOnly("price");
    host.querySelector<HTMLElement>('[data-variables-paste="mpg"]')?.click();
    expect(variableSelection().names).toEqual(["price"]);
  });

  test("a double click inserts at the caret; a multi-selection inserts all of it", () => {
    const host = mount();
    const row = (name: string): HTMLElement | null =>
      host.querySelector<HTMLElement>(`[data-variables-row="${name}"]`);

    row("price")?.dispatchEvent(new MouseEvent("dblclick", { bubbles: true }));
    expect(commandBar().text()).toBe("price");

    commandBar().clear();
    row("price")?.click();
    row("rep78")?.dispatchEvent(new MouseEvent("click", { bubbles: true, shiftKey: true }));
    row("rep78")?.dispatchEvent(new MouseEvent("dblclick", { bubbles: true }));
    expect(commandBar().text()).toBe("price mpg rep78");
  });

  test("every pointer gesture on a row has a key (06 §17)", () => {
    const host = mount();
    const row = (name: string): HTMLElement | null =>
      host.querySelector<HTMLElement>(`[data-variables-row="${name}"]`);
    const key = (name: string, init: KeyboardEventInit): void => {
      row(name)?.dispatchEvent(new KeyboardEvent("keydown", { bubbles: true, ...init }));
    };

    // The list is one tab stop; the arrows move inside it.
    expect(row("make")?.getAttribute("tabindex")).toBe("0");
    key("make", { key: "ArrowDown" });
    expect(row("price")?.getAttribute("tabindex")).toBe("0");
    expect(row("make")?.getAttribute("tabindex")).toBe("-1");

    // Space is the single click.
    key("price", { key: " " });
    expect(variableSelection().names).toEqual(["price"]);
    expect(variablesCounters.pastes).toBe(0);

    // Enter is the double click.
    commandBar().clear();
    key("price", { key: "Enter" });
    expect(commandBar().text()).toBe("price");
    expect(variablesCounters.pastes).toBe(1);

    // Mod+Enter is the 14 px paste column: one variable, no selection change.
    commandBar().clear();
    key("mpg", { key: "Enter", metaKey: true });
    expect(commandBar().text()).toBe("mpg");
    expect(variableSelection().names).toEqual(["price"]);
  });

  test("Shift extends in the pane's display order, not the dataset's", () => {
    const host = mount();
    sortButton(host, "name").click(); // headroom make mpg price rep78
    const row = (name: string): HTMLElement | null =>
      host.querySelector<HTMLElement>(`[data-variables-row="${name}"]`);
    row("make")?.click();
    row("mpg")?.dispatchEvent(new MouseEvent("click", { bubbles: true, shiftKey: true }));
    expect(variableSelection().names).toEqual(["make", "mpg"]);
  });
});

// ---------------------------------------------------------------------------
// The context menu issues real commands
// ---------------------------------------------------------------------------

describe("the context menu issues standard Stata commands ([GSM] 2)", () => {
  test("keep and drop are the commands a user would have typed", () => {
    const all = AUTO.map((r) => r.name);
    expect(keepCommand(["price", "mpg"], all)).toBe("keep price mpg");
    expect(dropCommand(["price"], all)).toBe("drop price");
    expect(keepLabel(["price"])).toBe("Keep only variable “price”");
    expect(keepLabel(["price", "mpg"])).toBe("Keep only selected variables");
  });

  test("a compact varlist only collapses runs adjacent in DATASET order", () => {
    const all = ["v1", "v2", "v3", "v4", "v5"];
    expect(varlistText(["v1", "v2", "v3", "v4"], all, "compact")).toBe("v1-v4");
    // Selected out of order, still a dataset-order range.
    expect(varlistText(["v4", "v1", "v3", "v2"], all, "compact")).toBe("v1-v4");
    // A run of two is spelled out: `a-b` saves nothing and reads worse.
    expect(varlistText(["v1", "v2"], all, "compact")).toBe("v1 v2");
    // Non-adjacent stays explicit.
    expect(varlistText(["v1", "v3", "v5"], all, "compact")).toBe("v1 v3 v5");
  });

  test("Drop issues the command through the ordinary submission path", async () => {
    const host = mount();
    host.querySelector<HTMLElement>('[data-variables-row="price"]')?.click();
    host
      .querySelector("[data-variables-rows]")
      ?.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true }));

    const item = Array.from(document.querySelectorAll<HTMLElement>('[role="menuitem"]')).find((i) =>
      i.textContent?.startsWith("Drop variable"),
    );
    expect(item).toBeDefined();
    item?.click();
    await new Promise((resolve) => setTimeout(resolve, 0));

    expect(recordedSubmissions().map((s) => s.text)).toEqual(["drop price"]);
    expect(variablesCounters.commands).toBe(1);
  });
});

// ---------------------------------------------------------------------------
// Columns, filter, windowing
// ---------------------------------------------------------------------------

describe("columns and filtering (06 §9.4)", () => {
  test("Name and Label are on by default; Name cannot be hidden", () => {
    expect(DEFAULT_VISIBLE_COLUMNS).toEqual(["name", "label"]);
    expect(visibleColumns(new Set()).map((c) => c.id)).toEqual(["name"]);
    expect(VAR_COLUMNS.find((c) => c.id === "name")?.required).toBe(true);
  });

  test("the filter haystack is the visible columns, lowercased", () => {
    const cols = visibleColumns(new Set(["label"]));
    expect(haystackOf(AUTO[1] as VariableRow, cols)).toEqual(["price", "price"]);
  });

  test("typing in the filter narrows the rows", () => {
    const host = mount();
    const field = host.querySelector<HTMLInputElement>("[data-variables-filter]");
    if (field === null) throw new Error("no filter field");
    field.value = "repair";
    field.dispatchEvent(new Event("input", { bubbles: true }));
    expect(names(host)).toEqual(["rep78"]);
  });

  test("rows are windowed: a 5 000-variable frame renders a screenful", () => {
    const many: VariableRow[] = Array.from({ length: 5_000 }, (_, i) => ({
      name: `v${i}`,
      storage: "float",
      format: "%9.0g",
    }));
    resetVariablesCounters();
    const host = mount(many);
    // The claim PRODUCT_SPEC §0a is about: the DOM cost is the viewport, not
    // the frame. The exact bound is the initial viewport plus overscan.
    expect(variablesCounters.rowsRendered).toBeLessThan(100);
    expect(names(host).length).toBeLessThan(100);
    expect(names(host)[0]).toBe("v0");
  });
});

// ---------------------------------------------------------------------------
// The selection the Properties pane follows
// ---------------------------------------------------------------------------

describe("selection (06 §9.4, §9.5)", () => {
  test("◀ ▶ step the primary and collapse the selection to one", () => {
    const order = AUTO.map((r) => r.name);
    selectOnly("make");
    extendSelection(order, "mpg");
    expect(variableSelection().names).toEqual(["make", "price", "mpg"]);

    expect(stepSelection(order, 1)).toBe("rep78");
    expect(variableSelection().names).toEqual(["rep78"]);
    expect(stepSelection(order, -1)).toBe("mpg");
  });

  test("a fresh pane responds to both arrows", () => {
    const order = AUTO.map((r) => r.name);
    expect(stepSelection(order, 1)).toBe("make");
    resetSelectionState();
    expect(stepSelection(order, -1)).toBe("headroom");
  });
});
