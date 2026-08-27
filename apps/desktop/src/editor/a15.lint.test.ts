/**
 * A15's standing guard — **the document write fence**, machine-checked across
 * the whole frontend.
 *
 * W13's headline acceptance bullet has two halves. The behavioural half lives in
 * `editor.doc.test.ts`: 200 random runs, collapses, expands and detaches, and
 * `doc.toString()` byte-identical afterwards. That half proves the code we have
 * does not write. It cannot prove that the code we get NEXT year does not, and
 * that is what the bullet's second clause asks for:
 *
 * > Only `doc_save`, `section_rename`, `section_move` and an accepted AI diff
 * > may write, all four of them through `stratum-workspace` (W26); **CI lints
 * > for any other write path (A15)**.
 *
 * This file is that lint. It runs in CI's `frontend` job (`pnpm vitest run
 * --coverage`, vite.config.ts `test.include = src/**\/*.test.ts`), it scans the
 * entire `apps/desktop/src` tree rather than only the tree W13 owns, and it is
 * a REGISTRY, not a heuristic.
 *
 * # Why a registry and not "no write anywhere"
 *
 * The obvious lint — "no `dispatch` carries `changes`" — is wrong, and running
 * it over this tree is what proves it wrong. Five of the thirteen hits are the
 * Command Bar editing **its own single-line input**, which is not a document at
 * all, and two more are §10's `↑ Add to do-file`, which is the user's own typing
 * arriving through a button instead of a keyboard. A fence that reddens for
 * those gets suppressed within a week, which is the failure mode
 * `scripts/check-topology.sh` already documents at length for `NUMBER_FORMAT`.
 *
 * So the assertion is: **every dispatch that carries `changes` in the frontend
 * is one of the sites enumerated below, with the count that is enumerated
 * below.** Adding one, moving one, or deleting one turns this test red, and the
 * only way to green is to write down which editor it targets and why A15
 * permits it. That is a review gate on a list of thirteen lines, which is
 * exactly the mechanical prevention the bullet asks for and exactly the shape
 * `check-topology.sh`'s `tauri-bridge` check uses for the Tauri seam.
 *
 * # What A15 actually fences
 *
 * ADR-010 / A15 govern **who may serialise a document**: `doc_save`,
 * `section_rename`, `section_move` and an accepted AI diff, all four inside
 * `stratum-workspace`, gated by `assert_comment_only` and
 * `assert_statement_partition_preserved`. They do not, and cannot, forbid the
 * user editing an open buffer — the user typing is the product. The three
 * `target` values below are that distinction made explicit:
 *
 * | target | meaning |
 * |---|---|
 * | `document`  | mutates the open `.do` buffer — needs an A15 justification |
 * | `scratch`   | mutates some other CodeMirror instance; not a document |
 * | `driver`    | a test/e2e driver standing in for the user's keyboard |
 *
 * # What this lint cannot reach, and who owns that
 *
 * The other half of the fence is Rust: W26's acceptance says `write.rs` is the
 * only module in the workspace that opens a `.do` for writing. A vitest scan
 * over `apps/desktop/src` cannot see `crates/**`, and the `frontend` CI job is
 * conditional on `apps/desktop/package.json` existing. The durable home for
 * both halves is a `check_a15_write_fence` scan in `scripts/check-topology.sh`,
 * which is W00's file. W13 owns no file under `.github/`, `scripts/` or
 * `xtask/src/`, so this is as far as R0 lets this unit take it; the gap is
 * reported rather than reached across.
 */

import { readFileSync, readdirSync, statSync } from "node:fs";
import { join, relative, resolve, sep } from "node:path";
import { describe, expect, it } from "vitest";

/** Which editor a registered write site mutates. See the table in the header. */
type WriteTarget = "document" | "scratch" | "driver";

interface WriteSite {
  /** Path relative to `apps/desktop`, POSIX separators. */
  readonly file: string;
  /** Exact number of `dispatch(… changes: …)` calls in the file. */
  readonly sites: number;
  readonly owner: string;
  readonly target: WriteTarget;
  /** Why A15 permits it. A `document` entry without one is a bug, not a note. */
  readonly why: string;
}

/**
 * THE REGISTRY. Fourteen sites, enumerated.
 *
 * Sorted by path so the diff a violation produces reads as an insertion rather
 * than a reshuffle.
 */
const WRITE_SITES: readonly WriteSite[] = [
  {
    file: "src/boot/wire.tsx",
    sites: 1,
    owner: "W17",
    target: "document",
    why:
      "the OS file-open route (`routeOpenedFile`): a double-clicked `.do` " +
      "loads `doc_open`'s reply — the file's own bytes off disk — into the " +
      "editor. A15 fences who may SERIALISE a document (buffer → disk); this " +
      "is the opposite direction, the open itself, and it reaches disk only " +
      "when doc_save later runs.",
  },
  {
    file: "src/commandbar/promote.ts",
    sites: 2,
    owner: "W16",
    target: "document",
    why:
      "spec §10 `↑ Add to do-file` / `⌥↑ Add as new block`. Inserts the command " +
      "THE USER JUST TYPED at the caret of the open buffer — no output text, no " +
      "file. Undoable with one Mod+Z and it reaches disk only when doc_save later " +
      "runs. A15 fences who may serialise a document; this is what the user typed.",
  },
  {
    file: "src/commandbar/view.tsx",
    sites: 5,
    owner: "W16",
    target: "scratch",
    why:
      "the Command Bar's own single-line EditorView (a local `view`, never " +
      "`activeEditor()`). Not a document: it has no path, no DocBytes and is " +
      "never saved.",
  },
  {
    file: "src/e2e/bridge.ts",
    sites: 3,
    owner: "W24",
    target: "driver",
    why:
      "the e2e driver typing on the user's behalf — `openText`, Scenario B's " +
      "`replaceRange`, and the between-scenario reset. Standing in for a " +
      "keyboard is the one thing a driver is for.",
  },
  {
    file: "src/editor/harness.ts",
    sites: 1,
    owner: "W13",
    target: "driver",
    why: "the unit-test harness's `typeChar`, i.e. one simulated keystroke.",
  },
];

/**
 * The four writers of A15, exactly as `editor/commands.ts` names them.
 *
 * W26 names the fourth `ai_apply_patch` on the wire and the frontend names it
 * `ai_diff_accepted`; both denote "an accepted AI diff". What matters here is
 * that the set has FOUR members and that widening it is a diff someone reads.
 */
const SANCTIONED_REASONS = [
  "doc_save",
  "section_rename",
  "section_move",
  "ai_diff_accepted",
] as const;

const PACKAGE_ROOT = resolve(process.cwd());
const SRC = join(PACKAGE_ROOT, "src");

function* walk(dir: string): Generator<string> {
  for (const entry of readdirSync(dir).sort()) {
    const path = join(dir, entry);
    if (statSync(path).isDirectory()) yield* walk(path);
    else if (path.endsWith(".ts") || path.endsWith(".tsx")) yield path;
  }
}

/** Package-relative, POSIX separators, so the registry reads the same on Windows. */
function rel(path: string): string {
  return relative(PACKAGE_ROOT, path).split(sep).join("/");
}

/**
 * Test files are out of scope, deliberately and narrowly.
 *
 * A `*.test.ts` does not ship, and half of this suite's job is to simulate a
 * user typing — asserting a document is unchanged after 200 operations requires
 * making some. Non-test drivers (`harness.ts`, `e2e/bridge.ts`) are NOT exempt;
 * they are registered above, by name and by count.
 */
function isTestFile(path: string): boolean {
  return path.endsWith(".test.ts") || path.endsWith(".test.tsx");
}

/**
 * Every `dispatch(...)` in `source` whose call carries a `changes:` key.
 *
 * The window is 600 characters and stops at the first `});`, which is generous
 * on purpose: a multi-line transaction spec is exactly the shape a sneaked-in
 * write has, and a scanner that has to guess here guesses towards reporting.
 * `applyChanges(changes: …)` in `src/wasm/**` and `changes: number` in
 * `panes/variables/selection.ts` are not matched, because neither is inside a
 * `dispatch(` call — which is why the anchor is `dispatch(` and not `changes:`.
 */
function writeSitesIn(source: string): number[] {
  const lines: number[] = [];
  for (const match of source.matchAll(/dispatch\(/g)) {
    const window = source.slice(match.index, match.index + 600);
    const close = window.indexOf("});");
    const call = close === -1 ? window : window.slice(0, close + 1);
    if (/\bchanges\s*:/.test(call)) lines.push(source.slice(0, match.index).split("\n").length);
  }
  return lines;
}

/** `path (n sites)` — the comparison shape, so a failure diffs as one line. */
function tally(file: string, sites: number): string {
  return `${file} (${sites} site${sites === 1 ? "" : "s"})`;
}

describe("A15 — the document write fence (the standing CI lint)", () => {
  it("every dispatch carrying `changes` in the frontend is a registered site", () => {
    const found: string[] = [];
    for (const path of walk(SRC)) {
      if (isTestFile(path)) continue;
      const sites = writeSitesIn(readFileSync(path, "utf8"));
      if (sites.length > 0) found.push(tally(rel(path), sites.length));
    }

    const expected = WRITE_SITES.map((s) => tally(s.file, s.sites)).sort();

    // If this failed, read the header of this file before touching the registry.
    // A new `scratch` or `driver` entry is a one-line change with a reason. A new
    // `document` entry is a change to the A15 fence itself and needs a ruling —
    // the four sanctioned writers live in stratum-workspace, not in the frontend.
    expect(found.sort()).toEqual(expected);
  });

  it("W13's own production source writes the document nowhere at all", () => {
    // Zero, not one: even `commands.ts`'s sanctioned path delegates the actual
    // edit to W26's gated writers instead of performing it here, so the strongest
    // true statement about this unit is that it contains no write at all.
    const offenders: string[] = [];
    for (const path of walk(join(SRC, "editor"))) {
      if (isTestFile(path) || rel(path) === "src/editor/harness.ts") continue;
      const source = readFileSync(path, "utf8");
      for (const line of writeSitesIn(source)) offenders.push(`${rel(path)}:${line}`);
    }
    expect(offenders).toEqual([]);

    for (const path of walk(join(SRC, "panes", "sections"))) {
      if (isTestFile(path)) continue;
      expect(writeSitesIn(readFileSync(path, "utf8"))).toEqual([]);
    }
  });

  it("the choke point is one function, and it counts every pass", () => {
    const commands = readFileSync(join(SRC, "editor", "commands.ts"), "utf8");

    // The counter is incremented in exactly one place in the entire frontend. A
    // second `documentWrites += 1` would let a bypass keep the behavioural test
    // in `editor.doc.test.ts` green while writing.
    const increments: string[] = [];
    for (const path of walk(SRC)) {
      if (isTestFile(path)) continue;
      const source = readFileSync(path, "utf8");
      for (const match of source.matchAll(/counters\.documentWrites\s*(\+\+|\+=|=)/g)) {
        increments.push(`${rel(path)}:${source.slice(0, match.index).split("\n").length}`);
      }
    }
    expect(increments).toEqual(["src/editor/commands.ts:74"]);

    // …and the increment is the FIRST statement of `writeDocument`, so no early
    // return can perform a write without being counted.
    const body = commands.slice(commands.indexOf("export function writeDocument("));
    const firstStatement = body
      .slice(body.indexOf("{") + 1)
      .trim()
      .split("\n")[0];
    expect(firstStatement).toBe("counters.documentWrites += 1;");
  });

  it("the WriteReason union is exactly A15's four writers", () => {
    const commands = readFileSync(join(SRC, "editor", "commands.ts"), "utf8");
    const declaration = /export type WriteReason =([^;]+);/.exec(commands);
    expect(declaration).not.toBeNull();

    const reasons = [...(declaration?.[1] ?? "").matchAll(/"([^"]+)"/g)].map((m) => m[1]);
    expect(reasons).toEqual([...SANCTIONED_REASONS]);

    // Every call site passes a STRING LITERAL from that set. A
    // `writeDocument(reason, …)` forwarding a variable would make the union
    // decorative — TypeScript checks the type at the forwarding site, not at the
    // place the string was built, and a `WriteReason` cast is one line away.
    const calls: string[] = [];
    for (const match of commands.matchAll(/(\bfunction\s+)?writeDocument\(\s*([^\n]*)/g)) {
      if (match[1] !== undefined) continue; // the declaration itself
      calls.push((match[2] ?? "").trim());
    }
    expect(calls.length).toBeGreaterThan(0);
    for (const call of calls) {
      const literal = /^"([^"]+)",/.exec(call);
      expect(literal, `writeDocument called with a non-literal reason: ${call}`).not.toBeNull();
      expect(SANCTIONED_REASONS).toContain(literal?.[1]);
    }
  });

  it("every `document` entry in the registry carries a justification", () => {
    for (const site of WRITE_SITES) {
      expect(site.why.length).toBeGreaterThan(40);
      if (site.target === "document") {
        // The two words that make it an A15 argument rather than an assertion.
        expect(site.why.toLowerCase()).toMatch(/a15|adr-010/);
      }
    }
  });
});
