/**
 * The backend-interchangeability harness.
 *
 * W11a's first acceptance bullet is that `loader.ts` is byte-compatible with the
 * stub and with the real module, and that the editor cannot tell them apart.
 * "Cannot tell them apart" is not a claim you make in a comment; it is a script
 * you run against each backend. This is that script.
 *
 * It deliberately checks the things that are true of *any* backend — the surface,
 * the coordinate system, the document mirror, generation discipline, the tiling
 * invariant, determinism — and not the correctness of the segmentation itself,
 * which is W11b's golden corpus and proptest parity gate.
 *
 * Cross-backend, it compares two things and the distinction matters:
 *
 * * **Structure, on an unambiguous document**, between every pair. Any tiling
 *   segmenter must return the same three regions for three plain commands, so
 *   this holds for the stub against the parser and keeps holding after W11b.
 * * **Content down to the hashes**, between two backends running the *same*
 *   rule. In wave 1 that pair was the TypeScript stub and `ReferenceSegmenter`,
 *   the same naive splitter transliterated into Rust — so every flat view
 *   crossed real wasm linear memory carrying real rows, and a stride,
 *   endianness or slot-order mistake failed here rather than in W13. **W11b has
 *   linked the parser**, so the built module runs the `parser` rule, the pair
 *   stops being same-rule, and these three comparisons now report as skips
 *   naming their successor: `crates/stratum-wasm/tests/parity.rs`, which runs
 *   the one rule natively and in wasm and is the comparison of record.
 *
 * What it cannot do is stand for a consumer nobody has written yet: every check
 * below is one someone thought of. `differential.ts` covers that half by
 * generating editor sessions and running them against every backend in
 * lockstep; {@link runConformance} folds its results in, so both arrive as
 * checks in the one report.
 *
 * `conformance.test.ts` is what runs it in CI — one vitest case per check, so a
 * regression names the check that broke rather than failing one opaque assertion.
 * This file stays a plain module with a CLI tail because the report is also worth
 * reading by hand while working on a backend:
 *
 * ```sh
 * node --experimental-strip-types apps/desktop/src/wasm/conformance.ts
 * ```
 */

import { truncationNotice } from "./complete.ts";
import { runDifferential } from "./differential.ts";
import type { EnvSpec } from "./differential.ts";
import { STUB_ALLOWED, SegmenterLoadError, loadSegmenter } from "./loader.ts";
import { WASM_ABI } from "./types.ts";
import type { DocChange, RawModule, RegionView, StratumSegmenter } from "./types.ts";

/** One backend to put through the harness. */
export interface BackendUnderTest {
  /** Label used in failure messages. */
  name: string;
  /** Produce a fresh segmenter. Called once per check. */
  load: () => Promise<StratumSegmenter>;
  /**
   * Whether the backend produces regions at all. Asked of the backend, not
   * assumed from its name, so the structural checks below refuse to pass
   * vacuously on an empty list.
   */
  segments: boolean;
  /**
   * Which segmentation rule the backend implements.
   *
   * `"naive"` is the deliberate line splitter that both the TypeScript stub and
   * `ReferenceSegmenter` in `crates/stratum-wasm/src/lib.rs` run; `"parser"` is
   * `stratum-parse`, which W11b links. Region content is comparable
   * byte-for-byte only between two backends running the same rule — the naive
   * rule gets `///`, `#delimit ;` and Mata wrong on purpose, so comparing it
   * against the parser would be asserting that the parser is wrong too.
   * Backends running *different* rules are still compared, on a document only
   * one segmentation is possible for; see below.
   */
  rule: "naive" | "parser";
}

/**
 * A real-module loader that always fails, for the checks about what happens
 * when there is no real module.
 *
 * Exported because `conformance.test.ts` asserts the same refusal directly and
 * must withhold the module the same way.
 *
 * The obvious spelling — hand `loadSegmenter` a `wasmSource` of empty bytes —
 * does not work and used to look like it did. `__wbg_init` opens with `if (wasm
 * !== undefined) return wasm;`, {@link discoverBackends} has initialised it by
 * the time these checks run, and a cached instance is returned whatever bytes
 * are asked for. Those checks were therefore green on a property of one stale
 * checked-in `.wasm` — that it reported `engine_linked()` false — and rebuilding
 * it from source turned all three red without a line of `loader.ts` changing.
 */
export const noRealModule = (): Promise<RawModule> =>
  Promise.reject(
    new SegmenterLoadError("the conformance harness withheld the real module on purpose"),
  );

/** Outcome of one check. */
export interface CheckResult {
  /** Backend name, or a pair for cross-backend checks. */
  subject: string;
  /** What was checked. */
  name: string;
  /** `"pass"`, `"fail"` or `"skip"`. */
  status: "pass" | "fail" | "skip";
  /** Failure or skip reason. */
  detail?: string;
}

/** Everything the harness produced. */
export interface ConformanceReport {
  /** Every check, in run order. */
  checks: CheckResult[];
  /** True when no check failed. */
  ok: boolean;
}

/** Every member of the editor-facing interface, with its declared arity. */
const SURFACE: Array<[string, number]> = [
  ["setDoc", 1],
  ["applyChanges", 1],
  ["resegment", 0],
  ["regionCount", 0],
  ["regions", 0],
  ["region", 1],
  ["regionAt", 1],
  ["tokens", 2],
  ["sections", 0],
  ["narrativeRegions", 0],
  ["diagnostics", 0],
  ["setCompletionEnv", 1],
  ["completionEnvGeneration", 0],
  ["complete", 1],
  ["quickFixes", 1],
  ["lints", 0],
  ["docText", 0],
  ["destroy", 0],
];

/** An ordinary do-file. */
const PLAIN_DOC = [
  "// %% Load",
  "sysuse auto, clear",
  "",
  "// %% Model",
  "foreach v of varlist price mpg {",
  "    summarize `v'",
  "}",
  "regress price mpg weight",
  "",
  "program define mysum",
  "    display 1",
  "end",
].join("\n");

/**
 * A do-file with exactly one possible segmentation.
 *
 * Three one-line commands: no comments to attach, no `///` to fold, no braces,
 * no `#delimit`, nothing the naive rule and the real parser can legitimately
 * disagree about. Any tiling segmenter must return three `Simple` regions on
 * these lines, which makes it the one document on which *every* pair of
 * backends is comparable, whatever rule each is running.
 */
const UNAMBIGUOUS_DOC = ["sysuse auto, clear", "summarize price", "list in 1/5"].join("\n");

/**
 * Everything the naive rule handles badly, which is exactly what the same-rule
 * comparison has to cover.
 *
 * A `///` fold, a `#delimit ;` region, a `/*md` narrative block, a `//|`
 * narrative run, an estimation command, a macro in command position, and a brace
 * left open at EOF. Both backends are *wrong* about most of this in the same
 * way; that they are wrong identically is the assertion — a divergence here is a
 * divergence in the flat-view encoding, not in anyone's idea of Stata.
 */
const PATHOLOGICAL_DOC = [
  "//| A narrative line",
  "//| and its continuation",
  "/*md a block */",
  "regress price ///",
  "    mpg weight",
  "#delimit ;",
  "summarize",
  "  price ;",
  "#delimit cr",
  "`cmd' using x",
  "foreach v of varlist price {",
  "    display `v'",
].join("\n");

/**
 * The same file with characters that break a naive offset conversion: a 2-byte
 * `é`, a 3-byte `→`, and a 4-byte emoji that occupies two UTF-16 code units.
 * Every one of these has a different byte width and unit width, which is the
 * whole failure mode `segmenter.ts` exists to close.
 */
const UNICODE_DOC = [
  "// %% Café",
  'label var price "prix (€) → net"',
  "summarize price",
  "* 📊 a chart",
  "list in 1/5",
].join("\n");

async function check(
  results: CheckResult[],
  subject: string,
  name: string,
  body: () => Promise<void> | void,
): Promise<void> {
  try {
    await body();
    results.push({ subject, name, status: "pass" });
  } catch (e) {
    results.push({ subject, name, status: "fail", detail: String(e) });
  }
}

/**
 * An assertion function, not a plain check: `asserts cond` lets the checks below
 * narrow — `assert(seg !== null, …)` then makes `seg` non-null for the rest of
 * the body, instead of forcing a non-null assertion that `biome.json` bans.
 */
function assert(cond: boolean, message: string): asserts cond {
  if (!cond) throw new Error(message);
}

/** Apply a whole document as one change, the way CM6 reports a paste. */
function replaceAll(before: string, after: string): DocChange[] {
  return [{ from: 0, to: before.length, insert: after }];
}

/**
 * Region *structure*, with the hashes left out.
 *
 * Two backends running different rules still have to agree about boundaries,
 * kinds, delimiters and line numbers on an unambiguous document — but not about
 * hashes, and never will: the naive rule runs FNV over the region's bytes and
 * `stratum-parse` runs blake3 over its canonical token stream. Comparing those
 * would be asserting that two different hash functions collide.
 */
function shapeOf(regions: RegionView[]): string {
  return JSON.stringify(
    regions.map((r) => [
      r.from,
      r.to,
      r.outerFrom,
      r.outerTo,
      r.kindCode,
      r.entryDelimiter,
      r.exitDelimiter,
      r.headLine,
      r.lastLine,
      r.executable,
      r.sectionHead,
    ]),
  );
}

/** Region content, normalised for comparison across backends. */
function contentOf(regions: RegionView[]): string {
  return JSON.stringify(
    regions.map((r) => [
      r.from,
      r.to,
      r.outerFrom,
      r.outerTo,
      r.kindCode,
      r.entryDelimiter,
      r.exitDelimiter,
      r.headLine,
      r.lastLine,
      r.executable,
      r.hashKey,
      r.hashOrdinal,
    ]),
  );
}

/** Run every check against every backend, plus the cross-backend comparisons. */
export async function runConformance(backends: BackendUnderTest[]): Promise<ConformanceReport> {
  const checks: CheckResult[] = [];

  for (const backend of backends) {
    const name = backend.name;

    await check(checks, name, "exposes the whole editor surface", async () => {
      const seg = await backend.load();
      for (const [member, arity] of SURFACE) {
        const fn = (seg as unknown as Record<string, unknown>)[member];
        assert(typeof fn === "function", `missing ${member}()`);
        assert(
          (fn as (...a: unknown[]) => unknown).length === arity,
          `${member}() takes ${(fn as (...a: unknown[]) => unknown).length} args, expected ${arity}`,
        );
      }
      assert(typeof seg.generation === "number", "generation is not a number");
      assert(seg.abi > 0, "abi is not reported");
      seg.destroy();
    });

    await check(checks, name, "mirrors the document through incremental edits", async () => {
      const seg = await backend.load();
      seg.setDoc(PLAIN_DOC);
      assert(seg.docText() === PLAIN_DOC, "setDoc did not take");

      // A two-change transaction, in pre-transaction coordinates and ascending
      // order — exactly what ChangeSet.iterChanges reports.
      const a = PLAIN_DOC.indexOf("sysuse");
      const b = PLAIN_DOC.indexOf("regress");
      seg.applyChanges([
        { from: a, to: a + 6, insert: "webuse" },
        { from: b, to: b + 7, insert: "logit" },
      ]);
      const expected = `${PLAIN_DOC.slice(0, a)}webuse${PLAIN_DOC.slice(a + 6, b)}logit${PLAIN_DOC.slice(b + 7)}`;
      assert(seg.docText() === expected, "two-change transaction applied wrong");
      seg.destroy();
    });

    await check(checks, name, "keeps UTF-16 and UTF-8 offsets in step", async () => {
      const seg = await backend.load();
      seg.setDoc(UNICODE_DOC);
      assert(seg.docText() === UNICODE_DOC, "unicode document did not round-trip");

      // Insert after the 4-byte emoji: the byte offset and the unit offset
      // differ by 4 here, and a wrapper that confuses them corrupts the doc.
      const at = UNICODE_DOC.indexOf("a chart");
      seg.applyChanges([{ from: at, to: at + 7, insert: "un graphique" }]);
      const expected = `${UNICODE_DOC.slice(0, at)}un graphique${UNICODE_DOC.slice(at + 7)}`;
      assert(seg.docText() === expected, "edit after a surrogate pair applied wrong");

      // And back to pure ASCII, which must not desynchronise the tables.
      seg.applyChanges(replaceAll(expected, "list\n"));
      assert(seg.docText() === "list\n", "returning to ASCII desynchronised the mirror");
      seg.destroy();
    });

    await check(checks, name, "answers every read method without throwing", async () => {
      // Structural presence is not enough: `tokens()` and `diagnostics()` cross
      // the wasm boundary and marshal, and a backend that compiles can still
      // throw the first time the editor asks it something on a real document.
      const seg = await backend.load();
      seg.setDoc(PLAIN_DOC);
      seg.resegment();
      assert(Array.isArray(seg.tokens(0, PLAIN_DOC.length)), "tokens() is not an array");
      assert(Array.isArray(seg.sections()), "sections() is not an array");
      assert(Array.isArray(seg.narrativeRegions()), "narrativeRegions() is not an array");
      assert(Array.isArray(seg.diagnostics()), "diagnostics() is not an array");
      assert(Array.isArray(seg.lints()), "lints() is not an array");
      assert(Array.isArray(seg.quickFixes(0)), "quickFixes() is not an array");
      assert(Array.isArray(seg.regions()), "regions() is not an array");
      assert(seg.region(-1) === null, "region(-1) is not null");
      assert(seg.regionCount() === seg.regions().length, "regionCount disagrees with regions()");
      // A token range past the end must clamp, not throw: the editor asks for
      // the viewport, and the viewport outlives a deletion by one frame.
      assert(
        Array.isArray(seg.tokens(PLAIN_DOC.length + 5000, PLAIN_DOC.length + 9000)),
        "an out-of-range token request threw",
      );
      seg.destroy();
    });

    await check(checks, name, "advances the generation only on a real change", async () => {
      const seg = await backend.load();
      seg.setDoc(PLAIN_DOC);
      const first = seg.resegment();
      assert(seg.resegment() === first, "resegment with no edit burned a generation");
      seg.applyChanges([{ from: 0, to: 0, insert: "// x\n" }]);
      assert(seg.resegment() !== first, "resegment after an edit did not advance");
      seg.destroy();
    });

    await check(checks, name, "is deterministic over the same input", async () => {
      const one = await backend.load();
      one.setDoc(PLAIN_DOC);
      one.resegment();
      const a = contentOf(one.regions());
      one.destroy();

      const two = await backend.load();
      // Same final text, reached by a different edit path.
      two.setDoc("");
      two.applyChanges([{ from: 0, to: 0, insert: PLAIN_DOC }]);
      two.resegment();
      const b = contentOf(two.regions());
      two.destroy();
      assert(a === b, "the same document segmented differently by edit path");
    });

    if (!backend.segments) {
      checks.push({
        subject: name,
        name: "outer spans tile the document",
        status: "skip",
        detail: "no segmenter linked; this backend reports no regions",
      });
    } else {
      await check(checks, name, "outer spans tile the document", async () => {
        const seg = await backend.load();
        seg.setDoc(PLAIN_DOC);
        seg.resegment();
        const regions = seg.regions();
        assert(regions.length > 0, "a non-empty document produced no regions");
        let cursor = 0;
        for (const r of regions) {
          assert(
            r.outerFrom === cursor,
            `outer span gap at region ${r.index}: expected ${cursor}, got ${r.outerFrom}`,
          );
          assert(r.outerTo >= r.outerFrom, `region ${r.index} has an inverted outer span`);
          assert(
            r.from >= r.outerFrom && r.to <= r.outerTo,
            `region ${r.index} span escapes its outer span`,
          );
          cursor = r.outerTo;
        }
        assert(cursor === PLAIN_DOC.length, `tiling stopped at ${cursor} of ${PLAIN_DOC.length}`);
        seg.destroy();
      });

      await check(checks, name, "regionAt agrees with the region list", async () => {
        const seg = await backend.load();
        seg.setDoc(PLAIN_DOC);
        seg.resegment();
        for (const r of seg.regions()) {
          const hit = seg.regionAt(r.outerFrom);
          assert(hit !== null, `no region at ${r.outerFrom}`);
          assert(hit?.index === r.index, `regionAt(${r.outerFrom}) found ${hit?.index}`);
        }
        seg.destroy();
      });
    }

    await check(checks, name, "reports a capped completion environment", async () => {
      const seg = await backend.load();
      seg.setDoc("summarize pri");
      seg.resegment();
      seg.setCompletionEnv(cappedEnv());
      const list = seg.complete(13);
      // The engine, not the popup, decides what was shed (A11).
      assert(list.truncated, "a truncated environment was not reported as truncated");
      const notice = truncationNotice(list, "en-US");
      assert(notice === "2,048 of 32,767", `truncation notice was ${String(notice)}`);
      seg.destroy();
    });

    await check(checks, name, "refuses use after destroy", async () => {
      const seg = await backend.load();
      seg.destroy();
      let threw = false;
      try {
        seg.setDoc("list");
      } catch {
        threw = true;
      }
      assert(threw, "setDoc after destroy did not throw");
    });
  }

  // --- the loader's own refusals -------------------------------------------
  //
  // These are the checks that stop a wrong module from looking like an empty
  // document. They are about `loader.ts`, not about a backend, so they run once.

  await check(checks, "loader", "refuses a module built for another layout", async () => {
    const stub = await import("./stub/index.ts");
    const wrong: RawModule = { ...stub.createStubModule(), abi_version: () => WASM_ABI + 1 };
    let caught: unknown;
    try {
      await loadSegmenter({ module: wrong, backend: "stub" });
    } catch (e) {
      caught = e;
    }
    assert(caught instanceof SegmenterLoadError, "an ABI mismatch was accepted");
    assert(String(caught).includes("ABI mismatch"), `unhelpful message: ${String(caught)}`);
  });

  await check(checks, "loader", "refuses an unlinked module as the real one", async () => {
    const stub = await import("./stub/index.ts");
    // Same module, claimed as the real backend: `engine_linked()` is false, so
    // it would report a document with no blocks at all.
    const unlinked = stub.createStubModule();
    let caught: unknown;
    try {
      await loadSegmenter({ module: unlinked, backend: "wasm" });
    } catch (e) {
      caught = e;
    }
    assert(caught instanceof SegmenterLoadError, "an unlinked module was accepted");
    assert(String(caught).includes("no segmenter linked"), `unhelpful: ${String(caught)}`);
  });

  await check(
    checks,
    "loader",
    "refuses the stub whenever the caller demands the real thing",
    async () => {
      // Independent of how the build was configured: `requireReal` is what the
      // editor's "must behave like production" suite sets, and it has to hold
      // even in a dev build where the fence is open.
      let caught: unknown;
      try {
        const seg = await loadSegmenter({ realModule: noRealModule, requireReal: true });
        seg.destroy();
      } catch (e) {
        caught = e;
      }
      assert(caught instanceof SegmenterLoadError, "requireReal accepted a fallback");
    },
  );

  await check(checks, "loader", "falls back exactly as far as the fence allows", async () => {
    // Two hosts, two correct answers, and the fence is what decides which.
    // Under Vite (`vite`, `vite build --mode development`, vitest) the `define`
    // is `true` and a broken real load degrades to the stub, loudly. Under a
    // bare Node run there is no `define` at all, STUB_ALLOWED is false, and the
    // same broken load must throw rather than silently hand back a naive
    // splitter that segments a do-file by looking for blank lines.
    let seg: StratumSegmenter | null = null;
    let caught: unknown;
    try {
      seg = await loadSegmenter({ realModule: noRealModule });
    } catch (e) {
      caught = e;
    }
    if (STUB_ALLOWED) {
      assert(seg !== null, `a dev build refused to fall back: ${String(caught)}`);
      assert(seg.backend === "stub", `fell back to ${seg.backend}, not the stub`);
      seg.destroy();
    } else {
      assert(seg === null, "a fenced build fell back to the stub anyway");
      assert(caught instanceof SegmenterLoadError, "a broken load did not fail closed");
    }
  });

  // --- cross-backend --------------------------------------------------------

  for (let i = 0; i < backends.length; i++) {
    for (let j = i + 1; j < backends.length; j++) {
      const a = backends[i] as BackendUnderTest;
      const b = backends[j] as BackendUnderTest;
      const pair = `${a.name} vs ${b.name}`;

      await check(checks, pair, "expose an identical surface", async () => {
        const sa = await a.load();
        const sb = await b.load();
        const keys = (s: StratumSegmenter) =>
          [
            ...Object.getOwnPropertyNames(Object.getPrototypeOf(s)),
            ...Object.keys(s as unknown as Record<string, unknown>),
          ]
            .filter((k) => k !== "constructor")
            .sort()
            .join(",");
        assert(keys(sa) === keys(sb), `surfaces differ:\n  ${keys(sa)}\n  ${keys(sb)}`);
        assert(sa.abi === sb.abi, `ABI differs: ${sa.abi} vs ${sb.abi}`);
        sa.destroy();
        sb.destroy();
      });

      await check(checks, pair, "mirror the document identically", async () => {
        const sa = await a.load();
        const sb = await b.load();
        for (const seg of [sa, sb]) {
          seg.setDoc(UNICODE_DOC);
          const at = UNICODE_DOC.indexOf("prix");
          seg.applyChanges([{ from: at, to: at + 4, insert: "précio" }]);
        }
        assert(sa.docText() === sb.docText(), "document mirrors diverged between backends");
        sa.destroy();
        sb.destroy();
      });

      // The check that survives W11b: whatever rule each backend runs, they must
      // agree about a document only one segmentation is possible for. It is what
      // "W13 cannot tell them apart" reduces to once the two backends are no
      // longer running the same algorithm.
      if (a.segments && b.segments) {
        await check(checks, pair, "agree on an unambiguous document", async () => {
          const sa = await a.load();
          const sb = await b.load();
          for (const seg of [sa, sb]) {
            seg.setDoc(UNAMBIGUOUS_DOC);
            seg.resegment();
          }
          const shape = shapeOf(sa.regions());
          assert(
            sa.regions().length === 3,
            `${a.name} found ${sa.regions().length} regions, not 3`,
          );
          assert(
            shape === shapeOf(sb.regions()),
            `backends disagree:\n  ${shape}\n  ${shapeOf(sb.regions())}`,
          );
          sa.destroy();
          sb.destroy();
        });
      } else {
        checks.push({
          subject: pair,
          name: "agree on an unambiguous document",
          status: "skip",
          detail: `${a.segments ? b.name : a.name} reports no regions at all`,
        });
      }

      if (a.rule === b.rule && a.segments && b.segments) {
        await check(checks, pair, "produce identical regions", async () => {
          const sa = await a.load();
          const sb = await b.load();
          for (const doc of [PLAIN_DOC, UNICODE_DOC, PATHOLOGICAL_DOC]) {
            for (const seg of [sa, sb]) {
              seg.setDoc(doc);
              seg.resegment();
            }
            // Hashes included. Two backends running one rule have to agree down
            // to the u64 pair, because this is the check that catches a stride,
            // an endianness or a slot-order mistake in the flat views — the
            // failures that produce plausible-looking regions anchored to the
            // wrong bytes.
            const left = contentOf(sa.regions());
            assert(
              left === contentOf(sb.regions()),
              `two ${a.rule} backends disagree about ${JSON.stringify(doc.slice(0, 24))}…:\n  ${left}\n  ${contentOf(sb.regions())}`,
            );
          }
          sa.destroy();
          sb.destroy();
        });

        await check(checks, pair, "produce identical sections and narrative runs", async () => {
          // The other two flat views. Nothing else in this file compares them,
          // and `SECTION_STRIDE` is the one stride that is neither 3 nor 9 — the
          // most likely place for an off-by-one row read to hide.
          const sa = await a.load();
          const sb = await b.load();
          for (const doc of [PLAIN_DOC, PATHOLOGICAL_DOC]) {
            for (const seg of [sa, sb]) {
              seg.setDoc(doc);
              seg.resegment();
            }
            assert(
              JSON.stringify(sa.sections()) === JSON.stringify(sb.sections()),
              `sections differ:\n  ${JSON.stringify(sa.sections())}\n  ${JSON.stringify(sb.sections())}`,
            );
            assert(
              JSON.stringify(sa.narrativeRegions()) === JSON.stringify(sb.narrativeRegions()),
              `narrative runs differ:\n  ${JSON.stringify(sa.narrativeRegions())}\n  ${JSON.stringify(sb.narrativeRegions())}`,
            );
          }
          assert(sa.sections().length + sa.narrativeRegions().length > 0, "nothing was compared");
          sa.destroy();
          sb.destroy();
        });

        await check(checks, pair, "produce identical tokens", async () => {
          const sa = await a.load();
          const sb = await b.load();
          for (const seg of [sa, sb]) {
            seg.setDoc(PLAIN_DOC);
            seg.resegment();
          }
          // `tokens()` is the only read method that takes arguments, so it is
          // the only one whose offset conversion and clamping can be wrong on
          // one side and right on the other.
          const of = (s: StratumSegmenter, from: number, to: number) =>
            JSON.stringify(s.tokens(from, to).map((t) => [t.from, t.to, t.tagCode]));
          for (const [from, to] of [
            [0, PLAIN_DOC.length],
            [PLAIN_DOC.indexOf("foreach"), PLAIN_DOC.indexOf("regress")],
            [PLAIN_DOC.length - 4, PLAIN_DOC.length + 900],
          ] as Array<[number, number]>) {
            assert(of(sa, from, to) === of(sb, from, to), `tokens differ over ${from}..${to}`);
          }
          sa.destroy();
          sb.destroy();
        });
      } else {
        // One skip per check, not one for the block. A check that disappears
        // from the report when it stops applying is a check nobody can see
        // stopped applying — and this branch became the live one the day W11b
        // linked the parser, so it is the report's only record that three
        // content comparisons moved out of this file.
        const why =
          a.segments && b.segments
            ? `${a.name} runs the ${a.rule} rule and ${b.name} runs the ${b.rule} rule; content parity between two builds of ONE rule is crates/stratum-wasm/tests/parity.rs`
            : `${a.segments ? b.name : a.name} reports no regions at all`;
        for (const name of [
          "produce identical regions",
          "produce identical sections and narrative runs",
          "produce identical tokens",
        ]) {
          checks.push({ subject: pair, name, status: "skip", detail: why });
        }
      }
    }
  }

  // The checks above are the ones W11a thought of. These are the ones nobody
  // thought of: generated editor programs, run against every backend at once.
  checks.push(...(await runDifferential(backends, encodeEnv)));

  return { checks, ok: checks.every((c) => c.status !== "fail") };
}

/**
 * A msgpack `CompletionEnv` with 2 048 of 32 767 variables and `truncated` set —
 * the A11 shape the popup has to render.
 */
function cappedEnv(): Uint8Array {
  return encodeEnv({ generation: 7, varCount: 2048, total: 32767, truncated: true });
}

/**
 * A msgpack `CompletionEnv` to the spec a generated session asked for.
 *
 * Exported because `differential.ts` needs the same bytes and the harness keeps
 * exactly one msgpack writer: two writers would drift, and the point of feeding
 * both backends the real wire format is that it is the real wire format.
 */
export function encodeEnv(spec: EnvSpec): Uint8Array {
  const varnames = Array.from({ length: spec.varCount }, (_, i) => `v${i}`);
  return encodeMap({
    generation: spec.generation,
    frame: "default",
    frames: ["default"],
    varnames,
    var_total: spec.total,
    truncated: spec.truncated,
    locals: ["i"],
    globals: [],
    scalars: [],
    matrices: [],
    programs: [],
    e_names: [],
    r_names: [],
    value_labels: [],
    stored_estimates: [],
    cwd: "/tmp",
  });
}

// A tiny msgpack writer, so the harness feeds both backends the SAME bytes the
// engine would broadcast rather than a JSON approximation of them.
type Encodable = string | number | boolean | Encodable[] | { [k: string]: Encodable };

function encodeMap(value: { [k: string]: Encodable }): Uint8Array {
  const out: number[] = [];
  writeValue(out, value);
  return Uint8Array.from(out);
}

function writeValue(out: number[], v: Encodable): void {
  if (typeof v === "boolean") {
    out.push(v ? 0xc3 : 0xc2);
  } else if (typeof v === "number") {
    writeUint(out, v);
  } else if (typeof v === "string") {
    writeStr(out, v);
  } else if (Array.isArray(v)) {
    writeHeader(out, v.length, 0x90, 0xdc, 0xdd);
    for (const item of v) writeValue(out, item);
  } else {
    const entries = Object.entries(v);
    writeHeader(out, entries.length, 0x80, 0xde, 0xdf);
    for (const [k, item] of entries) {
      writeStr(out, k);
      writeValue(out, item);
    }
  }
}

function writeHeader(out: number[], n: number, fix: number, wide: number, huge: number): void {
  if (n < 16) out.push(fix | n);
  else if (n < 0x10000) out.push(wide, (n >> 8) & 0xff, n & 0xff);
  else out.push(huge, (n >>> 24) & 0xff, (n >> 16) & 0xff, (n >> 8) & 0xff, n & 0xff);
}

function writeUint(out: number[], n: number): void {
  if (n < 0x80) out.push(n);
  else if (n < 0x100) out.push(0xcc, n);
  else if (n < 0x10000) out.push(0xcd, (n >> 8) & 0xff, n & 0xff);
  else out.push(0xce, (n >>> 24) & 0xff, (n >> 16) & 0xff, (n >> 8) & 0xff, n & 0xff);
}

function writeStr(out: number[], s: string): void {
  const bytes = new TextEncoder().encode(s);
  if (bytes.length < 32) out.push(0xa0 | bytes.length);
  else if (bytes.length < 0x100) out.push(0xd9, bytes.length);
  else out.push(0xda, (bytes.length >> 8) & 0xff, bytes.length & 0xff);
  for (const b of bytes) out.push(b);
}

/**
 * The backends available in this checkout: always the stub, plus the wasm module
 * once `cargo xtask wasm` has produced `generated/`.
 *
 * The wasm module is registered with `allowUnlinked`, so a harness-only build —
 * one whose `engine_linked()` is false, which the loader refuses everywhere
 * else — is still put through the harness rather than dropped from it.
 * W11b has since linked `stratum-parse`, so that is no longer the build anyone
 * gets from `cargo xtask wasm`; the flag stays because the reason for it never
 * depended on which segmenter is behind the surface. Registering the module
 * puts `reserve`/`splice` and every flat view against **real wasm linear
 * memory** — the part of `segmenter.ts` a JavaScript stub cannot exercise,
 * since only the real module can relocate the heap under a live view — and
 * dropping the backend on an unlinked build would take that coverage with it
 * while leaving the suite green on one backend.
 *
 * Neither `linked` nor `segments` is assumed from that: both are measured
 * below, so an unlinked build changes which checks apply and says so in the
 * report instead of quietly narrowing it.
 */
export async function discoverBackends(): Promise<BackendUnderTest[]> {
  const backends: BackendUnderTest[] = [
    {
      name: "stub",
      segments: true,
      rule: "naive",
      load: async () => {
        const stub = await import("./stub/index.ts");
        return loadSegmenter({ module: stub.createStubModule(), backend: "stub" });
      },
    },
  ];

  const wasmSource = await readGeneratedWasm();
  if (wasmSource) {
    const load = () => loadSegmenter({ wasmSource, allowUnlinked: true, requireReal: true });
    // Both flags are measured, never assumed. `engine_linked()` says which rule
    // is behind the surface; the probe says whether that rule emits anything, so
    // a build that links a segmenter and returns nothing fails a check instead
    // of quietly downgrading the suite to one backend.
    const linked = await isLinked(wasmSource);
    const probe = await load();
    probe.setDoc(PLAIN_DOC);
    probe.resegment();
    const segments = probe.regionCount() > 0;
    probe.destroy();
    backends.push({ name: "wasm", segments, rule: linked ? "parser" : "naive", load });
  }
  return backends;
}

/**
 * Whether the built module has a segmenter linked, asked through the loader's
 * own refusal rather than by reading `engine_linked()` directly — so this also
 * tests that the refusal fires.
 */
async function isLinked(wasmSource: unknown): Promise<boolean> {
  try {
    const seg = await loadSegmenter({ wasmSource, requireReal: true });
    seg.destroy();
    return true;
  } catch {
    return false;
  }
}

/**
 * Read the built `.wasm` in Node.
 *
 * `wasm-bindgen --target web` glue fetches the module beside itself, and Node
 * has no `fetch` for `file:` URLs, so the bytes are read here and handed to the
 * init function. Guarded on `process` so a browser test runner importing this
 * file does not pull `node:fs` into a bundle.
 */
async function readGeneratedWasm(): Promise<{ module_or_path: Uint8Array } | null> {
  if (typeof process === "undefined" || !process.versions?.node) return null;
  try {
    const { readFile } = await import("node:fs/promises");
    const url = new URL("./generated/stratum_wasm_bg.wasm", import.meta.url);
    // The single-object form; passing bytes positionally is deprecated in
    // wasm-bindgen 0.2.127 and warns on every load.
    return { module_or_path: new Uint8Array(await readFile(url)) };
  } catch {
    return null;
  }
}

/**
 * The CLI reporter's only output channel.
 *
 * `process.stdout` rather than `console.log`: this is a program whose entire
 * product is a report on stdout, and `noConsoleLog` is a rule about debug
 * statements left in application code. Writing to the stream directly says which
 * of the two this is without needing a suppression comment to explain it.
 */
function line(text: string): void {
  process.stdout.write(`${text}\n`);
}

async function main(): Promise<void> {
  const backends = await discoverBackends();
  const report = await runConformance(backends);
  for (const c of report.checks) {
    const mark = c.status === "pass" ? "ok  " : c.status === "skip" ? "skip" : "FAIL";
    line(`${mark} ${c.subject}: ${c.name}${c.detail ? ` — ${c.detail}` : ""}`);
  }
  const failed = report.checks.filter((c) => c.status === "fail").length;
  line(
    `\n${report.checks.length} checks, ${failed} failed, ` +
      `${report.checks.filter((c) => c.status === "skip").length} skipped ` +
      `(backends: ${backends.map((b) => b.name).join(", ")})`,
  );
  if (!report.ok) process.exitCode = 1;
}

// Run when invoked directly, stay importable when a test runner pulls it in.
if (process.argv[1] && import.meta.url.endsWith(process.argv[1].replace(/^.*(?=\/)/, ""))) {
  await main();
}
