/**
 * The source-level rules W14's acceptance states as CI greps.
 *
 * MECHANISM SUBSTITUTION — DECLARED, PENDING AN ARCHITECT RULING.
 * W14's acceptance says "a CI grep asserts no `toFixed`, `toPrecision`,
 * `toExponential` or `Intl.NumberFormat` appears anywhere under
 * `apps/desktop/src/renderers/`". That names a step in
 * `.github/workflows/ci.yml`, which is **W00's file** — R0 forbids this unit
 * writing it, so the rule is implemented here instead and the ci.yml step is
 * escalated rather than reached across for.
 *
 * The substitution is enforced, not weaker: ci.yml's `frontend` job runs
 * `pnpm vitest run --coverage`, which runs this file, and `frontend` is a
 * required check. It is also a strict superset of the named grep — it reads the
 * same bytes off disk, covers the two panes the unit owns as well as
 * `renderers/`, bans two escape hatches the four named tokens miss (see below),
 * excuses a token named inside a comment, and quotes `file:line: source` on a
 * hit instead of a bare exit code.
 *
 * These are tests rather than a shell script for the reason `platform/
 * bridge.test.ts` already established in this codebase: a grep that lives beside
 * the code it polices runs on every `pnpm test`, fails with the offending line
 * quoted, and cannot be forgotten when the CI file is edited by another unit.
 */

import { readFileSync, readdirSync, statSync } from "node:fs";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, test } from "vitest";

const here = dirname(fileURLToPath(import.meta.url));
const srcRoot = resolve(here, "..");

function walk(dir: string): string[] {
  const out: string[] = [];
  for (const entry of readdirSync(dir)) {
    const path = join(dir, entry);
    if (statSync(path).isDirectory()) out.push(...walk(path));
    else out.push(path);
  }
  return out;
}

/** Everything this unit owns: `renderers/**`, `panes/results/**`, `panes/compare/**`. */
const OWNED = [
  join(srcRoot, "renderers"),
  join(srcRoot, "panes/results"),
  join(srcRoot, "panes/compare"),
].flatMap(walk);

const code = OWNED.filter((f) => /\.tsx?$/.test(f));
const styles = OWNED.filter((f) => f.endsWith(".css"));
const shipped = code.filter((f) => !/\.test\.tsx?$/.test(f) && !f.endsWith("fixtures.ts"));

function lines(file: string): { n: number; text: string }[] {
  return readFileSync(file, "utf8")
    .split("\n")
    .map((text, i) => ({ n: i + 1, text }));
}

/** A hit outside a comment. A rule named in a comment is documentation. */
function offences(files: readonly string[], pattern: RegExp): string[] {
  const out: string[] = [];
  for (const file of files) {
    for (const { n, text } of lines(file)) {
      const stripped = text.replace(/^\s*(\/\/|\*|\/\*).*$/, "");
      if (pattern.test(stripped)) out.push(`${relative(srcRoot, file)}:${n}: ${text.trim()}`);
    }
  }
  return out;
}

describe("'Renderers never reformat numbers'", () => {
  test("no toFixed, toPrecision, toExponential or Intl.NumberFormat, anywhere", () => {
    // The acceptance names `apps/desktop/src/renderers/`; the two panes this unit
    // owns are held to it too, because a comparison table that rounds
    // differently from the card beside it is the same defect one file over.
    //
    // The pattern is a deliberate SUPERSET of the four tokens the acceptance
    // enumerates, because two ways of reformatting a number slip past a literal
    // grep for those four:
    //   * `toLocaleString` — `(6165.257).toLocaleString()` is "6,165.257", a
    //     different string from the golden's `6165.257`, and under a non-en
    //     locale it is "6.165,257". Same defect class, not in the named list.
    //   * `const { NumberFormat } = Intl` — destructuring hides the member
    //     access a `Intl\.NumberFormat` grep is keyed on. `Intl` has no
    //     legitimate use in a unit that consumes pre-formatted strings, so the
    //     whole global is banned rather than the one property.
    //
    // Scanned over SHIPPED files first, then over the tests too. A grep for a
    // forbidden token cannot also forbid the file that spells the token in
    // order to forbid it, so the exemption is named and then bounded: outside
    // comments, the only file in the whole unit that mentions any of them is
    // this one.
    const banned = /\b(toFixed|toPrecision|toExponential|toLocaleString|Intl)\b/;
    expect(offences(shipped, banned)).toEqual([]);
    const everywhere = offences(code, banned).map((hit) => hit.split(":")[0]);
    expect([...new Set(everywhere)]).toEqual(["renderers/contract.test.ts"]);
  });

  test("the corpus is the whole unit — a grep over nothing passes trivially", () => {
    // Named structurally rather than as a `>= 18` magic number: the acceptance
    // owns eight renderer families and two panes by name, so the guard asserts
    // those exact entry points were walked. A family deleted, renamed, or moved
    // out from under `walk()` fails here instead of quietly shrinking the scan.
    const entries = new Set(shipped.map((f) => relative(srcRoot, f)));
    for (const family of [
      "summarize",
      "estimation",
      "tabulate",
      "graph",
      "error",
      "table",
      "raw",
      "log",
    ]) {
      expect(entries, `renderers/${family} was not scanned`).toContain(
        `renderers/${family}/index.tsx`,
      );
    }
    expect(entries).toContain("panes/results/index.tsx");
    expect(entries).toContain("panes/compare/index.tsx");
    // Every family also ships its stylesheet, and the stylesheet scans below
    // (keyframes, hex literals, radius, box-shadow) are only meaningful if it
    // is in `styles`.
    expect(styles.length).toBeGreaterThanOrEqual(9);
  });

  test("no renderer reads a numeric field that has a `display` sibling", () => {
    // The structural views are the enforcement: `SummarizeRowView` does not even
    // declare `mean`, `sd`, `min`, `max` or `obs`, so `row.mean` is a type error
    // rather than a formatting decision. This asserts the views stayed that way.
    const views = readFileSync(join(srcRoot, "renderers/types.ts"), "utf8");
    const summarizeRow = views.slice(
      views.indexOf("interface SummarizeRowView"),
      views.indexOf("interface SummarizePayloadView"),
    );
    for (const field of ["mean:", "sd:", "min:", "max:", "obs:", "sum:"]) {
      expect(summarizeRow, `SummarizeRowView must not declare ${field}`).not.toContain(field);
    }
    const anova = views.slice(
      views.indexOf("interface AnovaTableView"),
      views.indexOf("interface ModelFlagView"),
    );
    for (const field of ["mss:", "rss:", "tss:", "df_m:", "ms_m:"]) {
      expect(anova, `AnovaTableView must not declare ${field}`).not.toContain(field);
    }
  });
});

describe("'The action row is data, not markup' (A22)", () => {
  const LABELS = [
    "Raw ▸",
    "Copy",
    "Export ▸",
    "Hide output",
    "Plot coefficients",
    "Run margins",
    "Compare ▸",
    "Diagnostics ▸",
    "Explain",
    "Check model",
    "Suggest next step",
  ];

  test("no renderer contains a hardcoded action label", () => {
    const others = shipped.filter((f) => !f.endsWith("renderers/actions.tsx"));
    const hits: string[] = [];
    for (const file of others) {
      for (const { n, text } of lines(file)) {
        const stripped = text.replace(/^\s*(\/\/|\*|\/\*).*$/, "");
        for (const label of LABELS) {
          if (stripped.includes(`"${label}"`) || stripped.includes(`>${label}<`)) {
            hits.push(`${relative(srcRoot, file)}:${n}: ${text.trim()}`);
          }
        }
      }
    }
    expect(hits).toEqual([]);
  });

  test("`actions.tsx` is the only file that knows a label at all", () => {
    const actions = readFileSync(join(srcRoot, "renderers/actions.tsx"), "utf8");
    for (const label of LABELS) expect(actions).toContain(`"${label}"`);
  });
});

describe("'Cards appear with zero animation'", () => {
  test("the only @keyframes in the unit is the indeterminate hairline shuttle", () => {
    const found: string[] = [];
    for (const file of styles) {
      for (const { text } of lines(file)) {
        const match = /@keyframes\s+([\w-]+)/.exec(text);
        if (match?.[1] !== undefined) found.push(match[1]);
      }
    }
    expect(found).toEqual(["stratum-hairline-shuttle"]);
  });

  test("nothing transitions opacity, transform or a card's appearance", () => {
    const hits = offences(styles, /transition:\s*(?!height\b|none\b)\S/);
    expect(hits).toEqual([]);
  });

  test("the only `animation` is on the hairline, and reduced motion stops it", () => {
    const card = readFileSync(join(srcRoot, "renderers/card.css"), "utf8");
    const animated = card
      .split("\n")
      .filter((l) => /^\s*animation:/.test(l))
      .map((l) => l.trim());
    expect(animated).toEqual([
      "animation: stratum-hairline-shuttle var(--motion-shuttle) linear infinite;",
      "animation: none;",
    ]);
    expect(card).toContain("@media (prefers-reduced-motion: reduce)");
  });
});

describe("stale rendering is a stylesheet fact, not a component guess (spec §13)", () => {
  const card = readFileSync(join(srcRoot, "renderers/card.css"), "utf8");

  test("the body dims to .62 and the header does not dim at all", () => {
    expect(card).toMatch(/\.card\[data-stale\]\s+\.card__body\s*\{[^}]*opacity:\s*0\.62/);
    expect(card).not.toMatch(/\.card__header\s*\{[^}]*opacity:/);
  });

  test("the stale rail is dashed, so colour is not the only channel (06 §17)", () => {
    expect(card).toMatch(/\.card\[data-stale\]\s+\.card__rail\s*\{[^}]*repeating-linear-gradient/);
    expect(card).toContain("var(--state-stale)");
  });
});

describe("06 §14: the palette is the product's, and the shapes are the product's", () => {
  test("no hex colour literal in any stylesheet this unit owns", () => {
    expect(offences(styles, /#[0-9a-fA-F]{3,8}\b/)).toEqual([]);
  });

  test("no radius above 3px, no shadow on a result surface (06 §14.4)", () => {
    expect(offences(styles, /border-radius:\s*(?!0\b|var\(--radius-)\S/)).toEqual([]);
    expect(offences(styles, /box-shadow:/)).toEqual([]);
  });

  test("no emoji and no icon font in the product UI (06 §14.7)", () => {
    // The card's `⋯`, `▸` and `·` are typographic marks, not emoji; the test
    // targets the pictographic range that §14.7 rules out.
    expect(offences(shipped, /\p{Extended_Pictographic}/u)).toEqual([]);
  });
});

describe("the frontend never parses SMCL, and never fetches an asset unauthenticated", () => {
  test("no renderer reaches for `{txt}`/`{res}` markup", () => {
    expect(offences(shipped, /\{(txt|res|com|err|hilite)\}/)).toEqual([]);
  });

  test("asset bytes go through `bridge().fetchAsset`, which carries the token", () => {
    const fetchers = shipped.filter((f) => /\bfetch\(/.test(readFileSync(f, "utf8")));
    expect(fetchers).toEqual([]);
    for (const file of [
      join(srcRoot, "renderers/raw/index.tsx"),
      join(srcRoot, "renderers/graph/index.tsx"),
    ]) {
      expect(readFileSync(file, "utf8")).toContain("bridge().fetchAsset");
    }
  });

  // The companion rule — "exactly one file in `src/` imports the Tauri API, and
  // it is `platform/bridge.ts`" — is W12's `platform/bridge.test.ts`, which
  // already scans every file including these. Restating it here would mean
  // spelling the scoped package name in this directory, which is precisely what
  // that test forbids: the duplicate check would itself be the violation.
});
