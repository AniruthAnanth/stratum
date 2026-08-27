/* tslint:disable */
/* eslint-disable */

/**
 * The per-document segmentation engine, one per open editor.
 *
 * Runs on the **main thread**, synchronously, inside the CodeMirror transaction
 * cycle (06 §3): a worker would reintroduce the frame lag the whole design
 * exists to delete.
 */
export class Engine {
    free(): void;
    [Symbol.dispose](): void;
    /**
     * Deterministic completion. HARD CONTRACT: < 2 ms, criterion-benched in CI.
     *
     * Truncation is stamped here rather than left to the backend: A11 is a
     * property of the ENVIRONMENT the engine shed entries from, not of the
     * candidate list, and a backend that forgot to propagate it would silently
     * tell the user that 2 048 variables are all the variables there are.
     */
    complete(pos: number): any;
    /**
     * The generation of the environment currently loaded, so the webview can
     * tell whether a `StateChanged` it just saw has been applied.
     */
    completion_env_generation(): bigint;
    /**
     * Parse diagnostics plus any splice faults. Rare; JSON is fine (§14).
     *
     * Faults are drained: a splice error is reported once, to the transaction
     * that caused it.
     */
    diagnostics(): any;
    /**
     * Document length in bytes. The webview asserts it against its own encoded
     * length after each transaction; a mismatch means the two buffers have
     * diverged and the wrapper resynchronises with a full replace.
     */
    doc_len(): number;
    /**
     * The document as JS sees it. Test and debug affordance — the editor is
     * authoritative for text, never this buffer (06 §2, rule 2).
     */
    doc_text(): string;
    /**
     * The current generation without re-segmenting.
     */
    generation(): number;
    /**
     * Whole-document lints that need no session state, as frozen
     * `Diagnostic`s. Lints that need live state come from the engine.
     */
    lints(): any;
    /**
     * Flat `i32` view, [`NARRATIVE_STRIDE`] per region — `//|` and `/*md`.
     */
    narrative_regions(): Int32Array;
    /**
     * A fresh engine over an empty document, generation 0.
     */
    constructor();
    /**
     * Deterministic quick fixes at `pos`, as frozen `Suggestion`s.
     */
    quick_fixes(pos: number): any;
    /**
     * Number of regions in the current segmentation.
     */
    region_count(): number;
    /**
     * Flat `u64` view, [`REGION_HASH_STRIDE`] per region.
     *
     * **THESE ARE HASHES, NOT IDENTITIES.** `BlockId` comes from the engine.
     */
    region_hashes(): BigUint64Array;
    /**
     * Flat `i32` view, [`REGION_STRIDE`] per region.
     */
    regions_view(): Int32Array;
    /**
     * Re-segment. Returns the generation, which increments only when the
     * document actually changed — an unchanged document costs one branch.
     *
     * Budget: < 150 µs incremental, 3–8 ms for a cold 10 k-line pass.
     */
    resegment(): number;
    /**
     * Pointer into wasm memory for JS to write UTF-8 into. Grows on demand.
     *
     * See [`Doc::reserve`]: the returned pointer is valid until the next call.
     */
    reserve(bytes: number): number;
    /**
     * Flat `i32` view, [`SECTION_STRIDE`] per section.
     */
    sections(): Int32Array;
    /**
     * Set the live environment pushed by the engine on `StateChanged`.
     *
     * Takes the engine's own msgpack bytes (§9/§10). A malformed payload keeps
     * the previous environment — completing against a stale variable list is a
     * far smaller failure than a popup that stops working.
     */
    set_completion_env(msgpack: Uint8Array): void;
    /**
     * Apply one CM6 change: replace `[from, to)` with `len` bytes already
     * written at `src` in the scratch buffer.
     *
     * Offsets are UTF-8 byte offsets. A rejected splice records a diagnostic and
     * leaves the document unchanged rather than unwinding into the transaction.
     */
    splice(from: number, to: number, src: number, len: number): void;
    /**
     * Flat `i32` triples `[from, to, tag]` for the requested byte range only.
     *
     * Scoped to the visible range because a 10 k-line file has ~8 k tokens per
     * screen and materialising the whole document's stream would cost more than
     * the parse (06 §3.4).
     */
    tokens(from: number, to: number): Int32Array;
}

/**
 * Version of the flat view layout this module was built with. `loader.ts`
 * refuses a module whose value differs from its own.
 */
export function abi_version(): number;

/**
 * Whether a real segmenter is linked.
 *
 * False for a harness-only build, which produces no regions at all. The loader
 * treats false as fatal in production and as "fall back to the fenced stub" in
 * development; without this, an unlinked module would look exactly like an
 * empty document.
 */
export function engine_linked(): boolean;

/**
 * Install the panic hook, when this build has one. Called by wasm-bindgen at
 * module instantiation.
 */
export function start(): void;

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
    readonly memory: WebAssembly.Memory;
    readonly __wbg_engine_free: (a: number, b: number) => void;
    readonly abi_version: () => number;
    readonly engine_complete: (a: number, b: number) => number;
    readonly engine_completion_env_generation: (a: number) => bigint;
    readonly engine_diagnostics: (a: number) => number;
    readonly engine_doc_len: (a: number) => number;
    readonly engine_doc_text: (a: number, b: number) => void;
    readonly engine_generation: (a: number) => number;
    readonly engine_lints: (a: number) => number;
    readonly engine_narrative_regions: (a: number) => number;
    readonly engine_new: () => number;
    readonly engine_quick_fixes: (a: number, b: number) => number;
    readonly engine_region_count: (a: number) => number;
    readonly engine_region_hashes: (a: number) => number;
    readonly engine_regions_view: (a: number) => number;
    readonly engine_resegment: (a: number) => number;
    readonly engine_reserve: (a: number, b: number) => number;
    readonly engine_sections: (a: number) => number;
    readonly engine_set_completion_env: (a: number, b: number, c: number) => void;
    readonly engine_splice: (a: number, b: number, c: number, d: number, e: number) => void;
    readonly engine_tokens: (a: number, b: number, c: number) => number;
    readonly start: () => void;
    readonly engine_linked: () => number;
    readonly __wbindgen_add_to_stack_pointer: (a: number) => number;
    readonly __wbindgen_export: (a: number, b: number, c: number) => void;
    readonly __wbindgen_export2: (a: number, b: number) => number;
    readonly __wbindgen_start: () => void;
}

export type SyncInitInput = BufferSource | WebAssembly.Module;

/**
 * Instantiates the given `module`, which can either be bytes or
 * a precompiled `WebAssembly.Module`.
 *
 * @param {{ module: SyncInitInput }} module - Passing `SyncInitInput` directly is deprecated.
 *
 * @returns {InitOutput}
 */
export function initSync(module: { module: SyncInitInput } | SyncInitInput): InitOutput;

/**
 * If `module_or_path` is {RequestInfo} or {URL}, makes a request and
 * for everything else, calls `WebAssembly.instantiate` directly.
 *
 * @param {{ module_or_path: InitInput | Promise<InitInput> }} module_or_path - Passing `InitInput` directly is deprecated.
 *
 * @returns {Promise<InitOutput>}
 */
export default function __wbg_init (module_or_path?: { module_or_path: InitInput | Promise<InitInput> } | InitInput | Promise<InitInput>): Promise<InitOutput>;
