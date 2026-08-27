/**
 * A minimal MessagePack reader, for the development stub only.
 *
 * The real module decodes `CompletionEnv` with `rmp-serde` against the frozen
 * struct. The stub has no Rust and no dependencies, but it still has to accept
 * the *same bytes* the engine broadcasts — otherwise `set_completion_env` would
 * be the one method where the two backends behave differently, and the popup's
 * "2 048 of 32 767" path (A11) could not be built or seen until W11b lands.
 *
 * Scope is exactly what `CompletionEnv` uses: maps with string keys, arrays of
 * strings, strings, unsigned integers and booleans. Anything else decodes to
 * `null` rather than throwing — a stub that crashes the editor on an unexpected
 * byte is worse than one that completes nothing.
 */

const decoder = new TextDecoder("utf-8", { fatal: false });

/** Anything this reader can produce. */
export type MsgValue = null | boolean | number | string | MsgValue[] | { [k: string]: MsgValue };

class Reader {
  private view: DataView;
  private bytes: Uint8Array;
  private pos = 0;

  constructor(bytes: Uint8Array) {
    this.bytes = bytes;
    this.view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  }

  read(): MsgValue {
    if (this.pos >= this.bytes.length) return null;
    const b = this.bytes[this.pos++] as number;

    if (b <= 0x7f) return b; // positive fixint
    if (b >= 0xe0) return b - 0x100; // negative fixint
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
      case 0xcc:
        return this.u8();
      case 0xcd:
        return this.u16();
      case 0xce:
        return this.u32();
      case 0xcf:
        return this.u64();
      case 0xd0:
        return this.i8();
      case 0xd1:
        return this.i16();
      case 0xd2:
        return this.i32();
      case 0xd9:
        return this.str(this.u8());
      case 0xda:
        return this.str(this.u16());
      case 0xdb:
        return this.str(this.u32());
      case 0xdc:
        return this.array(this.u16());
      case 0xdd:
        return this.array(this.u32());
      case 0xde:
        return this.map(this.u16());
      case 0xdf:
        return this.map(this.u32());
      default:
        // Floats, bins, exts, 64-bit signed: not in CompletionEnv. Stop here
        // rather than guessing a width and desynchronising the whole stream.
        this.pos = this.bytes.length;
        return null;
    }
  }

  private u8(): number {
    return this.bytes[this.pos++] as number;
  }
  private i8(): number {
    const v = this.view.getInt8(this.pos);
    this.pos += 1;
    return v;
  }
  private u16(): number {
    const v = this.view.getUint16(this.pos);
    this.pos += 2;
    return v;
  }
  private i16(): number {
    const v = this.view.getInt16(this.pos);
    this.pos += 2;
    return v;
  }
  private u32(): number {
    const v = this.view.getUint32(this.pos);
    this.pos += 4;
    return v;
  }
  private i32(): number {
    const v = this.view.getInt32(this.pos);
    this.pos += 4;
    return v;
  }
  private u64(): number {
    // `CompletionEnv.generation` is a u64. Numbers past 2^53 would lose
    // precision, and a generation counter never gets there.
    const v = this.view.getBigUint64(this.pos);
    this.pos += 8;
    return Number(v);
  }
  private str(len: number): string {
    const s = decoder.decode(this.bytes.subarray(this.pos, this.pos + len));
    this.pos += len;
    return s;
  }
  private array(len: number): MsgValue[] {
    const out: MsgValue[] = new Array(len);
    for (let i = 0; i < len; i++) out[i] = this.read();
    return out;
  }
  private map(len: number): { [k: string]: MsgValue } {
    const out: { [k: string]: MsgValue } = {};
    for (let i = 0; i < len; i++) {
      const key = this.read();
      const value = this.read();
      if (typeof key === "string") out[key] = value;
    }
    return out;
  }
}

/** Decode one MessagePack value. Returns `null` on anything unsupported. */
export function decodeMsgpack(bytes: Uint8Array): MsgValue {
  return new Reader(bytes).read();
}

/** The `CompletionEnv` fields the stub can use. */
export interface StubEnv {
  /** Environment generation, so the editor can tell a push landed. */
  generation: number;
  /** Variable names in storage order, already capped by the engine. */
  varnames: string[];
  /** True variable count, so the popup can say "2 048 of 32 767". */
  varTotal: number;
  /** The engine shed entries to stay inside its byte ceiling (A11). */
  truncated: boolean;
  /** Local macro names. */
  locals: string[];
  /** Global macro names. */
  globals: string[];
  /** Scalar names. */
  scalars: string[];
  /** Matrix names. */
  matrices: string[];
  /** Frame names. */
  frames: string[];
  /** Program names. */
  programs: string[];
}

function strings(v: MsgValue | undefined): string[] {
  return Array.isArray(v) ? v.filter((x): x is string => typeof x === "string") : [];
}

/** Project a decoded `CompletionEnv` onto what the stub completes from. */
export function readEnv(bytes: Uint8Array): StubEnv | null {
  const v = decodeMsgpack(bytes);
  if (!v || typeof v !== "object" || Array.isArray(v)) return null;
  const m = v as { [k: string]: MsgValue };
  return {
    generation: typeof m.generation === "number" ? m.generation : 0,
    varnames: strings(m.varnames),
    varTotal: typeof m.var_total === "number" ? m.var_total : strings(m.varnames).length,
    truncated: m.truncated === true,
    locals: strings(m.locals),
    globals: strings(m.globals),
    scalars: strings(m.scalars),
    matrices: strings(m.matrices),
    frames: strings(m.frames),
    programs: strings(m.programs),
  };
}
