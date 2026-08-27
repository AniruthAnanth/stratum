/**
 * The synthetic server is checked against the oracle before anything uses it.
 *
 * `frameServer` serves a 10 M-row frame by cycling `auto_40x12.bin`'s forty
 * observations through an SDP1 writer written here from `CONTRACTS.md` §8.1 and
 * the fixture README's four rulings. Every counter this unit asserts is measured
 * against pages that writer produced, so if it disagreed with the committed
 * bytes by one padding byte, the whole suite would be measuring a format we do
 * not ship.
 *
 * The strongest available check is byte equality with the fixture itself, and
 * that is what this asserts: re-encoding the oracle's own 480 cells reproduces
 * the file StataNow 18.5 MP's output was packed into.
 */

import { describe, expect, it } from "vitest";
import { decodeDataPage } from "../../ipc/hand";
import {
  autoDisplay,
  autoEdit,
  encodeDisplayPage,
  encodePage,
  fixtureBytes,
  frameServer,
} from "./harness";

describe("the SDP1 writer against auto_40x12.bin", () => {
  it("re-encodes the oracle byte for byte", () => {
    const columns = autoDisplay.cols.map((col) => ({
      idx: col.idx,
      cells: Array.from({ length: autoDisplay.nrows }, (_, row) =>
        col.kind === "text" ? col.cell(row) : "",
      ),
    }));
    const encoded = encodeDisplayPage({
      state: 17,
      row0: 0,
      seq: 1,
      nrows: autoDisplay.nrows,
      columns,
    });
    expect(new Uint8Array(encoded)).toEqual(new Uint8Array(fixtureBytes("auto_40x12.bin")));
  });

  it("pads the header so the payload starts 8-aligned (README §2.1)", () => {
    // A 41-row page has different digit counts in every offset, which is the case
    // the padding rule exists for: without it, whether a `num` column can be
    // viewed at all would depend on how many digits the row count happens to have.
    const encoded = encodeDisplayPage({
      state: 3,
      row0: 999_999,
      seq: 12,
      nrows: 3,
      columns: [{ idx: 0, cells: ["a", "", "ccc"] }],
    });
    const headerLen = new DataView(encoded).getUint32(4, true);
    expect((8 + headerLen) % 8).toBe(0);
    const page = decodeDataPage(encoded);
    expect(page.row0).toBe(999_999);
    const col = page.column(0);
    // The empty middle cell is two equal offsets, not an absent value.
    expect(col?.kind === "text" ? [col.cell(0), col.cell(1), col.cell(2)] : []).toEqual([
      "a",
      "",
      "ccc",
    ]);
  });
});

describe("the SDP1 writer against auto_40x12_edit.bin", () => {
  it("re-encodes the mixed page — one text column, eleven num — byte for byte", () => {
    // The edit fixture is the one that exercises the `num` branch, the 8-byte
    // alignment rule (§2.2) and the `data`-then-`aux` region order, and it is
    // mixed, so a writer that could only do one kind at a time could not
    // produce it. Byte equality here is what licenses `frameServer` to answer a
    // `render=edit` request with values the pane builds `replace` from.
    const columns = autoEdit.cols.map((col) => {
      if (col.kind === "text") {
        return {
          idx: col.idx,
          cells: Array.from({ length: autoEdit.nrows }, (_, row) => col.cell(row)),
        };
      }
      if (col.kind === "num") {
        return {
          idx: col.idx,
          values: Array.from(col.values),
          tags: Array.from(col.tags),
        };
      }
      throw new Error("auto.dta has no strL");
    });
    const encoded = encodePage({
      state: 17,
      row0: 0,
      seq: 1,
      nrows: autoEdit.nrows,
      columns,
    });
    expect(new Uint8Array(encoded)).toEqual(new Uint8Array(fixtureBytes("auto_40x12_edit.bin")));
  });
});

describe("the server", () => {
  it("serves any row of a 10 M-row frame from the oracle's forty", async () => {
    const server = frameServer({ rows: 10_000_000 });
    const url =
      "stratum-asset://localhost/frame/1/default/page?state=17&row0=9999960&nrows=40&cols=0,3&render=display&seq=1";
    const page = decodeDataPage(await (await server.fetchAsset(url)).arrayBuffer());

    expect(page.row0).toBe(9_999_960);
    expect(page.nrows).toBe(40);
    // 9 999 960 ≡ 0 (mod 40), so this window is `auto.dta` observations 1–40 and
    // the two missing `rep78` cells are where the README says they are.
    const make = page.column(0);
    const rep78 = page.column(3);
    expect(make?.kind === "text" ? make.cell(0) : "").toBe("AMC Concord");
    expect(rep78?.kind === "text" ? [rep78.cell(2), rep78.cell(6)] : []).toEqual([".", "."]);
  });

  it("rejects a held request when its signal aborts", async () => {
    const server = frameServer({ rows: 1000, mode: "manual" });
    const controller = new AbortController();
    const pending = server.fetchAsset(
      "stratum-asset://localhost/frame/1/default/page?state=17&row0=0&nrows=40&cols=0&render=display&seq=1",
      { signal: controller.signal },
    );
    controller.abort();
    await expect(pending).rejects.toThrow(/abort/i);
    expect(server.aborts()).toBe(1);
  });

  it("answers a render=edit request from the edit oracle, not the display one", async () => {
    const server = frameServer({ rows: 10_000_000 });
    const url =
      "stratum-asset://localhost/frame/1/default/page?state=17&row0=4000000&nrows=40&cols=0,1,3&render=edit&seq=2";
    const page = decodeDataPage(await (await server.fetchAsset(url)).arrayBuffer());

    const make = page.column(0);
    const price = page.column(1);
    const rep78 = page.column(3);
    expect(make?.kind).toBe("text");
    expect(price?.kind).toBe("num");
    // 4 000 000 ≡ 0 (mod 40): observation 1 of `auto.dta`, whose price IS 4099.
    // The display page says `4,099`, and `replace price = 4,099` is a syntax error.
    expect(price?.kind === "num" ? price.values[0] : 0).toBe(4099);
    expect(price?.kind === "num" ? price.tags[0] : 0).toBe(255);
    // The two missing `rep78` cells are tagged, not merely NaN.
    expect(rep78?.kind === "num" ? [rep78.tags[2], rep78.tags[6]] : []).toEqual([0, 0]);
    expect(rep78?.kind === "num" ? rep78.isMissing(2) : false).toBe(true);
  });
});
