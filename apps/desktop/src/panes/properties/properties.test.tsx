/**
 * The Properties pane's acceptance bullet — plan W16, 06 §9.5:
 *
 * > **Properties edits issue real Stata commands** (`label variable`, `format`,
 * > `rename`, `note`) that appear in Results, History and the log. This is a
 * > reproducibility feature, not a legacy quirk.
 *
 * So the assertion is on the *command text*, at the one boundary every command
 * in this product crosses (`submitCommand`). An edit that mutated a store and
 * left the log alone would show the same value in the same box and fail every
 * one of these.
 */

import { render } from "solid-js/web";
import { afterEach, beforeEach, describe, expect, test } from "vitest";
import { resetCommandBarHandle } from "../../commandbar/handle";
import { recordedSubmissions, resetSubmitState } from "../../commandbar/submit";
import { type VariableRow, resetVarState } from "../../state/vars";
import { resetSelectionState, selectOnly, variableSelection } from "../variables/selection";
import {
  commandFor,
  formatCommand,
  isDisplayFormat,
  isStataName,
  labelDataCommand,
  labelVariableCommand,
  noteCommand,
  quoteStataString,
  renameCommand,
  valueLabelCommand,
} from "./edits";
import { PropertiesPane, propertiesCounters, resetPropertiesCounters } from "./index";

const roots: (() => void)[] = [];

const AUTO: VariableRow[] = [
  { name: "make", storage: "str18", format: "%-18s", label: "Make and model" },
  { name: "price", storage: "int", format: "%8.0gc", label: "Price" },
  { name: "mpg", storage: "int", format: "%8.0g", label: "Mileage (mpg)" },
];

function mount(props: Parameters<typeof PropertiesPane>[0] = {}): HTMLElement {
  const host = document.createElement("div");
  document.body.append(host);
  roots.push(render(() => <PropertiesPane rows={AUTO} {...props} />, host));
  return host;
}

const field = (host: HTMLElement, name: string): HTMLInputElement => {
  const input = host.querySelector<HTMLInputElement>(`[data-properties-field="${name}"]`);
  if (input === null) throw new Error(`no ${name} field`);
  return input;
};

/** Type into a field and commit it, which is what `change` means for an input. */
function edit(input: HTMLInputElement, value: string): void {
  input.value = value;
  input.dispatchEvent(new Event("input", { bubbles: true }));
  input.dispatchEvent(new Event("change", { bubbles: true }));
}

function unlock(host: HTMLElement): void {
  host.querySelector<HTMLElement>("[data-properties-lock]")?.click();
}

beforeEach(() => {
  resetVarState();
  resetSubmitState();
  resetSelectionState();
  resetPropertiesCounters();
  selectOnly("price");
});

afterEach(() => {
  while (roots.length > 0) roots.pop()?.();
  document.body.replaceChildren();
  resetCommandBarHandle();
  resetSelectionState();
});

// ---------------------------------------------------------------------------
// The command composition
// ---------------------------------------------------------------------------

describe("every edit is a command a user could have typed (06 §9.5)", () => {
  test("the four verbs the bullet names", () => {
    expect(labelVariableCommand("price", "Price in 1978 dollars")).toBe(
      'label variable price "Price in 1978 dollars"',
    );
    expect(formatCommand("price", "%9.0gc")).toBe("format price %9.0gc");
    expect(renameCommand("price", "cost")).toBe("rename price cost");
    expect(noteCommand("price", "deflated to 1978")).toBe("note price: deflated to 1978");
    expect(noteCommand(undefined, "from the 1978 file")).toBe("note: from the 1978 file");
    expect(labelDataCommand("1978 automobile data")).toBe('label data "1978 automobile data"');
    expect(valueLabelCommand("foreign", "origin")).toBe("label values foreign origin");
  });

  test("clearing a label removes it rather than setting it to empty", () => {
    // Two different states in the `.dta`, and `describe` shows them differently.
    expect(labelVariableCommand("price", "")).toBe("label variable price");
    expect(valueLabelCommand("foreign", "")).toBe("label values foreign .");
    expect(labelDataCommand("")).toBe("label data");
  });

  test("a label containing a double quote uses Stata's compound quote", () => {
    expect(quoteStataString("plain")).toBe('"plain"');
    // An apostrophe is harmless and must not be mangled.
    expect(quoteStataString("driver's seat")).toBe('"driver\'s seat"');
    expect(quoteStataString('he said "hi"')).toBe('`"he said "hi""\'');
    // A newline cannot survive a command line, so it becomes a space rather
    // than an rc-198 the user cannot see the cause of.
    expect(quoteStataString("two\nlines")).toBe('"two lines"');
  });

  test("refusals are for edits that cannot become a command, never for rc", () => {
    expect(commandFor({ field: "name", variable: "price", value: "9lives" })).toEqual({
      ok: false,
      reason: "“9lives” is not a variable name",
    });
    expect(commandFor({ field: "format", variable: "price", value: "9.0g" }).ok).toBe(false);
    expect(commandFor({ field: "note", variable: "price", value: "   " }).ok).toBe(false);

    // `rename price mpg` collides — and is still a real command with a real
    // `r(110)`, which the user must see in Results rather than in a tooltip.
    expect(commandFor({ field: "name", variable: "price", value: "mpg" })).toEqual({
      ok: true,
      command: "rename price mpg",
    });
  });

  test("the name and format rules", () => {
    expect(isStataName("_merge")).toBe(true);
    expect(isStataName("v1")).toBe(true);
    expect(isStataName("1v")).toBe(false);
    expect(isStataName("has space")).toBe(false);
    expect(isStataName("a".repeat(33))).toBe(false);
    expect(isDisplayFormat("%8.0gc")).toBe(true);
    expect(isDisplayFormat("%-18s")).toBe(true);
    expect(isDisplayFormat("%td")).toBe(true);
    expect(isDisplayFormat("8.0g")).toBe(false);
  });
});

// ---------------------------------------------------------------------------
// The padlock
// ---------------------------------------------------------------------------

describe("the padlock ([GSM] 2)", () => {
  test("locked by default: an edit issues nothing and says why", () => {
    const host = mount();
    expect(field(host, "label").disabled).toBe(true);

    // Even driven directly, a locked pane refuses.
    const input = field(host, "label");
    input.disabled = false;
    edit(input, "Cost");
    expect(recordedSubmissions()).toHaveLength(0);
    expect(propertiesCounters.refused).toBe(1);
    expect(host.querySelector("[data-properties-refusal]")?.textContent).toContain("locked");
  });

  test("unlocking permits editing, and the edit is a command", async () => {
    const host = mount();
    unlock(host);
    expect(field(host, "label").disabled).toBe(false);

    edit(field(host, "label"), "Price in 1978 dollars");
    await new Promise((resolve) => setTimeout(resolve, 0));

    expect(recordedSubmissions().map((s) => s.text)).toEqual([
      'label variable price "Price in 1978 dollars"',
    ]);
    expect(propertiesCounters.commands).toBe(1);
  });

  test("renaming and reformatting go through the same path", async () => {
    const host = mount();
    unlock(host);
    edit(field(host, "name"), "cost");
    edit(field(host, "format"), "%9.2f");
    await new Promise((resolve) => setTimeout(resolve, 0));

    expect(recordedSubmissions().map((s) => s.text)).toEqual([
      "rename price cost",
      "format price %9.2f",
    ]);
  });

  test("a note is added, not replaced", async () => {
    const host = mount();
    unlock(host);
    const input = host.querySelector<HTMLInputElement>("[data-properties-note]");
    if (input === null) throw new Error("no note field");
    input.value = "deflated to 1978 dollars";
    input.dispatchEvent(new Event("input", { bubbles: true }));
    input.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    await new Promise((resolve) => setTimeout(resolve, 0));

    expect(recordedSubmissions().map((s) => s.text)).toEqual([
      "note price: deflated to 1978 dollars",
    ]);
    // The field empties: the next note is a new note, never an edit of this one.
    expect(input.value).toBe("");
  });

  test("an unchanged field issues nothing at all", () => {
    const host = mount();
    unlock(host);
    edit(field(host, "label"), "Price");
    expect(recordedSubmissions()).toHaveLength(0);
    expect(propertiesCounters.commands).toBe(0);
  });
});

// ---------------------------------------------------------------------------
// The arrows and the multi-selection rule
// ---------------------------------------------------------------------------

describe("following the Variables pane ([GSM] 2)", () => {
  test("◀ ▶ move the primary selection and the pane follows", () => {
    const host = mount();
    host.querySelector<HTMLElement>("[data-properties-next]")?.click();
    expect(variableSelection().primary).toBe("mpg");
    expect(field(host, "name").value).toBe("mpg");
    expect(propertiesCounters.steps).toBe(1);

    host.querySelector<HTMLElement>("[data-properties-previous]")?.click();
    expect(variableSelection().primary).toBe("price");
    expect(field(host, "label").value).toBe("Price");
  });

  test("stepping is not an edit: no command is issued", () => {
    const host = mount();
    unlock(host);
    host.querySelector<HTMLElement>("[data-properties-next]")?.click();
    host.querySelector<HTMLElement>("[data-properties-previous]")?.click();
    expect(recordedSubmissions()).toHaveLength(0);
  });

  test("the Data section shows what the wire can answer", () => {
    const host = mount({ frame: "default" });
    expect(host.querySelector("[data-properties-frame]")?.textContent).toBe("default");
    expect(host.querySelector("[data-properties-nvars]")?.textContent).toBe("3");
    // Filename/Size/Memory have no wire field, so they are not drawn at all
    // rather than drawn as an em dash that reads as "this dataset has none".
    expect(host.textContent).not.toContain("Filename");
    expect(host.textContent).not.toContain("Memory");
  });

  test("host-supplied facts are drawn when they exist", () => {
    const host = mount({
      data: { filename: "auto.dta", observations: 74, sortedBy: ["foreign", "make"] },
    });
    expect(host.textContent).toContain("auto.dta");
    expect(host.querySelector("[data-properties-sorted]")?.textContent).toBe("foreign make");
  });
});
