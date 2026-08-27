/**
 * The fenced development stub.
 *
 * It implements {@link RawModule} — the CONTRACTS §14 surface, not the editor's
 * — including a fabricated linear memory, so `segmenter.ts` drives it through
 * exactly the same code path it drives the real module through. That is what
 * "byte-compatible with both the stub and the real module" means in practice:
 * not two wrappers that behave alike, one wrapper that cannot tell.
 *
 * # The fence
 *
 * This module must never reach a release bundle, and three independent things
 * stop it:
 *
 * 1. `loader.ts` reaches it only through `await import()` inside
 *    `if (STUB_ALLOWED)`, where `STUB_ALLOWED` is the Vite `define`
 *    `__STRATUM_ALLOW_WASM_STUB__`. Defined to `false` in a production build,
 *    the branch is a constant-false and Rollup drops the import with it.
 * 2. {@link STUB_SENTINEL} is a literal string in this file. `cargo xtask wasm
 *    --check-bundle <dist>` greps the built assets for it and fails the build if
 *    it survives. A tree-shaking regression is therefore a red CI job, not a
 *    silent shipped stub.
 * 3. Creating the module logs a loud warning on every start-up in dev.
 *
 * A stub that can ship is a stub that will ship.
 */

import { WASM_ABI } from "../types.ts";
import type { RawEngine, RawModule } from "../types.ts";
import { completeAt } from "./completion.ts";
import { readEnv } from "./msgpack.ts";
import type { StubEnv } from "./msgpack.ts";
import { segment, tokenize } from "./naive.ts";
import type { NaiveSegmentation } from "./naive.ts";

/**
 * The literal `xtask wasm --check-bundle` greps production assets for.
 *
 * Assembled from fragments at module scope so that this file's own source, the
 * only place the whole string exists at once, is what a bundler would have to
 * carry for the grep to fire — and so that the check cannot be defeated by a
 * minifier renaming the constant.
 */
export const STUB_SENTINEL = ["STRATUM", "WASM", "STUB", "DO", "NOT", "SHIP"].join("_");

/** Fabricated linear memory: a growable `ArrayBuffer` with a bump allocator. */
class StubMemory {
  buffer: ArrayBuffer = new ArrayBuffer(1 << 16);

  /** Grow to at least `bytes` and return the scratch base pointer. */
  reserve(bytes: number): number {
    if (this.buffer.byteLength < bytes) {
      // Doubling, like a real `Vec`: the caller is told the pointer may move.
      let size = this.buffer.byteLength;
      while (size < bytes) size *= 2;
      const grown = new ArrayBuffer(size);
      new Uint8Array(grown).set(new Uint8Array(this.buffer));
      this.buffer = grown;
    }
    return 0;
  }

  /** A view over the whole buffer. Rebuilt per call — `reserve` may have moved it. */
  view(): Uint8Array {
    return new Uint8Array(this.buffer);
  }
}

class StubEngine implements RawEngine {
  private doc = new Uint8Array(0);
  private seg: NaiveSegmentation | null = null;
  private env: StubEnv | null = null;
  private gen = 0;
  private dirty = false;

  private memory: StubMemory;

  constructor(memory: StubMemory) {
    this.memory = memory;
  }

  reserve(bytes: number): number {
    return this.memory.reserve(bytes);
  }

  splice(from: number, to: number, src: number, len: number): void {
    if (from > to || to > this.doc.length) return;
    const insert = this.memory.view().subarray(src, src + len);
    const next = new Uint8Array(this.doc.length - (to - from) + len);
    next.set(this.doc.subarray(0, from), 0);
    next.set(insert, from);
    next.set(this.doc.subarray(to), from + len);
    this.doc = next;
    this.dirty = true;
  }

  resegment(): number {
    if (!this.dirty) return this.gen;
    this.seg = segment(this.doc);
    this.dirty = false;
    this.gen = (this.gen + 1) >>> 0;
    return this.gen;
  }

  generation(): number {
    return this.gen;
  }

  region_count(): number {
    return this.seg ? this.seg.rows.length / 9 : 0;
  }

  regions_view(): Int32Array {
    return Int32Array.from(this.seg?.rows ?? []);
  }

  region_hashes(): BigUint64Array {
    return BigUint64Array.from(this.seg?.hashes ?? []);
  }

  tokens(from: number, to: number): Int32Array {
    return Int32Array.from(tokenize(this.doc, from, to));
  }

  sections(): Int32Array {
    return Int32Array.from(this.seg?.sections ?? []);
  }

  narrative_regions(): Int32Array {
    return Int32Array.from(this.seg?.narrative ?? []);
  }

  diagnostics(): unknown {
    return this.seg?.diagnostics ?? [];
  }

  set_completion_env(msgpack: Uint8Array): void {
    const env = readEnv(msgpack);
    if (env) this.env = env;
  }

  completion_env_generation(): bigint {
    return BigInt(this.env?.generation ?? 0);
  }

  complete(pos: number): unknown {
    return completeAt(this.doc, pos, this.env);
  }

  quick_fixes(): unknown {
    // Quick fixes need the command table (W04b) and the dataflow index (W20).
    // The stub has neither, and inventing plausible-looking fixes would be the
    // one failure mode worse than offering none.
    return [];
  }

  lints(): unknown {
    return [];
  }

  doc_text(): string {
    return new TextDecoder().decode(this.doc);
  }

  doc_len(): number {
    return this.doc.length;
  }

  free(): void {
    this.doc = new Uint8Array(0);
    this.seg = null;
  }
}

/**
 * Build a stub {@link RawModule}. Logs once, loudly: the point is that nobody
 * debugs a segmentation oddity for an hour before noticing which backend they
 * are on.
 */
export function createStubModule(): RawModule {
  const memory = new StubMemory();
  const body =
    "Block segmentation is running on the development stub, not on the Stata " +
    "parser. Regions, tokens and completions are approximate. This module is " +
    "excluded from release builds.";
  console.warn(
    `%c${STUB_SENTINEL}%c\n${body}`,
    "background:#7a1f1f;color:#fff;padding:2px 6px;border-radius:3px;font-weight:700",
    "",
  );
  return {
    Engine: class extends StubEngine {
      constructor() {
        super(memory);
      }
    },
    abi_version: () => WASM_ABI,
    // The stub is not the real segmenter and says so. `loader.ts` uses this to
    // refuse it in production even if the fence were somehow bypassed.
    engine_linked: () => false,
    memory,
  };
}
