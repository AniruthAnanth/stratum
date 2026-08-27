/**
 * Token consumption — IMPLEMENTATION_PLAN W12 acceptance:
 *
 *   "The contrast check over `design/tokens.json` asserts body text >= 7:1, meta
 *    >= 4.5:1, glyphs/rules >= 3:1 in all four themes; it runs in `xtask tokens`
 *    (W00) and W12 consumes the result."
 *
 * Consuming it means two obligations, and this file discharges both.
 *
 * FIRST, the frontend must not re-declare what the check governs. `xtask tokens`
 * can only police `design/tokens.json`; a hex literal in a stylesheet is outside
 * its reach and is exactly how an interface drifts from its own palette. So the
 * stylesheets are scanned here.
 *
 * SECOND, the ratios are recomputed rather than trusted. `xtask tokens` already
 * recomputes them in Rust; doing it again in TypeScript is not redundant, it is
 * the point — two independent implementations of the same WCAG formula agreeing
 * on the same palette is evidence, and one implementation agreeing with itself
 * is not.
 *
 * TWO PARTS OF THE BULLET ARE OPEN AGAINST `design/tokens.json`, which is W00's
 * file and cannot be repaired from here:
 *
 *   - "all four themes". 06 §14.5 promises "two full themes plus a high-contrast
 *     variant of each" and then specifies values for two. `light_hc`/`dark_hc`
 *     are absent rather than invented; every loop in this file iterates the
 *     themes the file actually ships, so all of them start checking the moment
 *     two more land.
 *
 *     Landing them is NOT, however, a pure data change, and the last describe
 *     block in this file exists because of that. Measured by running the
 *     generator over a four-theme source: `emit_rust` iterates
 *     `/color/themes` and emitted all four (`THEMES: [&Theme; 4]`), but
 *     `emit_css` binds only `themes.first()` and `themes.get(1)`, so
 *     `light_hc`/`dark_hc` were dropped from `tokens.generated.css` without a
 *     word — and `verify_contrast` loops the literal `["light", "dark"]`, so a
 *     `light_hc` body text deliberately set to 1.7:1 against a 7:1 floor was
 *     accepted, as were recorded ratios of 99.99 and 0.01. Both are in
 *     `xtask/src/tokens.rs`, which is W00's. What this unit CAN do is refuse to
 *     consume a stylesheet that is quietly missing a theme its own source
 *     declares, which is what that block asserts.
 *   - meta text ≥ 4.5:1. Light `--n7` `#8A9099` measures 3.08 on `--n1` canvas
 *     and 2.95 on `--n2` surface. The `_note` in `tokens.json` proposes
 *     `#767C85`; that value does not fix it (4.03 / 3.85, and only 4.21 even
 *     against pure white). Holding the token's HSL hue 216° and saturation
 *     0.069, the lightest value that clears 4.5:1 on BOTH light grounds is
 *     `#6B717B` — but it clears the `--n2` ground by 0.002 (4.502069), which is
 *     a floor that a later half-step on `--n2` would silently drop back under.
 *     `#696F79`, two steps further down the same line, is the value to ship:
 *     4.85 / 4.64, still lighter than `--n8` `#5C636D` so the ramp keeps its
 *     order. Dark `--n7` `#7C8794` already clears (4.88 / 4.59) and does not
 *     move. Only `semantic.text_meta` carries `ref: n7`, so the repair is two
 *     hex values in the light theme plus moving both `text_meta` rows from
 *     `known_exceptions` to `enforced`. `state.interrupted` happens to share the
 *     current hex but carries no `ref` and is a GLYPH at a 3:1 floor, so it
 *     stays `#8A9099` and simply stops aliasing `--n7`.
 *
 * Neither is asserted here as a defect. See "carries no waiver the palette no
 * longer needs" for why a test must never pin the bad number.
 */

import { existsSync, readFileSync, readdirSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { describe, expect, it } from "vitest";

const repoRoot = ((): string => {
  let dir = process.cwd();
  while (!existsSync(join(dir, "design/tokens.json"))) {
    const parent = dirname(dir);
    if (parent === dir) throw new Error("design/tokens.json not found above the cwd");
    dir = parent;
  }
  return dir;
})();

interface TokenValue {
  value: string;
}
interface Tokens {
  color: {
    themes: Record<string, Record<string, Record<string, TokenValue>>>;
    _deferred?: string;
  };
  a11y: {
    min_contrast: Record<string, number | string>;
    enforced: { fg: string; bg: string; min: number; measured: Record<string, number> }[];
    known_exceptions: {
      fg: string;
      bg: string;
      policy_min: number;
      measured: Record<string, number>;
      _note?: string;
    }[];
  };
}

const tokens = JSON.parse(readFileSync(resolve(repoRoot, "design/tokens.json"), "utf8")) as Tokens;

// ---------------------------------------------------------------------------
// WCAG 2.1 relative-luminance contrast, written from the spec, not ported.
// ---------------------------------------------------------------------------

function channel(v: number): number {
  const s = v / 255;
  return s <= 0.03928 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
}

function luminance(hex: string): number {
  const h = hex.replace("#", "");
  const r = Number.parseInt(h.slice(0, 2), 16);
  const g = Number.parseInt(h.slice(2, 4), 16);
  const b = Number.parseInt(h.slice(4, 6), 16);
  return 0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b);
}

function contrast(a: string, b: string): number {
  const la = luminance(a);
  const lb = luminance(b);
  return (Math.max(la, lb) + 0.05) / (Math.min(la, lb) + 0.05);
}

/** `state.ok` -> themes[t].state.ok; a bare name is searched across groups. */
function colorOf(theme: string, name: string): string {
  const groups = tokens.color.themes[theme];
  if (groups === undefined) throw new Error(`no theme ${theme}`);
  if (name.includes(".")) {
    const [group, key] = name.split(".") as [string, string];
    const value = groups[group]?.[key]?.value;
    if (value === undefined) throw new Error(`no colour ${name} in ${theme}`);
    return value;
  }
  for (const group of Object.values(groups)) {
    const value = group[name]?.value;
    if (value !== undefined) return value;
  }
  throw new Error(`no colour ${name} in ${theme}`);
}

const themes = Object.keys(tokens.color.themes);

describe("the contrast policy over design/tokens.json", () => {
  it("recomputes every enforced pair and clears its floor", () => {
    for (const entry of tokens.a11y.enforced) {
      for (const theme of themes) {
        const actual = contrast(colorOf(theme, entry.fg), colorOf(theme, entry.bg));
        expect(actual, `${entry.fg} on ${entry.bg} (${theme})`).toBeGreaterThanOrEqual(entry.min);
        // And the number recorded in the file is the number, to 0.01 - a
        // recorded measurement that has drifted from the palette is a lie that
        // outlives the palette change that caused it.
        const recorded = entry.measured[theme];
        if (recorded !== undefined) expect(actual).toBeCloseTo(recorded, 1);
      }
    }
  });

  it("uses the floors 06 §14.5 states", () => {
    expect(tokens.a11y.min_contrast["body_text"]).toBe(7.0);
    expect(tokens.a11y.min_contrast["meta_text"]).toBe(4.5);
    expect(tokens.a11y.min_contrast["glyph"]).toBe(3.0);
    expect(tokens.a11y.min_contrast["rule"]).toBe(3.0);
  });

  it("clears 7:1 for body text on both of its surfaces", () => {
    for (const theme of themes) {
      for (const bg of ["canvas", "surface"]) {
        expect(
          contrast(colorOf(theme, "text_body"), colorOf(theme, bg)),
          `${theme}/${bg}`,
        ).toBeGreaterThanOrEqual(7);
      }
    }
  });

  it("records every below-floor pair as an exception with a stated reason", () => {
    for (const entry of tokens.a11y.known_exceptions) {
      for (const theme of themes) {
        const recorded = entry.measured[theme];
        if (recorded === undefined) continue;
        expect(
          contrast(colorOf(theme, entry.fg), colorOf(theme, entry.bg)),
          `${entry.fg} on ${entry.bg} (${theme})`,
        ).toBeCloseTo(recorded, 1);
      }
      expect(entry._note ?? "", `${entry.fg} on ${entry.bg} is waived without a reason`).toMatch(
        /\S/,
      );
    }
  });

  it("carries no waiver the palette no longer needs", () => {
    // Stated as "no UNNECESSARY waiver", never as "this pair is below its
    // floor". The distinction is the whole point: the light `--n7` entry is
    // labelled a DEFECT, and a test that pinned its bad ratio would have to be
    // rewritten by whoever repairs the palette — which is precisely how a defect
    // becomes permanent. In this form the repair lands in `design/tokens.json`
    // (W00's file) alone: darken the token, move the pair into `enforced`, and
    // this suite goes green on the strong path with no edit here.
    for (const entry of tokens.a11y.known_exceptions) {
      const worst = Math.min(
        ...themes.map((t) => contrast(colorOf(t, entry.fg), colorOf(t, entry.bg))),
      );
      expect(
        worst,
        `${entry.fg} on ${entry.bg} now clears ${entry.policy_min} in every theme; delete the waiver`,
      ).toBeLessThan(entry.policy_min);
    }
  });

  it("meters meta text against both of its surfaces, whichever list holds the pair", () => {
    // Without this, deleting the two `text_meta` rows from `known_exceptions`
    // would make the 4.5:1 floor vanish rather than pass. The pair must be filed
    // somewhere, at the policy number, and if it is filed as `enforced` it is
    // held to that number in every theme this file ships.
    for (const bg of ["canvas", "surface"]) {
      const held = tokens.a11y.enforced.find((e) => e.fg === "text_meta" && e.bg === bg);
      const waived = tokens.a11y.known_exceptions.find((e) => e.fg === "text_meta" && e.bg === bg);
      expect(held ?? waived, `text_meta on ${bg} is metered by neither list`).toBeDefined();
      expect(held?.min ?? waived?.policy_min).toBe(tokens.a11y.min_contrast["meta_text"]);
      if (held === undefined) continue;
      for (const theme of themes) {
        expect(
          contrast(colorOf(theme, "text_meta"), colorOf(theme, bg)),
          `${theme}/${bg}`,
        ).toBeGreaterThanOrEqual(4.5);
      }
    }
  });

  it("covers all four themes, or says in the file why it does not", () => {
    // The acceptance bullet asks for four themes. `design/tokens.json` ships two
    // and records the reason: 06 §14.5 promises a high-contrast variant of each
    // but gives no values, so `light_hc` and `dark_hc` are absent rather than
    // invented. This assertion passes today on the recorded deferral and starts
    // checking the other two the moment somebody adds them.
    if (themes.length === 4) {
      expect(new Set(themes)).toEqual(new Set(["light", "dark", "light_hc", "dark_hc"]));
    } else {
      expect(themes).toEqual(["light", "dark"]);
      expect(tokens.color._deferred, "the missing themes must be explained in the file").toMatch(
        /high-contrast/,
      );
    }
  });
});

describe("the stylesheets consume tokens rather than re-declaring colour", () => {
  const stylesDir = resolve(repoRoot, "apps/desktop/src/styles");
  const files: string[] = readdirSync(stylesDir).filter((f: string) => f.endsWith(".css"));

  it("has the stylesheets this unit ships", () => {
    expect(new Set(files)).toEqual(
      new Set(["base.css", "type.css", "chrome.css", "dock.css", "tables.css", "print.css"]),
    );
  });

  it.each(files.filter((f) => f !== "print.css"))(
    "%s declares no colour literal",
    (file: string) => {
      const source = readFileSync(join(stylesDir, file), "utf8");
      // Comments may name a colour when explaining why it is banned (`#1E1E1E` is
      // explicitly not our dark canvas), so they are stripped before scanning.
      const code = source.replace(/\/\*[\s\S]*?\*\//g, "");
      expect(code, `${file} contains a hex literal`).not.toMatch(/#[0-9a-fA-F]{3,8}\b/);
      expect(code, `${file} contains an rgb()/hsl() literal`).not.toMatch(/\b(rgba?|hsla?)\(/);
    },
  );

  it("print.css is the one deliberate exception", () => {
    // 06 §14.5: the print scheme is "pure white ground, black ink, unmodified
    // Okabe-Ito. Never depends on the user's app theme." Pinning the neutral
    // ends for print is the whole job, so it necessarily names them.
    const source = readFileSync(join(stylesDir, "print.css"), "utf8");
    expect(source).toMatch(/@media print/);
    expect(source).toMatch(/#fff/);
  });

  it("base.css imports the generated tokens and nothing re-declares them", () => {
    const base = readFileSync(join(stylesDir, "base.css"), "utf8");
    expect(base).toContain('@import "../../resources/tokens.generated.css"');
    // print.css is excluded for the same reason as above: overriding the role
    // tokens inside `@media print` IS its job, and it does it in one block.
    for (const file of files.filter((f) => f !== "print.css")) {
      const source = readFileSync(join(stylesDir, file), "utf8").replace(/\/\*[\s\S]*?\*\//g, "");
      // `--n0`..`--n11` and the semantic roles belong to the generated file.
      expect(source, `${file} re-declares a palette token`).not.toMatch(
        /^\s*--(n\d{1,2}|accent|canvas|surface|overlay|text-body|text-meta|state-\w+)\s*:/m,
      );
    }
  });
});

// ---------------------------------------------------------------------------
// The artifact this unit actually renders from.
// ---------------------------------------------------------------------------

/**
 * Everything above meters `design/tokens.json`. The frontend never reads that
 * file at runtime — it renders from `resources/tokens.generated.css`, and a
 * theme that clears 7:1 in the source is worth nothing if it never reached the
 * stylesheet. `xtask tokens --check` compares the artifact against a fresh
 * generation, so it cannot see a theme the generator never emits in the first
 * place: both sides agree, and both are short. Checking that here is not
 * duplicated effort, it is the only place the omission is visible.
 */
describe("the generated stylesheet carries every theme the source declares", () => {
  const generated = readFileSync(
    resolve(repoRoot, "apps/desktop/resources/tokens.generated.css"),
    "utf8",
  );

  it.each(themes)("%s is selectable by name", (theme: string) => {
    // Light is the unconditional `:root`, but it still appears by name in the
    // `:not([data-theme="light"])` guard that lets an explicit choice pin it,
    // so every theme owes this literal regardless of which slot it occupies.
    const why = [
      `design/tokens.json declares ${theme} but tokens.generated.css has no`,
      `[data-theme="${theme}"] selector. emit_css in xtask/src/tokens.rs (W00) binds`,
      "themes.first() and themes.get(1) only; a third or fourth theme is dropped",
      "silently.",
    ].join(" ");
    expect(generated, why).toContain(`[data-theme="${theme}"]`);
  });

  it.each(themes)("%s's palette reached the stylesheet", (theme: string) => {
    // Hex presence is a proxy for "this theme's block was emitted": two themes
    // sharing a body-text value would weaken it, and no two here do.
    const body = colorOf(theme, "text_body");
    const why = [
      `${theme} declares text_body ${body} in design/tokens.json, and no`,
      "--text-body in tokens.generated.css carries it",
    ].join(" ");
    expect(generated, why).toContain(`--text-body: ${body};`);
  });

  it("declares color-scheme only with values CSS defines", () => {
    // `theme_block` writes `color-scheme: {theme_id}` — the theme's KEY, not a
    // CSS keyword. That is correct only for as long as every key happens to be
    // spelled `light` or `dark`; a key of `light_hc` would emit
    // `color-scheme: light_hc`, which no browser honours and no CSS parser
    // rejects loudly. Whoever widens emit_css needs a theme -> color-scheme
    // mapping, and this is what will tell them.
    // Anchored to a declaration's own line so the `prefers-color-scheme` media
    // query — which ends in `)`, not `;` — is not swept up with it.
    const declared = [...generated.matchAll(/^\s*color-scheme:\s*([^;]+);/gm)].map((m) =>
      (m[1] ?? "").trim(),
    );
    expect(declared.length, "the generated stylesheet sets no color-scheme").toBeGreaterThan(0);
    for (const value of declared) {
      expect(["normal", "light", "dark", "light dark", "dark light"]).toContain(value);
    }
  });
});
