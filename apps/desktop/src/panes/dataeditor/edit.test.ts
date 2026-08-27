/**
 * "**Every edit issues `replace <var> = <val> in <n>`.**"
 *
 * The plan puts that bullet in bold and 06 §9.7 gives the reason —
 * "Reproducible by construction". A Data Editor that mutated the frame through
 * a private channel would produce a dataset nobody can rebuild from the
 * do-file, which is the exact failure the product exists to prevent.
 *
 * The literal is built from the RAW value and never from the displayed one, and
 * both halves of that sentence are checked here against the two oracles:
 * `auto_40x12_edit.bin` for what the value IS, `auto_40x12.bin` for what the
 * cell SHOWS. `price` observation 1 is `4099` and shows `4,099`; `foreign` is
 * `0` and shows `Domestic`. Sending either of the display strings is a syntax
 * error or a wrong value, so this file asserts that both are refused.
 */

import { describe, expect, test } from "vitest";
import { columnsFromVariables } from "../../grid/engine";
import { literalFor, replaceCommand, seedValue } from "./edit";
import { AUTO_VARS, autoDisplay, autoEdit } from "./harness";

const COLUMNS = columnsFromVariables(AUTO_VARS);
const column = (name: string): (typeof COLUMNS)[number] => {
  const found = COLUMNS.find((c) => c.name === name);
  if (found === undefined) throw new Error(`${name} is not a variable of auto.dta`);
  return found;
};

/** What `RenderMode::Edit` carries for one cell, as `PageSource.raw` would hand it over. */
function raw(
  idx: number,
  row: number,
): { kind: "text"; text: string } | { kind: "num"; value: number; tag: number } {
  const col = autoEdit.column(idx);
  if (col === undefined) throw new Error(`column ${idx} is missing from the edit page`);
  if (col.kind === "text") return { kind: "text", text: col.cell(row) };
  if (col.kind === "num") {
    return { kind: "num", value: col.values[row] ?? Number.NaN, tag: col.tags[row] ?? 255 };
  }
  throw new Error("auto.dta has no strL");
}

/** What the same cell DISPLAYS, from the display oracle. */
function shown(idx: number, row: number): string {
  const col = autoDisplay.column(idx);
  if (col === undefined || col.kind !== "text") throw new Error(`column ${idx} is not text`);
  return col.cell(row);
}

describe("the command is built from the raw value, not the displayed one", () => {
  test("price observation 1: the value is 4099 and the cell says 4,099", () => {
    expect(shown(1, 0)).toBe("4,099");
    const seed = seedValue(raw(1, 0), shown(1, 0));
    expect(seed).toBe("4099");

    const outcome = replaceCommand(column("price"), 1, seed);
    expect(outcome).toEqual({
      ok: true,
      literal: "4099",
      command: "replace price = 4099 in 1",
    });

    // And the display string is refused rather than sent: `replace price =
    // 4,099` parses as two arguments and is a syntax error.
    expect(replaceCommand(column("price"), 1, "4,099").ok).toBe(false);
  });

  test("foreign observation 1: the value is 0 and the cell says Domestic", () => {
    expect(shown(11, 0)).toBe("Domestic");
    const seed = seedValue(raw(11, 0), shown(11, 0));
    expect(seed).toBe("0");
    expect(replaceCommand(column("foreign"), 1, seed)).toEqual({
      ok: true,
      literal: "0",
      command: "replace foreign = 0 in 1",
    });

    const refused = replaceCommand(column("foreign"), 1, "Domestic");
    expect(refused.ok).toBe(false);
    // The refusal names the value label, because "Domestic is not a number" is
    // true and unhelpful; the user needs to be told to type the code.
    expect(refused.ok === false ? refused.reason : "").toContain("origin");
  });

  test("a missing rep78 seeds as its token and replaces with it", () => {
    // README §3: `rep78` is missing at observations 3 and 7.
    expect(shown(3, 2)).toBe(".");
    const cell = raw(3, 2);
    expect(cell.kind === "num" ? cell.tag : -1).toBe(0);
    const seed = seedValue(cell, shown(3, 2));
    expect(seed).toBe(".");
    expect(replaceCommand(column("rep78"), 3, seed).ok).toBe(true);
    expect(replaceCommand(column("rep78"), 3, seed)).toEqual({
      ok: true,
      literal: ".",
      command: "replace rep78 = . in 3",
    });
  });

  test("a float widens through its exact f32 value, and the command carries it", () => {
    // Fixture README §3 / 04 §2.6: `gear_ratio` obs 1 is 3.5799999237060547.
    const cell = raw(10, 0);
    expect(cell.kind === "num" ? cell.value : 0).toBe(3.5799999237060547);
    expect(shown(10, 0)).toBe("3.58");
    const seed = seedValue(cell, shown(10, 0));
    expect(replaceCommand(column("gear_ratio"), 1, seed).ok).toBe(true);
    expect(replaceCommand(column("gear_ratio"), 1, seed)).toMatchObject({
      command: "replace gear_ratio = 3.5799999237060547 in 1",
    });
  });

  test("a string variable is quoted, and its raw bytes are its display text", () => {
    // README §2.4: for `make` the two renderings coincide, which is why the
    // column is byte-identical between the two fixtures.
    expect(raw(0, 0)).toEqual({ kind: "text", text: "AMC Concord" });
    expect(shown(0, 0)).toBe("AMC Concord");
    expect(replaceCommand(column("make"), 1, "AMC Concord")).toEqual({
      ok: true,
      literal: '"AMC Concord"',
      command: 'replace make = "AMC Concord" in 1',
    });
  });
});

describe("literals Stata would accept, and only those", () => {
  test("compound double quotes for a value containing a quote", () => {
    const outcome = literalFor(column("make"), 'The 6" one');
    // `"The 6" one"` truncates at the embedded quote and silently stores `The 6`.
    expect(outcome).toEqual({ ok: true, command: "", literal: '`"The 6" one"\'' });
  });

  test("a line break in a string is refused rather than smuggled", () => {
    expect(literalFor(column("make"), "two\nlines").ok).toBe(false);
  });

  test("an emptied numeric cell is a missing value, as in Stata's own editor", () => {
    expect(literalFor(column("price"), "   ")).toMatchObject({ ok: true, literal: "." });
  });

  test("the extended missings .a–.z survive", () => {
    for (const token of [".", ".a", ".z"]) {
      expect(literalFor(column("price"), token)).toMatchObject({ ok: true, literal: token });
    }
    expect(literalFor(column("price"), ".aa").ok).toBe(false);
  });

  test("the numeric forms Stata prints are all accepted", () => {
    for (const value of ["0", "-1", "+2", "3.5", ".5", "1e6", "-2.5E-3", "4099"]) {
      expect(literalFor(column("price"), value)).toMatchObject({ ok: true, literal: value });
    }
    for (const value of ["4,099", "1 2", "abc", "1/2", "0x10"]) {
      expect(literalFor(column("price"), value).ok).toBe(false);
    }
  });
});

describe("`in n` counts observations", () => {
  test("the observation number is 1-based and must be an integer", () => {
    expect(replaceCommand(column("price"), 0, "1").ok).toBe(false);
    expect(replaceCommand(column("price"), -3, "1").ok).toBe(false);
    expect(replaceCommand(column("price"), 1.5, "1").ok).toBe(false);
    expect(replaceCommand(column("price"), 10_000_000, "1")).toMatchObject({
      command: "replace price = 1 in 10000000",
    });
  });
});

describe("seeding the overlay", () => {
  test("an unresident cell falls back to the display text rather than to nothing", () => {
    expect(seedValue(undefined, "4,099")).toBe("4,099");
  });

  test("an extended missing seeds as its own token, not as an empty box", () => {
    // An empty field would silently offer to turn `.a` into plain `.`.
    expect(seedValue({ kind: "num", value: 0, tag: 1 }, "")).toBe(".a");
    expect(seedValue({ kind: "num", value: 0, tag: 26 }, "")).toBe(".z");
    expect(seedValue({ kind: "num", value: 0, tag: 0 }, "")).toBe(".");
  });
});
