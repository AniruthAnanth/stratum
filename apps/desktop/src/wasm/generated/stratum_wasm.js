/* @ts-self-types="./stratum_wasm.d.ts" */

/**
 * The per-document segmentation engine, one per open editor.
 *
 * Runs on the **main thread**, synchronously, inside the CodeMirror transaction
 * cycle (06 §3): a worker would reintroduce the frame lag the whole design
 * exists to delete.
 */
export class Engine {
    __destroy_into_raw() {
        const ptr = this.__wbg_ptr;
        this.__wbg_ptr = 0;
        EngineFinalization.unregister(this);
        return ptr;
    }
    free() {
        const ptr = this.__destroy_into_raw();
        wasm.__wbg_engine_free(ptr, 0);
    }
    /**
     * Deterministic completion. HARD CONTRACT: < 2 ms, criterion-benched in CI.
     *
     * Truncation is stamped here rather than left to the backend: A11 is a
     * property of the ENVIRONMENT the engine shed entries from, not of the
     * candidate list, and a backend that forgot to propagate it would silently
     * tell the user that 2 048 variables are all the variables there are.
     * @param {number} pos
     * @returns {any}
     */
    complete(pos) {
        const ret = wasm.engine_complete(this.__wbg_ptr, pos);
        return takeObject(ret);
    }
    /**
     * The generation of the environment currently loaded, so the webview can
     * tell whether a `StateChanged` it just saw has been applied.
     * @returns {bigint}
     */
    completion_env_generation() {
        const ret = wasm.engine_completion_env_generation(this.__wbg_ptr);
        return BigInt.asUintN(64, ret);
    }
    /**
     * Parse diagnostics plus any splice faults. Rare; JSON is fine (§14).
     *
     * Faults are drained: a splice error is reported once, to the transaction
     * that caused it.
     * @returns {any}
     */
    diagnostics() {
        const ret = wasm.engine_diagnostics(this.__wbg_ptr);
        return takeObject(ret);
    }
    /**
     * Document length in bytes. The webview asserts it against its own encoded
     * length after each transaction; a mismatch means the two buffers have
     * diverged and the wrapper resynchronises with a full replace.
     * @returns {number}
     */
    doc_len() {
        const ret = wasm.engine_doc_len(this.__wbg_ptr);
        return ret >>> 0;
    }
    /**
     * The document as JS sees it. Test and debug affordance — the editor is
     * authoritative for text, never this buffer (06 §2, rule 2).
     * @returns {string}
     */
    doc_text() {
        let deferred1_0;
        let deferred1_1;
        try {
            const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
            wasm.engine_doc_text(retptr, this.__wbg_ptr);
            var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
            var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
            deferred1_0 = r0;
            deferred1_1 = r1;
            return getStringFromWasm0(r0, r1);
        } finally {
            wasm.__wbindgen_add_to_stack_pointer(16);
            wasm.__wbindgen_export(deferred1_0, deferred1_1, 1);
        }
    }
    /**
     * The current generation without re-segmenting.
     * @returns {number}
     */
    generation() {
        const ret = wasm.engine_generation(this.__wbg_ptr);
        return ret >>> 0;
    }
    /**
     * Whole-document lints that need no session state, as frozen
     * `Diagnostic`s. Lints that need live state come from the engine.
     * @returns {any}
     */
    lints() {
        const ret = wasm.engine_lints(this.__wbg_ptr);
        return takeObject(ret);
    }
    /**
     * Flat `i32` view, [`NARRATIVE_STRIDE`] per region — `//|` and `/*md`.
     * @returns {Int32Array}
     */
    narrative_regions() {
        const ret = wasm.engine_narrative_regions(this.__wbg_ptr);
        return takeObject(ret);
    }
    /**
     * A fresh engine over an empty document, generation 0.
     */
    constructor() {
        const ret = wasm.engine_new();
        this.__wbg_ptr = ret;
        EngineFinalization.register(this, this.__wbg_ptr, this);
        return this;
    }
    /**
     * Deterministic quick fixes at `pos`, as frozen `Suggestion`s.
     * @param {number} pos
     * @returns {any}
     */
    quick_fixes(pos) {
        const ret = wasm.engine_quick_fixes(this.__wbg_ptr, pos);
        return takeObject(ret);
    }
    /**
     * Number of regions in the current segmentation.
     * @returns {number}
     */
    region_count() {
        const ret = wasm.engine_region_count(this.__wbg_ptr);
        return ret >>> 0;
    }
    /**
     * Flat `u64` view, [`REGION_HASH_STRIDE`] per region.
     *
     * **THESE ARE HASHES, NOT IDENTITIES.** `BlockId` comes from the engine.
     * @returns {BigUint64Array}
     */
    region_hashes() {
        const ret = wasm.engine_region_hashes(this.__wbg_ptr);
        return takeObject(ret);
    }
    /**
     * Flat `i32` view, [`REGION_STRIDE`] per region.
     * @returns {Int32Array}
     */
    regions_view() {
        const ret = wasm.engine_regions_view(this.__wbg_ptr);
        return takeObject(ret);
    }
    /**
     * Re-segment. Returns the generation, which increments only when the
     * document actually changed — an unchanged document costs one branch.
     *
     * Budget: < 150 µs incremental, 3–8 ms for a cold 10 k-line pass.
     * @returns {number}
     */
    resegment() {
        const ret = wasm.engine_resegment(this.__wbg_ptr);
        return ret >>> 0;
    }
    /**
     * Pointer into wasm memory for JS to write UTF-8 into. Grows on demand.
     *
     * See [`Doc::reserve`]: the returned pointer is valid until the next call.
     * @param {number} bytes
     * @returns {number}
     */
    reserve(bytes) {
        const ret = wasm.engine_reserve(this.__wbg_ptr, bytes);
        return ret >>> 0;
    }
    /**
     * Flat `i32` view, [`SECTION_STRIDE`] per section.
     * @returns {Int32Array}
     */
    sections() {
        const ret = wasm.engine_sections(this.__wbg_ptr);
        return takeObject(ret);
    }
    /**
     * Set the live environment pushed by the engine on `StateChanged`.
     *
     * Takes the engine's own msgpack bytes (§9/§10). A malformed payload keeps
     * the previous environment — completing against a stale variable list is a
     * far smaller failure than a popup that stops working.
     * @param {Uint8Array} msgpack
     */
    set_completion_env(msgpack) {
        const ptr0 = passArray8ToWasm0(msgpack, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.engine_set_completion_env(this.__wbg_ptr, ptr0, len0);
    }
    /**
     * Apply one CM6 change: replace `[from, to)` with `len` bytes already
     * written at `src` in the scratch buffer.
     *
     * Offsets are UTF-8 byte offsets. A rejected splice records a diagnostic and
     * leaves the document unchanged rather than unwinding into the transaction.
     * @param {number} from
     * @param {number} to
     * @param {number} src
     * @param {number} len
     */
    splice(from, to, src, len) {
        wasm.engine_splice(this.__wbg_ptr, from, to, src, len);
    }
    /**
     * Flat `i32` triples `[from, to, tag]` for the requested byte range only.
     *
     * Scoped to the visible range because a 10 k-line file has ~8 k tokens per
     * screen and materialising the whole document's stream would cost more than
     * the parse (06 §3.4).
     * @param {number} from
     * @param {number} to
     * @returns {Int32Array}
     */
    tokens(from, to) {
        const ret = wasm.engine_tokens(this.__wbg_ptr, from, to);
        return takeObject(ret);
    }
}
if (Symbol.dispose) Engine.prototype[Symbol.dispose] = Engine.prototype.free;

/**
 * Version of the flat view layout this module was built with. `loader.ts`
 * refuses a module whose value differs from its own.
 * @returns {number}
 */
export function abi_version() {
    const ret = wasm.abi_version();
    return ret >>> 0;
}

/**
 * Whether a real segmenter is linked.
 *
 * False for a harness-only build, which produces no regions at all. The loader
 * treats false as fatal in production and as "fall back to the fenced stub" in
 * development; without this, an unlinked module would look exactly like an
 * empty document.
 * @returns {boolean}
 */
export function engine_linked() {
    const ret = wasm.engine_linked();
    return ret !== 0;
}

/**
 * Install the panic hook, when this build has one. Called by wasm-bindgen at
 * module instantiation.
 */
export function start() {
    wasm.start();
}
function __wbg_get_imports() {
    const import0 = {
        __proto__: null,
        __wbg_Error_408e67f47ca7b58b: function(arg0, arg1) {
            const ret = Error(getStringFromWasm0(arg0, arg1));
            return addHeapObject(ret);
        },
        __wbg___wbindgen_throw_bb96b2010945f0bc: function(arg0, arg1) {
            throw new Error(getStringFromWasm0(arg0, arg1));
        },
        __wbg_new_116be93542d39019: function() {
            const ret = new Array();
            return addHeapObject(ret);
        },
        __wbg_new_ebe3e0f6837f0879: function() {
            const ret = new Object();
            return addHeapObject(ret);
        },
        __wbg_new_from_slice_1f7a0d975f26baea: function(arg0, arg1) {
            const ret = new Int32Array(getArrayI32FromWasm0(arg0, arg1));
            return addHeapObject(ret);
        },
        __wbg_new_from_slice_b61d590a0b3abdb3: function(arg0, arg1) {
            const ret = new BigUint64Array(getArrayU64FromWasm0(arg0, arg1));
            return addHeapObject(ret);
        },
        __wbg_set_6be42768c690e380: function(arg0, arg1, arg2) {
            getObject(arg0)[takeObject(arg1)] = takeObject(arg2);
        },
        __wbg_set_a80955eb93b145c6: function(arg0, arg1, arg2) {
            getObject(arg0)[arg1 >>> 0] = takeObject(arg2);
        },
        __wbindgen_cast_0000000000000001: function(arg0) {
            // Cast intrinsic for `F64 -> Externref`.
            const ret = arg0;
            return addHeapObject(ret);
        },
        __wbindgen_cast_0000000000000002: function(arg0, arg1) {
            // Cast intrinsic for `Ref(String) -> Externref`.
            const ret = getStringFromWasm0(arg0, arg1);
            return addHeapObject(ret);
        },
        __wbindgen_cast_0000000000000003: function(arg0) {
            // Cast intrinsic for `U64 -> Externref`.
            const ret = BigInt.asUintN(64, arg0);
            return addHeapObject(ret);
        },
        __wbindgen_object_clone_ref: function(arg0) {
            const ret = getObject(arg0);
            return addHeapObject(ret);
        },
        __wbindgen_object_drop_ref: function(arg0) {
            takeObject(arg0);
        },
    };
    return {
        __proto__: null,
        "./stratum_wasm_bg.js": import0,
    };
}

const EngineFinalization = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(ptr => wasm.__wbg_engine_free(ptr, 1));

function addHeapObject(obj) {
    if (heap_next === heap.length) heap.push(heap.length + 1);
    const idx = heap_next;
    heap_next = heap[idx];

    heap[idx] = obj;
    return idx;
}

function dropObject(idx) {
    if (idx < 1028) return;
    heap[idx] = heap_next;
    heap_next = idx;
}

function getArrayI32FromWasm0(ptr, len) {
    ptr = ptr >>> 0;
    return getInt32ArrayMemory0().subarray(ptr / 4, ptr / 4 + len);
}

function getArrayU64FromWasm0(ptr, len) {
    ptr = ptr >>> 0;
    return getBigUint64ArrayMemory0().subarray(ptr / 8, ptr / 8 + len);
}

let cachedBigUint64ArrayMemory0 = null;
function getBigUint64ArrayMemory0() {
    if (cachedBigUint64ArrayMemory0 === null || cachedBigUint64ArrayMemory0.byteLength === 0) {
        cachedBigUint64ArrayMemory0 = new BigUint64Array(wasm.memory.buffer);
    }
    return cachedBigUint64ArrayMemory0;
}

let cachedDataViewMemory0 = null;
function getDataViewMemory0() {
    if (cachedDataViewMemory0 === null || cachedDataViewMemory0.buffer.detached === true || (cachedDataViewMemory0.buffer.detached === undefined && cachedDataViewMemory0.buffer !== wasm.memory.buffer)) {
        cachedDataViewMemory0 = new DataView(wasm.memory.buffer);
    }
    return cachedDataViewMemory0;
}

let cachedInt32ArrayMemory0 = null;
function getInt32ArrayMemory0() {
    if (cachedInt32ArrayMemory0 === null || cachedInt32ArrayMemory0.byteLength === 0) {
        cachedInt32ArrayMemory0 = new Int32Array(wasm.memory.buffer);
    }
    return cachedInt32ArrayMemory0;
}

function getStringFromWasm0(ptr, len) {
    return decodeText(ptr >>> 0, len);
}

let cachedUint8ArrayMemory0 = null;
function getUint8ArrayMemory0() {
    if (cachedUint8ArrayMemory0 === null || cachedUint8ArrayMemory0.byteLength === 0) {
        cachedUint8ArrayMemory0 = new Uint8Array(wasm.memory.buffer);
    }
    return cachedUint8ArrayMemory0;
}

function getObject(idx) { return heap[idx]; }

let heap = new Array(1024).fill(undefined);
heap.push(undefined, null, true, false);

let heap_next = heap.length;

function passArray8ToWasm0(arg, malloc) {
    const ptr = malloc(arg.length * 1, 1) >>> 0;
    getUint8ArrayMemory0().set(arg, ptr / 1);
    WASM_VECTOR_LEN = arg.length;
    return ptr;
}

function takeObject(idx) {
    const ret = getObject(idx);
    dropObject(idx);
    return ret;
}

let cachedTextDecoder = new TextDecoder('utf-8', { ignoreBOM: true, fatal: true });
cachedTextDecoder.decode();
const MAX_SAFARI_DECODE_BYTES = 2146435072;
let numBytesDecoded = 0;
function decodeText(ptr, len) {
    numBytesDecoded += len;
    if (numBytesDecoded >= MAX_SAFARI_DECODE_BYTES) {
        cachedTextDecoder = new TextDecoder('utf-8', { ignoreBOM: true, fatal: true });
        cachedTextDecoder.decode();
        numBytesDecoded = len;
    }
    return cachedTextDecoder.decode(getUint8ArrayMemory0().subarray(ptr, ptr + len));
}

let WASM_VECTOR_LEN = 0;

let wasmModule, wasmInstance, wasm;
function __wbg_finalize_init(instance, module) {
    wasmInstance = instance;
    wasm = instance.exports;
    wasmModule = module;
    cachedBigUint64ArrayMemory0 = null;
    cachedDataViewMemory0 = null;
    cachedInt32ArrayMemory0 = null;
    cachedUint8ArrayMemory0 = null;
    wasm.__wbindgen_start();
    return wasm;
}

async function __wbg_load(module, imports) {
    if (typeof Response === 'function' && module instanceof Response) {
        if (!module.ok) {
            throw new Error(`failed to fetch Wasm: ${module.status} ${module.statusText} fetching '${module.url}'`);
        }

        if (typeof WebAssembly.instantiateStreaming === 'function') {
            try {
                return await WebAssembly.instantiateStreaming(module, imports);
            } catch (e) {
                const validResponse = expectedResponseType(module.type);

                if (validResponse && module.headers.get('Content-Type') !== 'application/wasm') {
                    console.warn("`WebAssembly.instantiateStreaming` failed because your server does not serve Wasm with `application/wasm` MIME type. Falling back to `WebAssembly.instantiate` which is slower. Original error:\n", e);

                } else { throw e; }
            }
        }

        const bytes = await module.arrayBuffer();
        return await WebAssembly.instantiate(bytes, imports);
    } else {
        const instance = await WebAssembly.instantiate(module, imports);

        if (instance instanceof WebAssembly.Instance) {
            return { instance, module };
        } else {
            return instance;
        }
    }

    function expectedResponseType(type) {
        switch (type) {
            case 'basic': case 'cors': case 'default': return true;
        }
        return false;
    }
}

function initSync(module) {
    if (wasm !== undefined) return wasm;


    if (module !== undefined) {
        if (Object.getPrototypeOf(module) === Object.prototype) {
            ({module} = module)
        } else {
            console.warn('using deprecated parameters for `initSync()`; pass a single object instead')
        }
    }

    const imports = __wbg_get_imports();
    if (!(module instanceof WebAssembly.Module)) {
        module = new WebAssembly.Module(module);
    }
    const instance = new WebAssembly.Instance(module, imports);
    return __wbg_finalize_init(instance, module);
}

async function __wbg_init(module_or_path) {
    if (wasm !== undefined) return wasm;


    if (module_or_path !== undefined) {
        if (Object.getPrototypeOf(module_or_path) === Object.prototype) {
            ({module_or_path} = module_or_path)
        } else {
            console.warn('using deprecated parameters for the initialization function; pass a single object instead')
        }
    }

    if (module_or_path === undefined) {
        module_or_path = new URL('stratum_wasm_bg.wasm', import.meta.url);
    }
    const imports = __wbg_get_imports();

    if (typeof module_or_path === 'string' || (typeof Request === 'function' && module_or_path instanceof Request) || (typeof URL === 'function' && module_or_path instanceof URL)) {
        module_or_path = fetch(module_or_path);
    }

    const { instance, module } = await __wbg_load(await module_or_path, imports);

    return __wbg_finalize_init(instance, module);
}

export { initSync, __wbg_init as default };
