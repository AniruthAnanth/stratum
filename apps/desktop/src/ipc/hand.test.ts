/**
 * `hand.ts` against the checked-in oracles.
 *
 * The three SDP1 fixtures are W00's (A29) and were captured from StataNow 18.5
 * MP, not written by us. That is the whole value of asserting against them: one
 * side of the comparison did not come from our code, so agreement means the wire
 * format is right rather than that two of our functions are consistent.
 */

import { existsSync, readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { describe, expect, it } from "vitest";
import {
  type BlockStatusState,
  DataPageError,
  type HasBlockState,
  STATUS_RANK,
  clientKey,
  codeHash,
  decodeDataPage,
  isCodeHash,
  worseOf,
} from "./hand";

/**
 * Walks up to the repository root rather than hard-coding `../../../..`.
 * `new URL(..., import.meta.url)` is not usable here: Vite rewrites it into an
 * asset import and then refuses to serve a path outside the project root.
 */
const repoRoot = ((): string => {
  let dir = process.cwd();
  while (!existsSync(resolve(dir, "tests/fixtures/sdp1"))) {
    const parent = dirname(dir);
    if (parent === dir) throw new Error("tests/fixtures/sdp1 not found above the cwd");
    dir = parent;
  }
  return dir;
})();

const fixture = (name: string): ArrayBuffer => {
  const bytes = readFileSync(resolve(repoRoot, "tests/fixtures/sdp1", name));
  return bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength) as ArrayBuffer;
};

describe("decodeDataPage — auto_40x12.bin (the normative fixture)", () => {
  const page = decodeDataPage(fixture("auto_40x12.bin"));

  it("reads the header", () => {
    // `state = 17` is spec §13's own worked example, chosen by the fixture so a
    // decoder that forgets to read `state` fails visibly instead of accidentally
    // agreeing with a 0 default.
    expect(page.state).toBe(17);
    expect(page.row0).toBe(0);
    expect(page.nrows).toBe(40);
    expect(page.seq).toBe(1);
    expect(page.cols).toHaveLength(12);
  });

  it("is all `text` columns in storage order", () => {
    expect(page.cols.map((c) => c.kind)).toEqual(Array(12).fill("text"));
    expect(page.cols.map((c) => c.idx)).toEqual([0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]);
  });

  it("decodes the first three `make` values the README names", () => {
    const make = page.column(0);
    expect(make?.kind).toBe("text");
    if (make?.kind !== "text") throw new Error("unreachable");
    expect(make.cell(0)).toBe("AMC Concord");
    expect(make.cell(1)).toBe("AMC Pacer");
    expect(make.cell(2)).toBe("AMC Spirit");
    // aux[0..4] = 0, 11, 20, 30 — the offsets the README prints at 0x03B0.
    expect(Array.from(make.offsets.slice(0, 4))).toEqual([0, 11, 20, 30]);
    expect(make.offsets[40]).toBe(make.arena.length);
  });

  it("carries Display formatting done in the CORE, not here", () => {
    const price = page.column(1);
    const headroom = page.column(4);
    const foreign = page.column(11);
    if (price?.kind !== "text" || headroom?.kind !== "text" || foreign?.kind !== "text") {
      throw new Error("unreachable");
    }
    // %8.0gc inserts the comma; %6.1f keeps the trailing zero; the value label
    // replaces the number. All four behaviours are Stata's, captured, not ours.
    expect(price.cell(0)).toBe("4,099");
    expect(headroom.cell(0)).toBe("2.5");
    expect(foreign.cell(0)).toBe("Domestic");
    // And no format padding: a Display cell holds what `string(x, "%fmt")`
    // returns, already trimmed (fixture README §2.4).
    expect(price.cell(0)).not.toMatch(/^\s/);
  });

  it("renders a missing value as Stata renders it", () => {
    const rep78 = page.column(3);
    if (rep78?.kind !== "text") throw new Error("unreachable");
    // README §3: rep78 observations 3 and 7, 1-based.
    expect(rep78.cell(2)).toBe(".");
    expect(rep78.cell(6)).toBe(".");
  });

  it("decodes every one of the 480 cells without throwing", () => {
    let count = 0;
    for (const col of page.cols) {
      if (col.kind !== "text") continue;
      for (let row = 0; row < page.nrows; row++) {
        expect(typeof col.cell(row)).toBe("string");
        count++;
      }
    }
    expect(count).toBe(480);
  });
});

describe("decodeDataPage — auto_40x12_edit.bin (the `num` branch)", () => {
  const page = decodeDataPage(fixture("auto_40x12_edit.bin"));

  it("is one text column and eleven numeric ones", () => {
    expect(page.cols[0]?.kind).toBe("text");
    expect(page.cols.slice(1).map((c) => c.kind)).toEqual(Array(11).fill("num"));
  });

  it("widens a Stata float through its exact f32 value", () => {
    const gearRatio = page.column(10);
    if (gearRatio?.kind !== "num") throw new Error("unreachable");
    // 04 §2.6: a float widens through its exact f32 value, so this is
    // 3.5799999237060547 and NOT 3.58. A decoder that rounded would pass a
    // `toBeCloseTo` and be wrong.
    expect(gearRatio.values[0]).toBe(3.5799999237060547);
  });

  it("marks the two missing cells and carries Stata's own sentinel bits", () => {
    const rep78 = page.column(3);
    if (rep78?.kind !== "num") throw new Error("unreachable");
    expect(rep78.isMissing(2)).toBe(true);
    expect(rep78.isMissing(6)).toBe(true);
    expect(rep78.isMissing(0)).toBe(false);
    expect(rep78.tags[2]).toBe(0); // plain system missing `.`
    expect(rep78.tags[0]).toBe(255);

    // The tag is redundant with the payload by construction; check the other
    // half, because "the decoder read the tag column" and "the f64 really is the
    // sentinel" are different claims and only the second one is about the wire.
    const bits = new DataView(new ArrayBuffer(8));
    bits.setFloat64(0, rep78.values[2] as number, true);
    expect(bits.getBigUint64(0, true)).toBe(0x7fe0_0000_0000_0000n);
  });

  it("keeps `make` byte-identical to the Display page", () => {
    const display = decodeDataPage(fixture("auto_40x12.bin")).column(0);
    const edit = page.column(0);
    if (display?.kind !== "text" || edit?.kind !== "text") throw new Error("unreachable");
    // str18 with %-18s: the two renderings coincide, which is the README's own
    // stated reason for including this column in both fixtures.
    expect(Array.from(edit.arena)).toEqual(Array.from(display.arena));
  });
});

describe("decodeDataPage — strl_3x2_edit.bin (the `blob` branch)", () => {
  const page = decodeDataPage(fixture("strl_3x2_edit.bin"));

  it("matches the header the README prints verbatim", () => {
    expect(page.state).toBe(1);
    expect(page.nrows).toBe(3);
    expect(page.cols.map((c) => c.kind)).toEqual(["blob", "text"]);
  });

  it("distinguishes an empty strL from an absent one", () => {
    const big = page.column(0);
    if (big?.kind !== "blob") throw new Error("unreachable");
    // README §3: row 3 is the empty strL, so aux[2] === aux[3] — the case a
    // decoder that treats "zero length" as "absent" gets wrong.
    expect(big.offsets[2]).toBe(big.offsets[3]);
    expect(big.bytes(2).length).toBe(0);
    expect(big.cell(2)).toBe("");
    expect(big.offsets[3]).toBe(213);
  });

  it("reads the bitmap as LSB-first and finds it all zero", () => {
    const big = page.column(0);
    if (big?.kind !== "blob") throw new Error("unreachable");
    expect(big.binaryBitmap.length).toBe(1); // ceil(3/8)
    expect([0, 1, 2].map((r) => big.isBinary(r))).toEqual([false, false, false]);
    expect(typeof big.cell(0)).toBe("string");
  });
});

describe("decodeDataPage — rejection", () => {
  it("rejects a buffer that is not SDP1", () => {
    const buf = new ArrayBuffer(16);
    new DataView(buf).setUint32(0, 0x53445030, false); // "SDP0"
    expect(() => decodeDataPage(buf)).toThrow(DataPageError);
  });

  it("rejects a header_len that runs past the buffer", () => {
    const buf = new ArrayBuffer(16);
    const view = new DataView(buf);
    view.setUint32(0, 0x53445031, false);
    view.setUint32(4, 4096, true);
    expect(() => decodeDataPage(buf)).toThrow(/runs past the buffer/);
  });

  it("rejects a truncated column rather than reading past it", () => {
    // Take the real fixture and lie about one column's length.
    const source = new Uint8Array(fixture("auto_40x12.bin"));
    const headerLen = new DataView(source.buffer).getUint32(4, true);
    const header = JSON.parse(new TextDecoder().decode(source.subarray(8, 8 + headerLen))) as {
      cols: { len: number }[];
    };
    const first = header.cols[0];
    if (first === undefined) throw new Error("unreachable");
    first.len = 1 << 30;
    const rewritten = new TextEncoder().encode(JSON.stringify(header));
    expect(rewritten.length).toBeLessThanOrEqual(headerLen);
    const buf = source.slice();
    buf.set(rewritten, 8);
    buf.fill(0x20, 8 + rewritten.length, 8 + headerLen);
    expect(() => decodeDataPage(buf.buffer as ArrayBuffer)).toThrow(/out of bounds/);
  });
});

describe("worseOf — the display rule (ARCHITECTURE C20)", () => {
  const s = (state: BlockStatusState): HasBlockState => ({ state });

  it("ranks the seven judgements in the documented order", () => {
    const order: BlockStatusState[] = [
      "never_run",
      "broken",
      "failed",
      "interrupted",
      "stale",
      "current_unverifiable",
      "current",
    ];
    for (let i = 1; i < order.length; i++) {
      const worse = order[i - 1] as BlockStatusState;
      const better = order[i] as BlockStatusState;
      expect(STATUS_RANK[worse]).toBeLessThan(STATUS_RANK[better]);
      expect(worseOf(s(worse), s(better)).state).toBe(worse);
      expect(worseOf(s(better), s(worse)).state).toBe(worse);
    }
  });

  it("lets Queued and Running win outright, in argument order", () => {
    // They are facts about the kernel, not judgements about the text.
    expect(worseOf(s("running"), s("never_run")).state).toBe("running");
    expect(worseOf(s("never_run"), s("running")).state).toBe("running");
    expect(worseOf(s("queued"), s("failed")).state).toBe("queued");
    expect(worseOf(s("running"), s("queued")).state).toBe("running");
  });

  it("returns one of its arguments, never a reconstruction", () => {
    // `Failed { exec, rc }` has to keep its `rc`: the stale banner renders it.
    const failed: HasBlockState & { rc: number } = { state: "failed", rc: 111 };
    const current: HasBlockState & { rc: number } = { state: "current", rc: 0 };
    expect(worseOf(failed, current)).toBe(failed);
  });

  it("is a total order: worseOf(a, a) is a", () => {
    for (const state of Object.keys(STATUS_RANK) as BlockStatusState[]) {
      expect(worseOf(s(state), s(state)).state).toBe(state);
    }
  });
});

describe("branded ids", () => {
  it("accepts 32 lowercase hex and nothing else", () => {
    const good = "0123456789abcdef0123456789abcdef";
    expect(isCodeHash(good)).toBe(true);
    expect(codeHash(good)).toBe(good);
    expect(isCodeHash(good.toUpperCase())).toBe(false);
    expect(isCodeHash(good.slice(1))).toBe(false);
    expect(() => codeHash("nope")).toThrow(TypeError);
  });

  it("composes the widget key ARCHITECTURE C4 specifies", () => {
    const hash = codeHash("0123456789abcdef0123456789abcdef");
    expect(clientKey(hash, 3)).toBe(`${hash}:3`);
  });
});
