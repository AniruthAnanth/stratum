/**
 * The Project pane — 06 §8.2's left group.
 *
 * Two properties, and the second is the one that matters in this unit: opening a
 * dataset changes the state of the session, so it is a `use` in the log; opening
 * a `.do` changes only what is on screen, so it is not a command at all. Getting
 * that boundary wrong in either direction is how a project explorer becomes a
 * source of state the log cannot explain.
 */

import { render } from "solid-js/web";
import { afterEach, beforeEach, describe, expect, test } from "vitest";
import { recordedSubmissions, resetSubmitState } from "../../commandbar/submit";
import {
  type ProjectEntry,
  ProjectPane,
  cdCommand,
  projectCounters,
  resetProjectCounters,
  useCommand,
} from "./index";

const roots: (() => void)[] = [];

const ENTRIES: ProjectEntry[] = [
  { path: "analysis.do", kind: "do" },
  { path: "clean.do", kind: "do" },
  { path: "data/auto.dta", kind: "data" },
  { path: "logs/session.smcl", kind: "log" },
];

function mount(props: Parameters<typeof ProjectPane>[0] = {}): HTMLElement {
  const host = document.createElement("div");
  document.body.append(host);
  roots.push(render(() => <ProjectPane entries={ENTRIES} {...props} />, host));
  return host;
}

const entry = (host: HTMLElement, path: string): HTMLElement | null =>
  host.querySelector<HTMLElement>(`[data-project-entry="${path}"]`);

beforeEach(() => {
  resetSubmitState();
  resetProjectCounters();
});

afterEach(() => {
  while (roots.length > 0) roots.pop()?.();
  document.body.replaceChildren();
});

describe("opening a file", () => {
  test("a dataset is a `use`, in the log", async () => {
    const host = mount();
    entry(host, "data/auto.dta")?.dispatchEvent(new MouseEvent("dblclick", { bubbles: true }));
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(recordedSubmissions().map((s) => s.text)).toEqual(['use "data/auto.dta", clear']);
    expect(projectCounters.commands).toBe(1);
  });

  test("a do-file is opened, not run: no command is issued", async () => {
    const opened: string[] = [];
    const host = mount({ onOpenFile: (path) => opened.push(path) });
    entry(host, "analysis.do")?.dispatchEvent(new MouseEvent("dblclick", { bubbles: true }));
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(opened).toEqual(["analysis.do"]);
    expect(recordedSubmissions()).toHaveLength(0);
    expect(projectCounters.opens).toBe(1);
  });

  test("the two commands are quoted, because paths have spaces in them", () => {
    expect(useCommand("My Data/auto 2.dta")).toBe('use "My Data/auto 2.dta", clear');
    expect(cdCommand("/Users/x/My Project")).toBe('cd "/Users/x/My Project"');
  });

  test("`cd` is offered only when a root is known", async () => {
    const bare = mount();
    expect(bare.querySelector("[data-project-cd]")).toBeNull();

    const rooted = mount({ root: "/Users/x/project" });
    rooted.querySelector<HTMLElement>("[data-project-cd]")?.click();
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(recordedSubmissions().map((s) => s.text)).toEqual(['cd "/Users/x/project"']);
  });
});

describe("the grouping", () => {
  test("files are grouped by kind, do-files first", () => {
    const host = mount();
    const groups = Array.from(host.querySelectorAll<HTMLElement>("[data-project-group]")).map(
      (g) => g.dataset["projectGroup"],
    );
    expect(groups).toEqual(["do", "data", "log"]);
  });

  test("collapsing a group does not re-scan the project", () => {
    const host = mount();
    const drawn = projectCounters.rowsRendered;
    expect(drawn).toBe(ENTRIES.length);
    host.querySelector<HTMLElement>('[data-project-group="do"]')?.click();
    // The two `.do` rows are removed; nothing else is rebuilt.
    expect(projectCounters.rowsRendered).toBe(drawn);
    expect(host.querySelectorAll("[data-project-entry]")).toHaveLength(2);
  });

  test("an empty project says why rather than drawing an empty tree", () => {
    const host = mount({ entries: [] });
    expect(host.textContent).toContain("no directory-listing command");
  });
});
