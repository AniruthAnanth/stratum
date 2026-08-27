/**
 * Test fixtures — **imported only by `*.test.tsx`, never by the app.**
 *
 * The interesting half of this file is [`scenarioA`], which reads
 * `tests/fixtures/mock/scenario_a.msgpack` — W07's committed stream — and decodes
 * the real frames into the real envelopes. That matters more than it looks:
 *
 *  * It is the only way, before W17 generates `src/ipc/types.ts`, to check that
 *    the structural views in `types.ts` describe what serde ACTUALLY puts on the
 *    wire — that `ResultPayload::Summarize` flattens beside `"kind"` rather than
 *    nesting, that `CardAction::RawOutput` is `{"action":"raw_output"}`, that
 *    `Option<String>` is `nil` and not an absent key. A hand-written view that is
 *    merely plausible is worth nothing; one that decodes 74 observations of
 *    StataMP 18.5's own `regress` is evidence.
 *  * Every number in it is StataMP 18.5's, copied from
 *    `tests/golden/stata18/core_surface.log`. A renderer built against invented
 *    numbers is a renderer that has never seen a real column width.
 *
 * The MessagePack reader below covers exactly what `rmp_serde::to_vec_named`
 * emits for these types: maps with string keys, arrays, strings, both integer
 * families, both float widths, booleans, nil and bin. `u64` is read as `bigint`
 * and narrowed to `number` only when it is exactly representable, so the mock's
 * `sample_hash` (0x5354415441313835, above 2^53) survives as itself.
 */

import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import type { DatasetStateId, ExecId, ResultId } from "../ipc/hand";
import type { CardActionView, PayloadKind, ResultEnvelopeView, ResultPayloadView } from "./types";

// ---------------------------------------------------------------------------
// MessagePack
// ---------------------------------------------------------------------------

export type MsgValue =
  | null
  | boolean
  | number
  | bigint
  | string
  | Uint8Array
  | MsgValue[]
  | { [k: string]: MsgValue };

const utf8 = new TextDecoder("utf-8", { fatal: true });

class Reader {
  private pos = 0;
  private readonly view: DataView;

  constructor(private readonly bytes: Uint8Array) {
    this.view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  }

  get done(): boolean {
    return this.pos >= this.bytes.length;
  }

  private u8(): number {
    const b = this.bytes[this.pos];
    if (b === undefined) throw new RangeError("msgpack: truncated");
    this.pos += 1;
    return b;
  }

  private take(n: number): Uint8Array {
    const out = this.bytes.subarray(this.pos, this.pos + n);
    if (out.length !== n) throw new RangeError("msgpack: truncated");
    this.pos += n;
    return out;
  }

  private str(n: number): string {
    return utf8.decode(this.take(n));
  }

  private array(n: number): MsgValue[] {
    const out: MsgValue[] = [];
    for (let i = 0; i < n; i++) out.push(this.read());
    return out;
  }

  private map(n: number): { [k: string]: MsgValue } {
    const out: { [k: string]: MsgValue } = {};
    for (let i = 0; i < n; i++) {
      const key = this.read();
      out[typeof key === "string" ? key : String(key)] = this.read();
    }
    return out;
  }

  /** `u64` stays a bigint only when a double would lose it. */
  private big(v: bigint): number | bigint {
    return v <= BigInt(Number.MAX_SAFE_INTEGER) && v >= BigInt(-Number.MAX_SAFE_INTEGER)
      ? Number(v)
      : v;
  }

  read(): MsgValue {
    const b = this.u8();
    if (b <= 0x7f) return b;
    if (b >= 0xe0) return b - 0x100;
    if ((b & 0xf0) === 0x80) return this.map(b & 0x0f);
    if ((b & 0xf0) === 0x90) return this.array(b & 0x0f);
    if ((b & 0xe0) === 0xa0) return this.str(b & 0x1f);

    switch (b) {
      case 0xc0:
        return null;
      case 0xc2:
        return false;
      case 0xc3:
        return true;
      case 0xc4:
        return this.take(this.u8()).slice();
      case 0xc5: {
        const n = this.view.getUint16(this.pos);
        this.pos += 2;
        return this.take(n).slice();
      }
      case 0xc6: {
        const n = this.view.getUint32(this.pos);
        this.pos += 4;
        return this.take(n).slice();
      }
      case 0xca: {
        const v = this.view.getFloat32(this.pos);
        this.pos += 4;
        return v;
      }
      case 0xcb: {
        const v = this.view.getFloat64(this.pos);
        this.pos += 8;
        return v;
      }
      case 0xcc:
        return this.u8();
      case 0xcd: {
        const v = this.view.getUint16(this.pos);
        this.pos += 2;
        return v;
      }
      case 0xce: {
        const v = this.view.getUint32(this.pos);
        this.pos += 4;
        return v;
      }
      case 0xcf: {
        const v = this.view.getBigUint64(this.pos);
        this.pos += 8;
        return this.big(v);
      }
      case 0xd0: {
        const v = this.view.getInt8(this.pos);
        this.pos += 1;
        return v;
      }
      case 0xd1: {
        const v = this.view.getInt16(this.pos);
        this.pos += 2;
        return v;
      }
      case 0xd2: {
        const v = this.view.getInt32(this.pos);
        this.pos += 4;
        return v;
      }
      case 0xd3: {
        const v = this.view.getBigInt64(this.pos);
        this.pos += 8;
        return this.big(v);
      }
      case 0xd9:
        return this.str(this.u8());
      case 0xda: {
        const n = this.view.getUint16(this.pos);
        this.pos += 2;
        return this.str(n);
      }
      case 0xdb: {
        const n = this.view.getUint32(this.pos);
        this.pos += 4;
        return this.str(n);
      }
      case 0xdc: {
        const n = this.view.getUint16(this.pos);
        this.pos += 2;
        return this.array(n);
      }
      case 0xdd: {
        const n = this.view.getUint32(this.pos);
        this.pos += 4;
        return this.array(n);
      }
      case 0xde: {
        const n = this.view.getUint16(this.pos);
        this.pos += 2;
        return this.map(n);
      }
      case 0xdf: {
        const n = this.view.getUint32(this.pos);
        this.pos += 4;
        return this.map(n);
      }
      default:
        throw new TypeError(`msgpack: unsupported byte 0x${b.toString(16)}`);
    }
  }
}

export function decodeMsgpack(bytes: Uint8Array): MsgValue {
  return new Reader(bytes).read();
}

// ---------------------------------------------------------------------------
// §10 framing
// ---------------------------------------------------------------------------

/** `len:u32LE | kind:u8 | corr:u32LE | payload`, `len` counting the 5-byte head. */
export function decodeFrames(bytes: Uint8Array): MsgValue[] {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const out: MsgValue[] = [];
  let at = 0;
  while (at + 4 <= bytes.length) {
    const len = view.getUint32(at, true);
    const bodyAt = at + 4 + 5;
    const bodyEnd = at + 4 + len;
    if (bodyEnd > bytes.length) throw new RangeError("frame runs past the buffer");
    out.push(decodeMsgpack(bytes.subarray(bodyAt, bodyEnd)));
    at = bodyEnd;
  }
  return out;
}

// ---------------------------------------------------------------------------
// The committed stream
// ---------------------------------------------------------------------------

const here = dirname(fileURLToPath(import.meta.url));
export const repoRoot = resolve(here, "../../../..");

export function goldenLog(name = "core_surface"): string {
  return readFileSync(resolve(repoRoot, `tests/golden/stata18/${name}.log`), "utf8");
}

/** Every event of `tests/fixtures/mock/scenario_a.msgpack`, in order. */
export function scenarioAEvents(): { [k: string]: MsgValue }[] {
  const bytes = readFileSync(resolve(repoRoot, "tests/fixtures/mock/scenario_a.msgpack"));
  return decodeFrames(new Uint8Array(bytes)).map((event) => {
    if (typeof event !== "object" || event === null || Array.isArray(event)) {
      throw new TypeError("EngineEvent is not a map");
    }
    return event as { [k: string]: MsgValue };
  });
}

/**
 * The three `ResultEnvelope`s: `sysuse` (DataChanged), `summarize` and `regress`.
 *
 * The cast is the point of the exercise rather than a shortcut around it: if the
 * wire shape did not match [`ResultEnvelopeView`], the assertions in
 * `fixture.test.ts` that read `.payloads[0].kind`, `.raw.head` and
 * `.actions[…].action` off these values would fail.
 */
export function scenarioAEnvelopes(): ResultEnvelopeView[] {
  return scenarioAEvents()
    .filter((event) => event["event"] === "result")
    .map((event) => event["envelope"] as unknown as ResultEnvelopeView);
}

// ---------------------------------------------------------------------------
// Synthetic envelopes for the variants the mock stream does not contain
// ---------------------------------------------------------------------------

/**
 * An envelope around one payload. The scaffolding — ids, `raw`, `layout_hint` —
 * is the mock's shape; only the payload varies, which is what makes
 * "every variant has `Raw ▸` in the same position" a statement about the SHELL
 * rather than about ten hand-built cards.
 */
export function envelopeOf(
  payload: ResultPayloadView,
  actions: readonly CardActionView[] = [{ action: "raw_output" }],
  cmdline = "summarize price mpg",
): ResultEnvelopeView {
  const raw = "    Variable |        Obs\n       price |         74\n";
  return {
    result: 41 as unknown as ResultId,
    revision: 0,
    exec: 41 as unknown as ExecId,
    dataset_state: 17 as unknown as DatasetStateId,
    cmdline,
    duration_us: 80_000,
    rc: payload.kind === "error" ? 111 : 0,
    payloads: [payload],
    raw: {
      bytes: raw.length,
      lines: 2,
      head: raw,
      truncated: false,
      asset: { path: "result/1/41/raw", mime: "text/plain; charset=utf-8", bytes: raw.length },
    },
    layout_hint: { rows: 2, cols: 6, est_px: 132 },
    actions,
  };
}

/** One payload of every `ResultPayload` variant, keyed by its wire tag. */
export function payloadOfEveryKind(): Readonly<Record<PayloadKind, ResultPayloadView>> {
  return {
    log: { kind: "log", lines: 1, runs: [{ text: "(1978 automobile data)\n", style: "text" }] },
    summarize: {
      kind: "summarize",
      detail: false,
      weight: null,
      qualifier: null,
      rows: [
        {
          var: "price",
          label: "Price",
          format: "%8.0gc",
          missing: 3,
          display: {
            obs: "74",
            mean: "6165.257",
            sd: "2949.496",
            min: "3291",
            max: "15906",
          },
          detail: null,
          var_kind: "numeric",
          sparkline: [22, 14, 9, 8, 5, 4, 3, 2, 2, 1, 1, 1, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1],
        },
      ],
    },
    tabulate: {
      kind: "tabulate",
      row_var: "foreign",
      col_var: "rep78",
      row_label: "Car origin",
      col_label: "Repair record 1978",
      row_keys: [
        [0, "Domestic"],
        [1, "Foreign"],
      ],
      col_keys: [
        [1, null],
        [2, null],
        [3, null],
        [4, null],
        [5, null],
      ],
      counts: [2, 8, 27, 9, 2, 0, 0, 3, 9, 9],
      row_totals: [48, 21],
      col_totals: [2, 8, 30, 18, 11],
      total: 69,
      requested: ["freq"],
      tests: [{ name: "pearson", display: "          Pearson chi2(4) =  27.2640   Pr = 0.000" }],
      truncated: null,
    },
    estimation: {
      kind: "estimation",
      cmd: "regress",
      cmdline: "regress price mpg",
      depvar: "price",
      n: 74,
      eq_names: [""],
      terms: [
        {
          eq: 0,
          name: "mpg",
          display: "mpg",
          b: 21.85359,
          ci_lo: -126.1758,
          ci_hi: 169.883,
          p: 0.769,
          display_num: ["21.8536", "74.22114", "0.29", "0.769", "-126.1758", "169.883"],
          omitted: false,
          base: false,
          empty: false,
        },
      ],
      scalars: [["r2", 0.4996]],
      macros: [],
      anova: null,
      vce: "ols",
      estimates_name: null,
      sample_hash: "6004496033318516789",
      diagnostics: [],
    },
    graph: {
      kind: "graph",
      name: "Graph",
      asset: { path: "graph/1/41.svg", mime: "image/svg+xml", bytes: 1024 },
      intrinsic_pt: [400, 300],
      scheme: "stratum",
      source_cmd: "histogram price",
    },
    table: {
      kind: "table",
      title: "correlate",
      colnames: ["price", "mpg"],
      rownames: ["price", "mpg"],
      cells: [
        { t: "num", value: 1, display: "1.0000" },
        null,
        { t: "num", value: -0.4686, display: "-0.4686" },
        { t: "num", value: 1, display: "1.0000" },
      ],
      col_align: ["decimal", "decimal"],
    },
    scalars: {
      kind: "scalars",
      values: [["r(mean)", { t: "num", value: 6165.256756756757, display: "6165.257" }]],
    },
    data_changed: {
      kind: "data_changed",
      frame: "default",
      obs_before: 0,
      obs_after: 74,
      vars_before: 0,
      vars_after: 12,
      created: [],
      modified: [],
      dropped: [],
      renamed: [],
      notes: ["(1978 automobile data)"],
    },
    error: {
      kind: "error",
      severity: "error",
      code: "STATA0111",
      stata_rc: 111,
      message: "incme not found",
      offending_token: "incme",
      suggestions: [{ label: "Did you mean `income`?", kind: "rename", edits: [{}] }],
      notes: [],
    },
    unknown: { kind: "unknown" },
  };
}
