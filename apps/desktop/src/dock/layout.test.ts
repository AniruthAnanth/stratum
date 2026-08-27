/**
 * The layout system — IMPLEMENTATION_PLAN W12 acceptance:
 *
 *   "Layout preset switch <= 120 ms, preserving editor text, undo history,
 *    scroll and cards (they live outside the dock and are re-parented)."
 *
 * The budget is the easy half. The hard half is the preservation claim, and the
 * only way to assert it honestly is to put mutable state that dockview knows
 * nothing about inside a pane — a DOM node identity, a text buffer, an undo
 * stack, a scroll offset — and check that all four are the same objects with the
 * same values on the other side of `fromJSON`.
 */

import { afterEach, beforeEach, describe, expect, it } from "vitest";
import {
  SWITCH_BUDGET_MS,
  applyLayout,
  applyPreset,
  attachDock,
  detachDock,
  layoutSpec,
  resetLayoutState,
} from "../state/layout";
import { type DockAdapter, createDock } from "./adapter";
import {
  PRESET_IDS,
  dockComponents,
  paneForIndex,
  paneOrder,
  validateLayoutSpec,
} from "./layoutSpec";
import {
  captureScroll,
  clearPaneRegistry,
  disposePaneHosts,
  paneHost,
  registerPane,
  restoreScroll,
} from "./panes";
import { preset } from "./presets";

/**
 * A stand-in for W13's editor: a DOM node, a text buffer, an undo stack and a
 * scroller. None of it is anything dockview can serialise, which is the point.
 */
interface FakeEditor {
  element: HTMLElement;
  text: string;
  undo: string[];
  scroller: HTMLElement;
}

let editor: FakeEditor | undefined;
let container: HTMLDivElement;
let dock: DockAdapter;

const mountFakeEditor = (host: HTMLElement): void => {
  const scroller = document.createElement("div");
  scroller.className = "cm-scroller";
  host.appendChild(scroller);
  editor = { element: host, text: "", undo: [], scroller };
};

beforeEach(() => {
  resetLayoutState();
  clearPaneRegistry();
  disposePaneHosts();
  editor = undefined;

  container = document.createElement("div");
  container.style.width = "1280px";
  container.style.height = "740px";
  document.body.appendChild(container);

  registerPane("editor", mountFakeEditor, "Editor");
  dock = createDock(container);
  dock.layout(1280, 740);
  attachDock(dock);
});

afterEach(() => {
  detachDock();
  dock.dispose();
  container.remove();
  disposePaneHosts();
  clearPaneRegistry();
  resetLayoutState();
});

describe("the shipped presets", () => {
  it.each(PRESET_IDS)("%s is a valid LayoutSpec", (id) => {
    const result = validateLayoutSpec(preset(id));
    expect(result.ok, result.ok ? "" : JSON.stringify(result.issues)).toBe(true);
  });

  it("docks the panes 06 §8.2-§8.4 describe", () => {
    expect(new Set(dockComponents(preset("modern").windows[0]?.dock))).toEqual(
      new Set(["project", "variables", "sections", "editor", "results", "graphs", "assistant"]),
    );
    // Classic mirrors Stata 18's Widescreen layout, Command under Results.
    expect(new Set(dockComponents(preset("classic").windows[0]?.dock))).toEqual(
      new Set(["history", "results", "commandbar", "variables", "properties"]),
    );
    // Sidebar folds History into the right stack, so there is no left column.
    expect(new Set(dockComponents(preset("classic-sidebar").windows[0]?.dock))).toEqual(
      new Set(["results", "commandbar", "variables", "properties", "history"]),
    );
    expect(dockComponents(preset("focus").windows[0]?.dock)).toEqual(["editor"]);
  });

  it("orders panes left to right for Mod+1..9", () => {
    // The user counts panes on screen, so Mod+1 is the leftmost group's first
    // tab and not whichever panel happens to be first in the `panels` map.
    expect(paneOrder(preset("modern")).slice(0, 5)).toEqual([
      "project",
      "variables",
      "sections",
      "editor",
      "results",
    ]);
    expect(paneForIndex(preset("classic"), 1)).toBe("history");
    expect(paneForIndex(preset("focus"), 1)).toBe("editor");
    expect(paneForIndex(preset("focus"), 2)).toBeUndefined();
  });

  it("carries the chrome and defaults each preset's section specifies", () => {
    expect(preset("modern").chrome).toEqual({ topBar: "full", statusBar: true });
    expect(preset("classic").defaults.inlineResults).toBe("off");
    expect(preset("classic").defaults.commandBar).toBe("pane");
    expect(preset("focus").chrome).toEqual({ topBar: "auto-hide", statusBar: false });
    expect(preset("focus").defaults.inlineResults).toBe("always");
    expect(preset("focus").defaults.commandBar).toBe("overlay");
  });

  it("hands out a copy, so a dragged sash cannot mutate a preset", () => {
    const a = preset("modern");
    const b = preset("modern");
    expect(a).not.toBe(b);
    expect(a.windows[0]?.dock).not.toBe(b.windows[0]?.dock);
  });
});

describe("preset switching", () => {
  it("re-parents the pane instead of rebuilding it", () => {
    const before = paneHost("editor");
    expect(dock.panes()).toContain("editor");

    if (editor === undefined) throw new Error("editor did not mount");
    editor.text = "sysuse auto\nsummarize price mpg";
    editor.undo.push("sysuse auto");
    editor.undo.push("summarize price mpg");

    applyPreset("focus");
    applyPreset("classic-sidebar"); // no editor at all in Classic
    applyPreset("modern");

    const after = paneHost("editor");
    // Same node, same mount, same everything the editor put inside it.
    expect(after).toBe(before);
    expect(editor.element).toBe(before);
    expect(editor.text).toBe("sysuse auto\nsummarize price mpg");
    expect(editor.undo).toEqual(["sysuse auto", "summarize price mpg"]);
    expect(before.querySelector(".cm-scroller")).toBe(editor.scroller);
  });

  it("mounts a pane exactly once across many switches", () => {
    let mounts = 0;
    clearPaneRegistry();
    disposePaneHosts();
    registerPane("results", (host) => {
      mounts++;
      host.appendChild(document.createElement("span"));
    });
    applyPreset("modern");
    applyPreset("classic");
    applyPreset("modern");
    expect(mounts).toBe(1);
  });

  it("captures and restores scroll around the re-parent", () => {
    if (editor === undefined) throw new Error("editor did not mount");

    // jsdom reports every rect as zero and clamps `scrollTop` to 0, so the
    // offset is faked at the property level AND every write is recorded. The
    // recorded writes are the assertion: a switch must produce a SECOND write
    // of the captured value, which is only possible if the adapter captured on
    // dispose and restored on init. Asserting the final value alone would pass
    // in jsdom even if neither had happened.
    const writes: number[] = [];
    let scrollTop = 0;
    Object.defineProperty(editor.scroller, "scrollTop", {
      configurable: true,
      get: () => scrollTop,
      set: (v: number) => {
        scrollTop = v;
        writes.push(v);
      },
    });

    editor.scroller.scrollTop = 4212;
    expect(writes).toEqual([4212]);

    applyPreset("focus");
    expect(writes).toEqual([4212, 4212]);
    expect(editor.scroller.scrollTop).toBe(4212);
  });

  it("keys the capture by element identity, not by position", () => {
    // The pure half of the mechanism, with the zeroing a real engine performs
    // put back in by hand.
    const root = document.createElement("div");
    const inner = document.createElement("div");
    root.appendChild(inner);
    let top = 0;
    Object.defineProperty(inner, "scrollTop", {
      configurable: true,
      get: () => top,
      set: (v: number) => {
        top = v;
      },
    });

    inner.scrollTop = 1234;
    const state = captureScroll(root);
    inner.scrollTop = 0; // what removing an element from the document does
    restoreScroll(state);
    expect(inner.scrollTop).toBe(1234);
  });

  it("stays inside the 120 ms budget for every preset pair", () => {
    const ids = PRESET_IDS;
    const worst = { pair: "", ms: 0 };
    for (const from of ids) {
      for (const to of ids) {
        applyPreset(from);
        const ms = applyPreset(to);
        if (ms > worst.ms) {
          worst.pair = `${from} -> ${to}`;
          worst.ms = ms;
        }
      }
    }
    expect(worst.ms, `slowest switch: ${worst.pair}`).toBeLessThanOrEqual(SWITCH_BUDGET_MS);
  });

  it("updates the store and the dock together", () => {
    applyPreset("classic");
    expect(layoutSpec().id).toBe("classic");
    expect(dock.panes()).toContain("history");
    expect(dock.panes()).not.toContain("editor");
  });
});

describe("validation and fallback", () => {
  it("rejects a spec whose dock places one pane twice", () => {
    // Two panels claiming one persistent host would make the second
    // `appendChild` silently steal the node from the first.
    const spec = preset("modern");
    const dockBlob = spec.windows[0]?.dock as {
      panels: Record<string, { id: string; contentComponent: string }>;
    };
    dockBlob.panels["editor2"] = { id: "editor2", contentComponent: "editor" };
    const result = validateLayoutSpec(spec);
    expect(result.ok).toBe(false);
    if (result.ok) throw new Error("unreachable");
    expect(result.issues[0]?.message).toMatch(/appears twice/);
  });

  it("collects every issue rather than stopping at the first", () => {
    const result = validateLayoutSpec({ schema: 2, id: "nope", name: 7, windows: [] });
    expect(result.ok).toBe(false);
    if (result.ok) throw new Error("unreachable");
    expect(result.issues.length).toBeGreaterThan(3);
    expect(result.issues.map((i) => i.path)).toContain("schema");
    expect(result.issues.map((i) => i.path)).toContain("chrome");
  });

  it("warns about an unknown pane id without rejecting the layout", () => {
    const spec = preset("focus");
    const dockBlob = spec.windows[0]?.dock as {
      panels: Record<string, { id: string; contentComponent: string }>;
    };
    dockBlob.panels["future"] = { id: "future", contentComponent: "future" };
    const result = validateLayoutSpec(spec);
    expect(result.ok).toBe(true);
    if (!result.ok) throw new Error("unreachable");
    expect(result.warnings.some((w) => w.message.includes("future"))).toBe(true);
  });

  it("renders an unregistered pane as an empty host, not a crash", () => {
    // `graphs` is W19's and does not exist yet. Modern docks it anyway.
    applyPreset("modern");
    const host = paneHost("graphs");
    expect(host.hasAttribute("data-unregistered")).toBe(true);
    expect(host.dataset["pane"]).toBe("graphs");
  });
});

describe("applyLayout", () => {
  it("reports its own elapsed time", () => {
    const ms = applyLayout(preset("classic"));
    expect(ms).toBeGreaterThanOrEqual(0);
    expect(ms).toBeLessThanOrEqual(SWITCH_BUDGET_MS);
  });
});
