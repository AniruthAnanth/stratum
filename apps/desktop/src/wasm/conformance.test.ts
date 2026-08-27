// @vitest-environment node
//
// Not jsdom, which is the suite default. Two reasons, and the first is not a
// preference: jsdom installs its own `TextEncoder`, whose `Uint8Array` comes
// from a different realm, and esbuild refuses to start when it sees that — so
// the production-build half below cannot run under jsdom at all. The second is
// that nothing in CONTRACTS §14 touches the DOM; the segmenter is bytes, typed
// arrays and linear memory, and testing it against a fake document would be
// testing the fake.

/**
 * W11a's two acceptance bullets, as tests the runner actually collects.
 *
 * 1. **Interchangeability.** The bullet says W13's editor suite is run against
 *    each backend. W13 does not exist, so that suite cannot be run; what stands
 *    in for it is two things, and the difference between them matters.
 *
 *    `conformance.ts` drives every backend in this checkout — the stub, and the
 *    real module once `cargo xtask wasm` has produced `generated/` — through a
 *    list of checks. A list is evidence about the things on it, which is not the
 *    claim: the claim is that *no* consumer can tell the backends apart, and the
 *    consumer that finds the difference is the one nobody thought to write a
 *    check for.
 *
 *    So `differential.ts` writes the consumers instead. Each seed generates one
 *    editor session — the calls a CodeMirror-backed editor makes, in an order
 *    and with arguments nobody chose — and runs it against every backend in
 *    lockstep, comparing after each call. It found three ways the backends were
 *    distinguishable that the list had missed: `regionAt(-1)` answered
 *    differently depending on whether the document contained a non-ASCII
 *    character, the real module put `undefined` where the stub put `null` in
 *    every nullable diagnostic field, and the two disagreed about which bytes a
 *    completion replaces. All three are fixed; the sessions are what keeps them
 *    fixed.
 *
 *    Three differences are sanctioned and asserted rather than removed, because
 *    W11a's other bullet requires them: `seg.backend` names the backend, the
 *    stub announces itself in the problems pane, and the candidate list is the
 *    completion source's product, which the two wave-1 backends implement
 *    differently on purpose.
 *
 *    **What is still owed.** None of this is W13's suite. When W13 lands,
 *    someone must point it at both backends — `discoverBackends()` is exported
 *    for exactly that, and a `describe.each` over what it returns is the whole
 *    change — and this comment should stop claiming a substitute.
 *
 * 2. **The fence.** The stub must be unreachable in a release build and loud in
 *    dev. Both halves are asserted here, and the release half is asserted by
 *    running a real production `vite build` in-process and looking for the stub
 *    in the emitted chunks — the same thing `cargo xtask wasm --check-bundle`
 *    does to `dist/`, moved close enough to the code to fail in seconds. The
 *    development build is checked too: a fence that passes because the stub was
 *    never in the graph proves nothing, so the test requires the stub to be
 *    present in the dev bundle and absent from the production one.
 */

import { fileURLToPath } from "node:url";
import { build } from "vite";
import type { Rollup } from "vite";
import { afterEach, describe, expect, it, vi } from "vitest";
import { discoverBackends, noRealModule, runConformance } from "./conformance.ts";
import { STUB_ALLOWED, SegmenterLoadError, loadSegmenter } from "./loader.ts";
import { STUB_SENTINEL, createStubModule } from "./stub/index.ts";

// The stub announces itself on every construction, which is the point of it —
// but 25 checks' worth of banner drowns the runner's own output.
vi.spyOn(console, "warn").mockImplementation(() => {});

const backends = await discoverBackends();
const report = await runConformance(backends);

describe("backend discovery", () => {
  it("always finds the stub", () => {
    expect(backends.map((b) => b.name)).toContain("stub");
  });

  it("finds the real module whenever `cargo xtask wasm` has been run", async () => {
    const { access } = await import("node:fs/promises");
    const built = await access(new URL("./generated/stratum_wasm_bg.wasm", import.meta.url)).then(
      () => true,
      () => false,
    );
    // Not an unconditional requirement: a fresh checkout has no `generated/`,
    // and failing there would mean "you have not built the wasm yet" surfaced as
    // a broken test suite. But when the artifact IS present the harness must
    // pick it up, or the whole cross-backend half runs on one backend and says
    // nothing while looking green.
    expect(backends.some((b) => b.name === "wasm")).toBe(built);
  });

  it("produced checks for every backend it found", () => {
    for (const backend of backends) {
      expect(report.checks.some((c) => c.subject === backend.name)).toBe(true);
    }
  });
});

describe("cross-backend conformance", () => {
  for (const check of report.checks) {
    it(`${check.subject}: ${check.name}`, (ctx) => {
      if (check.status === "skip") {
        ctx.skip();
        return;
      }
      if (check.status === "fail") {
        throw new Error(check.detail ?? "check failed without a reason");
      }
    });
  }

  it("ran the cross-backend comparison whenever there were two backends", () => {
    if (backends.length < 2) return;
    expect(report.checks.some((c) => c.subject.includes(" vs "))).toBe(true);
  });
});

// ---------------------------------------------------------------------------
// The fence.
// ---------------------------------------------------------------------------

afterEach(() => {
  vi.mocked(console.warn).mockClear();
});

describe("the stub fence, dev half", () => {
  it("is unlocked under a non-production build", () => {
    // Pins `vite.config.ts`'s `define: { __STRATUM_ALLOW_WASM_STUB__: ... }`.
    // Without it `STUB_ALLOWED` fails closed, the stub is unreachable in dev as
    // well as in release, and W13 has no reference backend to develop against.
    expect(STUB_ALLOWED).toBe(true);
  });

  it("logs loudly, once per module, naming the sentinel", () => {
    createStubModule();
    expect(console.warn).toHaveBeenCalledTimes(1);
    const [format] = vi.mocked(console.warn).mock.calls[0] ?? [];
    expect(String(format)).toContain(STUB_SENTINEL);
    expect(String(format)).toContain("development stub");
  });

  it("still refuses the stub when the caller demands the real module", async () => {
    // `noRealModule`, not a deliberately-broken `wasmSource`: by the time this
    // runs `discoverBackends()` has initialised the glue, and `__wbg_init`
    // hands back its cached instance whatever it is asked for. See
    // `LoadOptions.realModule`.
    await expect(
      loadSegmenter({ requireReal: true, realModule: noRealModule }),
    ).rejects.toBeInstanceOf(SegmenterLoadError);
  });
});

/**
 * Mirrors `contains_stub` in `xtask/src/wasm.rs`, which is the gate of record.
 *
 * Both spellings, for the same reason the Rust side checks both: a minifier that
 * folds `[...].join("_")` leaves the whole sentinel, and one that does not leaves
 * the six fragments in order.
 */
const SENTINEL_FRAGMENTS = ["STRATUM", "WASM", "STUB", "DO", "NOT", "SHIP"];

function carriesStub(code: string): boolean {
  if (code.includes(SENTINEL_FRAGMENTS.join("_"))) return true;
  let cursor = 0;
  for (const fragment of SENTINEL_FRAGMENTS) {
    const at = [code.indexOf(`"${fragment}"`, cursor), code.indexOf(`'${fragment}'`, cursor)]
      .filter((i) => i >= 0)
      .sort((a, b) => a - b)[0];
    if (at === undefined) return false;
    cursor = at + fragment.length + 2;
  }
  return true;
}

/** Bundle `loader.ts` on its own, through the app's real `vite.config.ts`. */
async function bundleLoader(mode: string): Promise<{ code: string; chunks: number }> {
  const root = fileURLToPath(new URL("../..", import.meta.url));
  const entry = "\0stratum-fence-entry";
  // Reaching `loadSegmenter` is what pulls `loader.ts` — and, if the fence is
  // open, the whole `stub/` subtree — into the graph. `STUB_ALLOWED` comes along
  // so the folded constant survives into the output and the define can be read
  // back off the bundle rather than inferred from what tree-shaking did.
  const source = [
    `import { loadSegmenter, STUB_ALLOWED } from "${root}src/wasm/loader.ts";`,
    "globalThis.__stratumFence = [loadSegmenter, STUB_ALLOWED];",
  ].join("\n");
  const result = await build({
    root,
    mode,
    logLevel: "silent",
    plugins: [
      {
        name: "stratum-fence-entry",
        resolveId: (id) => (id === "stratum-fence-entry" ? entry : null),
        load: (id) => (id === entry ? source : null),
      },
    ],
    build: {
      write: false,
      sourcemap: false,
      // Unminified so the six fragments survive as readable literals; the scan
      // above handles either form, and a failure is legible in the output.
      minify: false,
      rollupOptions: { input: "stratum-fence-entry", output: { entryFileNames: "entry.js" } },
    },
  });
  // `build()` is typed for all three of its modes; only the two bundle-returning
  // ones are reachable here, and `watch` is not configured.
  const bundles: Rollup.RollupOutput[] = Array.isArray(result)
    ? result
    : "output" in result
      ? [result]
      : [];
  const output = bundles
    .flatMap((bundle) => bundle.output)
    .filter((chunk): chunk is Rollup.OutputChunk => chunk.type === "chunk")
    .map((chunk) => chunk.code);
  return { code: output.join("\n"), chunks: output.length };
}

describe("the stub fence, release half", () => {
  it("drops the stub from a production bundle", async () => {
    const bundle = await bundleLoader("production");
    // Not vacuous: the loader itself must be in there, or the absence of the
    // stub says nothing about the fence.
    expect(bundle.code).toContain("SegmenterLoadError");
    expect(bundle.code).toContain("STUB_ALLOWED = false");
    expect(carriesStub(bundle.code)).toBe(false);
    // The stub is a lazy chunk when it survives; production should emit only
    // the entry.
    expect(bundle.chunks).toBe(1);
  }, 120_000);

  it("keeps the stub in a development bundle", async () => {
    const bundle = await bundleLoader("development");
    expect(bundle.code).toContain("STUB_ALLOWED = true");
    // The control for the test above. If this ever goes false, the production
    // result stops being evidence of anything.
    expect(carriesStub(bundle.code)).toBe(true);
  }, 120_000);
});
