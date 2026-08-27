/**
 * The History pane's acceptance bullet — plan W16, 06 §9.3:
 *
 * > History: `Command | _rc` columns, failed commands **in red**, **single click
 * > → Command pane (replacing contents), double click → resubmit**.
 *
 * The two gestures are asserted against counters rather than against what is on
 * screen, because "a single click did not run the command" is the absence of an
 * effect and an absence cannot be seen. `historyCounters.resubmits` and
 * `submitCounters.submissions` both staying at zero after a click is the proof;
 * a pane that ran a `bootstrap, reps(1000)` on a single click would pass any
 * test written against the rendered rows.
 */

import { render } from "solid-js/web";
import { afterEach, beforeEach, describe, expect, test } from "vitest";
import { commandBar, resetCommandBarHandle } from "../../commandbar/handle";
import { setDoFileInserter } from "../../commandbar/promote";
import { recordedSubmissions, resetSubmitState, submitCounters } from "../../commandbar/submit";
import { type HistoryEntry, appendHistory, resetHistoryState } from "../../state/history";
import { matchesFilter } from "./filter";
import { HistoryPane, historyCounters, resetHistoryCounters } from "./index";

const roots: (() => void)[] = [];

const ENTRIES: HistoryEntry[] = [
  { seq: 1, command: "sysuse auto, clear", rc: 0, origin: "commandbar" },
  { seq: 2, command: "summarize price", rc: 0, origin: "commandbar" },
  { seq: 3, command: "summarize nosuchvar", rc: 111, origin: "commandbar" },
];

function mount(props: Parameters<typeof HistoryPane>[0] = {}): HTMLElement {
  const host = document.createElement("div");
  document.body.append(host);
  roots.push(render(() => <HistoryPane {...props} />, host));
  return host;
}

const rowsOf = (host: HTMLElement): HTMLElement[] =>
  Array.from(host.querySelectorAll<HTMLElement>("[data-history-row]"));

beforeEach(() => {
  resetHistoryState();
  resetSubmitState();
  resetHistoryCounters();
  for (const entry of ENTRIES) appendHistory(entry);
});

afterEach(() => {
  while (roots.length > 0) roots.pop()?.();
  document.body.replaceChildren();
  resetCommandBarHandle();
  setDoFileInserter(null);
});

describe("the two columns (06 §9.3)", () => {
  test("Command and _rc, oldest first", () => {
    const host = mount();
    const headers = Array.from(host.querySelectorAll("th")).map((th) => th.textContent);
    expect(headers).toEqual(["Command", "_rc"]);

    const rows = rowsOf(host);
    expect(rows).toHaveLength(3);
    expect(rows[0]?.querySelector(".hist__cmd")?.textContent).toBe("sysuse auto, clear");
    expect(rows[2]?.querySelector(".hist__cmd")?.textContent).toBe("summarize nosuchvar");
  });

  test("a failure is marked as a failure AND prints its return code", () => {
    const host = mount();
    const rows = rowsOf(host);
    // `data-failed` is what `history.css` paints `--state-failed`; the printed
    // `_rc` is the second channel 06 §17 requires, so colour is never alone.
    expect(rows[1]?.hasAttribute("data-failed")).toBe(false);
    expect(rows[2]?.hasAttribute("data-failed")).toBe(true);
    expect(rows[2]?.querySelector(".hist__rc")?.textContent).toBe("111");
    expect(rows[1]?.querySelector(".hist__rc")?.textContent).toBe("");
  });
});

describe("single click loads, double click resubmits ([GSM] 2)", () => {
  test("a single click replaces the Command window's contents and runs nothing", () => {
    const host = mount();
    commandBar().replace("half-typed command");

    rowsOf(host)[1]?.click();

    expect(commandBar().text()).toBe("summarize price");
    expect(historyCounters.loads).toBe(1);
    // The whole point: nothing ran.
    expect(historyCounters.resubmits).toBe(0);
    expect(submitCounters.submissions).toBe(0);
    expect(recordedSubmissions()).toHaveLength(0);
  });

  test("a second single click replaces again rather than appending", () => {
    const host = mount();
    rowsOf(host)[1]?.click();
    rowsOf(host)[0]?.click();
    expect(commandBar().text()).toBe("sysuse auto, clear");
  });

  test("a double click resubmits", async () => {
    const host = mount();
    const row = rowsOf(host)[1];
    row?.dispatchEvent(new MouseEvent("dblclick", { bubbles: true }));
    await Promise.resolve();

    expect(historyCounters.resubmits).toBe(1);
    expect(recordedSubmissions().map((s) => s.text)).toEqual(["summarize price"]);
    expect(recordedSubmissions()[0]?.origin).toBe("history");
  });
});

describe("the same two gestures from the keyboard (06 §17)", () => {
  const key = (row: HTMLElement | undefined, init: KeyboardEventInit): void => {
    row?.dispatchEvent(new KeyboardEvent("keydown", { bubbles: true, ...init }));
  };

  test("the list is one tab stop, and the arrows move inside it", () => {
    const host = mount();
    const rows = rowsOf(host);
    expect(rows.map((r) => r.getAttribute("tabindex"))).toEqual(["0", "-1", "-1"]);

    key(rows[0], { key: "ArrowDown" });
    expect(rowsOf(host).map((r) => r.getAttribute("tabindex"))).toEqual(["-1", "0", "-1"]);

    key(rowsOf(host)[1], { key: "End" });
    expect(rowsOf(host).map((r) => r.getAttribute("tabindex"))).toEqual(["-1", "-1", "0"]);
  });

  test("Enter is the single click: it loads and runs nothing", () => {
    const host = mount();
    commandBar().replace("half-typed command");

    key(rowsOf(host)[1], { key: "Enter" });

    expect(commandBar().text()).toBe("summarize price");
    expect(historyCounters.loads).toBe(1);
    // The reason a single click must not run is the `bootstrap, reps(1000)` in
    // the list, and that reason does not change when the finger is on Enter.
    expect(historyCounters.resubmits).toBe(0);
    expect(recordedSubmissions()).toHaveLength(0);
  });

  test("Mod+Enter is the double click: it resubmits", async () => {
    const host = mount();
    key(rowsOf(host)[1], { key: "Enter", metaKey: true });
    await Promise.resolve();

    expect(historyCounters.resubmits).toBe(1);
    expect(recordedSubmissions().map((s) => s.text)).toEqual(["summarize price"]);
  });
});

describe("the filter at the base (06 §9.3)", () => {
  test("it matches any word, ignoring case, by default", () => {
    expect(matchesFilter({ query: "SUM", mode: "any" }, "summarize price")).toBe(true);
    expect(matchesFilter({ query: "reg mpg", mode: "any" }, "summarize mpg")).toBe(true);
    expect(matchesFilter({ query: "reg mpg", mode: "all" }, "summarize mpg")).toBe(false);
    expect(matchesFilter({ query: "", mode: "any" }, "anything")).toBe(true);
  });

  test("typing in the field narrows the rows", () => {
    const host = mount();
    const field = host.querySelector<HTMLInputElement>("[data-history-filter]");
    if (field === null) throw new Error("no filter field");
    field.value = "nosuchvar";
    field.dispatchEvent(new Event("input", { bubbles: true }));
    expect(rowsOf(host)).toHaveLength(1);
  });

  test("filtering is O(rows) per filter change and not per render", () => {
    const host = mount();
    const afterFirstRender = historyCounters.filterPasses;
    expect(afterFirstRender).toBe(3);

    // A click changes the selection, which re-renders rows — but must not
    // re-filter: PRODUCT_SPEC §0a forbids O(rows) work on an interaction path.
    rowsOf(host)[0]?.click();
    expect(historyCounters.filterPasses).toBe(afterFirstRender);
  });
});

describe("Do selected ([GSM] 2)", () => {
  test("it runs every selected command even after one fails", async () => {
    const host = mount();
    const rows = rowsOf(host);
    rows[0]?.click();
    rows[2]?.dispatchEvent(new MouseEvent("click", { bubbles: true, shiftKey: true }));

    const scroll = host.querySelector("[data-history-rows]");
    scroll?.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true }));

    const items = Array.from(document.querySelectorAll<HTMLElement>('[role="menuitem"]'));
    const doSelected = items.find((i) => i.textContent?.includes("Do selected"));
    expect(doSelected).toBeDefined();
    doSelected?.click();
    // `submitAll` awaits each command in turn ([GSM] 2: it does not stop on an
    // error), so the queue drains over several microtask turns.
    await new Promise((resolve) => setTimeout(resolve, 0));

    expect(recordedSubmissions().map((s) => s.text)).toEqual([
      "sysuse auto, clear",
      "summarize price",
      "summarize nosuchvar",
    ]);
  });
});
