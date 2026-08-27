/**
 * The one entry point the editor uses to obtain a segmenter.
 *
 * `loadSegmenter()` returns a {@link StratumSegmenter} — layer 2 of `types.ts` —
 * and nothing about the returned object tells you which backend produced it.
 * That is the contract W13 is written against, and it is the reason the editor's
 * test suite can be run twice, once per backend, and expect the same behaviour
 * of everything except segmentation *content*.
 *
 * # Build-time configuration (W12 owns `vite.config.ts`)
 *
 * ```ts
 * // vite.config.ts — the function form, because `mode` is needed
 * export default defineConfig(({ mode }) => ({
 *   define: {
 *     __STRATUM_ALLOW_WASM_STUB__: JSON.stringify(mode !== "production"),
 *   },
 *   // …
 * }));
 * ```
 *
 * With the define set to the literal `false`, `STUB_ALLOWED` folds to `false`,
 * the guarded `await import("./stub/index.ts")` becomes unreachable, and Rollup
 * drops the entire `stub/` subtree from the bundle. `cargo xtask wasm
 * --check-bundle dist` then greps the emitted assets for the stub's sentinel and
 * fails the build if any of it survived — because a tree-shaking regression that
 * only shows up as "the editor silently used the naive splitter in production"
 * is not a bug anyone finds from a bug report.
 *
 * **It has to be a literal.** Spelling the define as the expression
 * `"import.meta.env.DEV"` looks equivalent and is not: define substitution runs
 * once and does not re-enter its own output, so the text survives into the
 * bundle unreplaced, nothing folds, and the stub ships. Measured, not assumed —
 * `conformance.test.ts` bundles this file both ways and asserts the outcome.
 *
 * If the define is missing entirely, `STUB_ALLOWED` is `false`. Failing closed
 * is the only safe default for a fence, and `conformance.ts` covers both hosts:
 * a bare `node` run has no define and must refuse to fall back.
 */

import { createSegmenter } from "./segmenter.ts";
import { WASM_ABI } from "./types.ts";
import type { RawModule, SegmenterBackend, StratumSegmenter } from "./types.ts";

declare const __STRATUM_ALLOW_WASM_STUB__: boolean | undefined;

/**
 * Whether this build may fall back to the development stub.
 *
 * Read through `typeof` so an unconfigured build — a unit test, a Node script,
 * a bundler that dropped the define — is treated as production rather than
 * crashing on an undefined global.
 */
export const STUB_ALLOWED: boolean =
  typeof __STRATUM_ALLOW_WASM_STUB__ !== "undefined" && __STRATUM_ALLOW_WASM_STUB__ === true;

/** Where the real module's wasm-bindgen glue is emitted by `xtask wasm`. */
const GENERATED_GLUE = "./generated/stratum_wasm.js";

/** Options for {@link loadSegmenter}. */
export interface LoadOptions {
  /**
   * Override the module source. Used by tests and by `conformance.ts` to drive
   * a specific backend; production passes nothing.
   */
  module?: RawModule | (() => Promise<RawModule>);
  /** Which backend `module` is, when one is supplied. Defaults to `"wasm"`. */
  backend?: SegmenterBackend;
  /**
   * Refuse the stub even where it would be allowed. The editor's parity run sets
   * this for the "must behave like production" half of the suite.
   */
  requireReal?: boolean;
  /**
   * What to hand `__wbg_init`. A browser passes nothing and the glue fetches the
   * `.wasm` beside itself; Node has no `fetch` for `file:` URLs, so the
   * conformance harness reads the bytes and passes them here.
   *
   * **It cannot be used to simulate a broken module.** `__wbg_init` opens with
   * `if (wasm !== undefined) return wasm;` and the glue is an ES module, so the
   * first successful init in a process is the last call that reads this at all:
   * every later one resolves with the cached instance whatever bytes it is
   * given. Use {@link LoadOptions.realModule} for that.
   */
  wasmSource?: unknown;
  /**
   * Replace how the real module is obtained.
   *
   * **Test-only**, and it exists because the three properties below cannot
   * otherwise be asserted in a process that has already loaded the module —
   * which is every process that discovered the wasm backend first:
   *
   * * `requireReal` refuses the stub even where the fence is open,
   * * an open fence degrades a failed real load to the stub,
   * * a closed fence throws instead.
   *
   * Each is a statement about what happens when the real module is
   * *unavailable*, and until this existed the harness manufactured that by
   * handing `wasmSource` empty bytes. That never actually forced anything: the
   * load failed because the checked-in `.wasm` reported `engine_linked()` false
   * and step 3 below rejected it, not because the bytes were bad. So the three
   * checks were green on a fact about one stale artifact, and rebuilding that
   * artifact from source — the correct thing to do — turned all three red
   * against a loader that had not changed. What is under test is the loader, so
   * the way to withhold the module has to be the loader's, not the artifact's.
   *
   * Production passes nothing and gets {@link loadReal}. The fence is
   * untouched: this decides what counts as the real module, never whether the
   * stub may stand in for it — `STUB_ALLOWED` and `requireReal` still do that,
   * and both are applied to whatever this returns.
   */
  realModule?: () => Promise<RawModule>;
  /**
   * Accept a module whose `engine_linked()` is false.
   *
   * **Test-only.** It exists because the harness-only build — everything in
   * CONTRACTS §14 with no segmenter behind it — is exactly what W11a ships and
   * what `conformance.ts` must be able to drive. Production never sets it, and
   * the check it disables is the one thing standing between an unlinked build
   * and an editor that silently shows a document with no blocks.
   */
  allowUnlinked?: boolean;
}

/** Raised when no usable backend could be produced. */
export class SegmenterLoadError extends Error {
  constructor(message: string, options?: { cause?: unknown }) {
    super(message, options);
    this.name = "SegmenterLoadError";
  }
}

/**
 * Load the real wasm module, falling back to the fenced stub only where the
 * build allows it.
 *
 * Order of events, and each one is a check that has caught something in a
 * comparable codebase:
 *
 * 1. Instantiate the module.
 * 2. `abi_version()` must equal {@link WASM_ABI}. A stale `.wasm` next to fresh
 *    TypeScript reads the flat rows at the wrong stride and produces regions
 *    that are subtly, silently wrong — never an exception.
 * 3. `engine_linked()` must be true. A harness-only build (no segmenter linked
 *    yet) produces zero regions, which is indistinguishable from an empty
 *    document unless someone asks.
 */
export async function loadSegmenter(options: LoadOptions = {}): Promise<StratumSegmenter> {
  const explicit = options.module;
  if (explicit) {
    const module = typeof explicit === "function" ? await explicit() : explicit;
    const backend = options.backend ?? "wasm";
    checkAbi(module, backend);
    if (backend === "wasm" && options.allowUnlinked !== true) checkLinked(module);
    return createSegmenter(module, backend);
  }

  const real = options.realModule ?? (() => loadReal(options.wasmSource));
  let realError: unknown;
  try {
    const module = await real();
    checkAbi(module, "wasm");
    if (options.allowUnlinked !== true) checkLinked(module);
    return createSegmenter(module, "wasm");
  } catch (e) {
    realError = e;
  }

  if (!STUB_ALLOWED || options.requireReal === true) {
    throw new SegmenterLoadError(
      "the Stratum wasm segmenter could not be loaded and this build has no " +
        "fallback (run `cargo xtask wasm` to build it)",
      { cause: realError },
    );
  }

  const stub = await import("./stub/index.ts");
  const module = stub.createStubModule();
  checkAbi(module, "stub");
  return createSegmenter(module, "stub");
}

/**
 * Instantiate the generated glue and assemble a {@link RawModule} from it.
 *
 * The specifier goes through a variable so neither Vite nor `tsc` tries to
 * resolve it at build time: `generated/` is produced by `cargo xtask wasm` and
 * is absent from a fresh checkout, and a hard import would turn "you have not
 * built the wasm yet" into a build error in every unrelated file.
 *
 * `memory` is deliberately taken from the init result, not from the module
 * namespace: `wasm-bindgen --target web` puts the `WebAssembly.Memory` on
 * `InitOutput` and does not re-export it. Reading it off the namespace yields
 * `undefined`, and the first `reserve` then writes into nothing — which is a
 * silently empty document rather than an error.
 */
async function loadReal(wasmSource?: unknown): Promise<RawModule> {
  const specifier = GENERATED_GLUE;
  const glue = (await import(/* @vite-ignore */ specifier)) as {
    default?: (source?: unknown) => Promise<{ memory: WebAssembly.Memory }>;
  } & Partial<RawModule>;
  if (typeof glue.default !== "function") {
    throw new SegmenterLoadError("the generated wasm glue has no init export");
  }
  const init = await glue.default(wasmSource);
  if (typeof glue.abi_version !== "function" || typeof glue.Engine !== "function") {
    throw new SegmenterLoadError(
      "the generated wasm glue does not expose the CONTRACTS §14 surface",
    );
  }
  return {
    Engine: glue.Engine,
    abi_version: glue.abi_version,
    engine_linked: glue.engine_linked as () => boolean,
    memory: init.memory,
  };
}

function checkAbi(module: RawModule, backend: SegmenterBackend): void {
  const abi = module.abi_version();
  if (abi !== WASM_ABI) {
    throw new SegmenterLoadError(
      `wasm ABI mismatch: the ${backend} module reports layout ${abi}, this ` +
        `build reads layout ${WASM_ABI}. Rebuild with \`cargo xtask wasm\`.`,
    );
  }
}

function checkLinked(module: RawModule): void {
  if (!module.engine_linked()) {
    throw new SegmenterLoadError(
      "this stratum-wasm build has no segmenter linked, so it would report a " +
        "document with no blocks at all",
    );
  }
}
