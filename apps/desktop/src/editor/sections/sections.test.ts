/**
 * Sections, folding and Document View — spec §3 and §24, 06 §4.8 and §4.9.
 *
 * The through-line is the same one as everywhere else in this unit: a marker is
 * an ordinary Stata comment, a fold is a decoration, and Document View is a
 * decoration too. None of the three may change a byte of the file, and the
 * three text-changing operations (`rename`, `move`, `insert`) are W26's gated
 * writers reached through a seam, which is asserted by their refusing to act
 * when no writer is installed.
 */

import { beforeAll, describe, expect, it, vi } from "vitest";
import { counters } from "../blocks/segmenter";
import { mountEditor } from "../harness";
import { sourceOffsetForNode, toggleDocumentView } from "./docview";
import { foldableAt, isFolded, toggleFoldAt } from "./fold";
import { renderNarrative, stripNarrativePrefix } from "./markdown";
import { sectionAt, sectionTitle, sectionWriter, sections, setSectionWriter } from "./markers";

beforeAll(() => {
  vi.spyOn(console, "warn").mockImplementation(() => {});
});

const DOC = [
  "// %% Data loading",
  "use survey.dta, clear",
  "",
  "// %% Cleaning",
  "drop if missing(income)",
  "foreach v of varlist a b c {",
  "  replace `v' = 0 if missing(`v')",
  "}",
].join("\n");

describe("section markers are ordinary comments", () => {
  it("indexes every marker and reads its label out of the source", async () => {
    const h = await mountEditor(DOC);
    const found = sections(h.view.state);
    expect(found.length).toBeGreaterThanOrEqual(2);
    expect(sectionTitle(h.view.state, found[0] as never)).toBe("Data loading");
    expect(sectionTitle(h.view.state, found[1] as never)).toBe("Cleaning");
    h.destroy();
  });

  it("answers which section a position is in", async () => {
    const h = await mountEditor(DOC);
    const at = h.view.state.doc.line(5).from;
    expect(sectionTitle(h.view.state, sectionAt(h.view.state, at) as never)).toBe("Cleaning");
    h.destroy();
  });

  it("refuses to rename or move without W26's gated writer", async () => {
    const h = await mountEditor(DOC);
    const before = h.view.state.doc.toString();
    expect(sectionWriter()).toBeNull();

    const { registerEditorCommands } = await import("../commands");
    const dispose = registerEditorCommands();
    const { getCommand } = await import("../../keys/registry");
    // The verbs exist so the palette can list them, and report themselves
    // disabled rather than reaching for `view.dispatch` (A15).
    expect(getCommand("section.rename")?.enabled?.({})).toBe(false);
    expect(getCommand("section.moveUp")?.enabled?.({})).toBe(false);
    expect(h.view.state.doc.toString()).toBe(before);
    expect(counters.documentWrites).toBe(0);
    dispose();
    setSectionWriter(null);
    h.destroy();
  });
});

describe("folding is a decoration", () => {
  it("folds a brace block without touching the text", async () => {
    const h = await mountEditor(DOC);
    const before = h.view.state.doc.toString();
    const inside = h.view.state.doc.line(6).from + 2;
    const range = foldableAt(h.view.state, inside);
    expect(range).not.toBeNull();

    expect(toggleFoldAt(h.view, inside)).toBe(true);
    expect(
      isFolded(h.view.state, (range as { from: number }).from, (range as { to: number }).to),
    ).toBe(true);
    expect(h.view.state.doc.toString()).toBe(before);

    toggleFoldAt(h.view, inside);
    expect(
      isFolded(h.view.state, (range as { from: number }).from, (range as { to: number }).to),
    ).toBe(false);
    expect(h.view.state.doc.toString()).toBe(before);
    h.destroy();
  });

  it("never hides the head line of what it folds", async () => {
    const h = await mountEditor(DOC);
    const head = h.view.state.doc.line(6);
    const range = foldableAt(h.view.state, head.from + 2);
    // The fold starts at the END of the head line, so `foreach …` stays visible.
    expect((range as { from: number }).from).toBe(head.to);
    h.destroy();
  });
});

describe("Document View", () => {
  const NARRATIVE = [
    "//: ## Wage regressions",
    "//: We drop the top 1%.",
    "regress lwage educ",
  ].join("\n");

  it("renders narrative runs and un-renders them without changing the file", async () => {
    const h = await mountEditor(NARRATIVE);
    const before = h.view.state.doc.toString();
    toggleDocumentView(h.view, true);
    expect(h.view.state.doc.toString()).toBe(before);
    toggleDocumentView(h.view, false);
    expect(h.view.state.doc.toString()).toBe(before);
    expect(counters.documentWrites).toBe(0);
    h.destroy();
  });
});

describe("the narrative renderer", () => {
  it("builds nodes rather than HTML, and carries a source offset on each", () => {
    const host = document.createElement("div");
    renderNarrative(host, [
      { text: "## Wage regressions", at: 10 },
      { text: "We drop the **top 1%** of `income`.", at: 40 },
      { text: "", at: 80 },
      { text: "- first", at: 90 },
      { text: "- second", at: 100 },
    ]);

    expect(host.querySelector("h3")?.textContent).toBe("Wage regressions");
    expect(host.querySelector("p strong")?.textContent).toBe("top 1%");
    expect(host.querySelector("p code")?.textContent).toBe("income");
    expect(host.querySelectorAll("li")).toHaveLength(2);
    expect(host.querySelector("h3")?.getAttribute("data-src")).toBe("10");
  });

  it("never interprets markup in the source as markup in the output", () => {
    const host = document.createElement("div");
    renderNarrative(host, [{ text: "<script>alert(1)</script> and <b>bold</b>", at: 0 }]);
    // Text, not elements. Nothing here assigns `innerHTML`, so a do-file cannot
    // put an element into the editor no matter what it contains.
    expect(host.querySelector("script")).toBeNull();
    expect(host.querySelector("b")).toBeNull();
    expect(host.textContent).toContain("<script>alert(1)</script>");
  });

  it("recognises both narrative prefixes and eats exactly one space", () => {
    expect(stripNarrativePrefix("//: hello")).toEqual({ text: "hello", skip: 4 });
    expect(stripNarrativePrefix("//:hello")).toEqual({ text: "hello", skip: 3 });
    expect(stripNarrativePrefix("// not narrative")).toBeNull();
  });

  it("maps a click on rendered prose back to a source offset", () => {
    const host = document.createElement("div");
    renderNarrative(host, [{ text: "**bold** text", at: 123 }]);
    const strong = host.querySelector("strong");
    expect(strong).not.toBeNull();
    expect(sourceOffsetForNode(strong)).toBe(123);
    expect(sourceOffsetForNode(null)).toBeNull();
  });
});
