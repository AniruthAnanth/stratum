/**
 * The renderers against real bytes and the real golden.
 *
 * Two claims are checked here that no synthetic fixture can check:
 *
 *  1. **The structural views in `types.ts` describe what serde actually emits.**
 *     §12 forbids hand-writing a mirror of a Rust type, and `src/ipc/types.ts` —
 *     the generated file the views will be replaced by — does not exist until
 *     W17 wires the host. Until then the honest substitute is not "read the Rust
 *     carefully"; it is to decode W07's committed MessagePack, which came out of
 *     `rmp_serde::to_vec_named` over the frozen structs, and read the fields off
 *     it. A view that got a name, a nesting or a null convention wrong fails
 *     below.
 *
 *  2. **The classic text is StataMP 18.5's, byte for byte.** The raw renderer
 *     must not touch it, so the assertion is against
 *     `tests/golden/stata18/core_surface.log` itself rather than against a copy.
 */

import { describe, expect, test } from "vitest";
import { goldenLog, scenarioAEnvelopes, scenarioAEvents } from "./fixtures";
import type { EstimationPayloadView, ResultEnvelopeView, SummarizePayloadView } from "./types";

const events = scenarioAEvents();
const envelopes = scenarioAEnvelopes();
const golden = goldenLog();

function payload<T>(envelope: ResultEnvelopeView, kind: string): T {
  const found = envelope.payloads.find((p) => p.kind === kind);
  if (found === undefined) throw new Error(`no ${kind} payload`);
  return found as unknown as T;
}

describe("the committed mock stream decodes as §10 frames", () => {
  test("27 events, three of them results, in run order", () => {
    expect(events).toHaveLength(27);
    expect(events.filter((e) => e["event"] === "result")).toHaveLength(3);
    expect(envelopes.map((e) => e.cmdline)).toEqual([
      "sysuse auto, clear",
      "summarize price mpg",
      "regress price mpg weight foreign",
    ]);
  });

  test("`seq` is strictly increasing by one across the whole stream (§7)", () => {
    const seqs = events.map((e) => Number(e["seq"]));
    expect(seqs).toEqual(seqs.map((_, i) => i + 1));
  });
});

describe("the wire shape the structural views claim", () => {
  test("a payload's fields sit BESIDE `kind`, not nested under a second key", () => {
    const p = envelopes[1]?.payloads[0] as unknown as Record<string, unknown>;
    expect(p["kind"]).toBe("summarize");
    expect(Object.keys(p)).toEqual(["kind", "detail", "weight", "qualifier", "rows"]);
    expect(p["summarize"]).toBeUndefined();
  });

  test("`CardAction` is `{action: <snake_case tag>}` with its payload beside it", () => {
    expect(envelopes[2]?.actions).toEqual([
      { action: "copy_table" },
      { action: "plot_coefficients" },
      { action: "raw_output" },
    ]);
  });

  test("`Option<T>` is null, not an absent key", () => {
    const p = payload<SummarizePayloadView>(envelopes[1] as ResultEnvelopeView, "summarize");
    expect(p.weight).toBeNull();
    expect(p.qualifier).toBeNull();
    expect(p.rows[0]?.detail).toBeNull();
    expect(p.rows[0]?.label).toBe("Price");
  });

  test("every field a renderer reads is present on the envelope", () => {
    for (const envelope of envelopes) {
      expect(typeof envelope.cmdline).toBe("string");
      expect(typeof envelope.duration_us).toBe("number");
      expect(typeof envelope.rc).toBe("number");
      expect(typeof envelope.raw.head).toBe("string");
      expect(typeof envelope.layout_hint.est_px).toBe("number");
      expect(Array.isArray(envelope.actions)).toBe(true);
      expect(Array.isArray(envelope.payloads)).toBe(true);
    }
  });

  test("`sample_hash` really is a u64 that a JS number cannot hold", () => {
    const p = payload<EstimationPayloadView>(envelopes[2] as ResultEnvelopeView, "estimation");
    // 0x5354415441313835. Read as a double it lands on a different integer, and
    // two distinct e(sample) bitmaps could then compare equal — which is exactly
    // the silent cross-sample comparison §19 exists to prevent.
    expect(typeof p.sample_hash).toBe("bigint");
    expect(String(p.sample_hash)).toBe("6004496033318516789");
    expect(BigInt(String(p.sample_hash)) > BigInt(Number.MAX_SAFE_INTEGER)).toBe(true);
    // Round-tripping through a double changes the value. That is the bug the
    // opaque-key decision avoids, demonstrated rather than asserted from memory.
    expect(BigInt(Number(p.sample_hash))).not.toBe(BigInt(String(p.sample_hash)));
  });

  test("REPORTED: `code_hash` arrives as 16 bytes, not as §12's 32 hex chars", () => {
    // CONTRACTS §12 declares `CodeHash = string & {…}` with a 32-lowercase-hex
    // invariant that `ipc/hand.ts:codeHash()` throws on violation of, while
    // `ids.rs` derives Serialize on `CodeHash(pub [u8; 16])` — which puts an
    // array of 16 integers on the wire. No renderer reads the field, so nothing
    // here is blocked; it is asserted so the inconsistency is recorded against
    // real bytes rather than argued from two documents.
    const raw = envelopes[0] as unknown as Record<string, unknown>;
    expect(Array.isArray(raw["code_hash"])).toBe(true);
    expect(raw["code_hash"]).toHaveLength(16);
  });
});

describe("the classic text is StataMP 18.5's, byte for byte", () => {
  test("the regress block appears in core_surface.log CONTIGUOUSLY", () => {
    const head = envelopes[2]?.raw.head ?? "";
    expect(head.length).toBeGreaterThan(0);
    expect(golden).toContain(head);
  });

  test("every summarize line appears in core_surface.log verbatim", () => {
    const lines = golden.split("\n");
    for (const line of (envelopes[1]?.raw.head ?? "").split("\n").slice(1)) {
      if (line.length === 0) continue;
      expect(lines).toContain(line);
    }
  });

  /**
   * TRIPWIRE — a defect in `apps/desktop/src-tauri/src/mock_engine.rs`, which is
   * W07's file and not this unit's to edit.
   *
   * `SUMMARIZE_CLASSIC` and `REGRESS_CLASSIC` are written as `"\` + newline
   * string literals. Rust's line continuation swallows the newline **and the
   * leading whitespace of the next line**, so the FIRST line of each block loses
   * its indentation: the golden has `    Variable |` and `      Source |`, and
   * the fixture carries `Variable |` and `Source |`. Every other line is
   * byte-identical.
   *
   * W07's own guard did not catch it because it uses `log.contains(line)` — a
   * substring test, which a de-indented line passes. This asserts the exact
   * deviation instead, so it is a recorded fact rather than an unnoticed one.
   *
   * **When W07 restores the indentation, this test fails.** That is deliberate:
   * delete it, and drop the `.slice(1)` in the two tests above.
   */
  test("TRIPWIRE: exactly one line per block is not byte-identical (W07 defect)", () => {
    const lines = golden.split("\n");
    const deviations: [string, string][] = [];
    for (const index of [1, 2]) {
      for (const line of (envelopes[index]?.raw.head ?? "").split("\n")) {
        if (line.length === 0 || lines.includes(line)) continue;
        const match = lines.find((l) => l.trimStart() === line.trimStart());
        expect(match, `not in the golden at all: ${line}`).toBeDefined();
        deviations.push([line, match ?? ""]);
      }
    }
    expect(deviations.map(([mock]) => mock)).toEqual([
      "Variable |        Obs        Mean    Std. dev.       Min        Max",
      "Source |       SS           df       MS      Number of obs   =        74",
    ]);
    // The only difference is leading whitespace the Rust literal ate.
    for (const [mock, gold] of deviations) {
      expect(gold.trimStart()).toBe(mock);
      expect(gold.length).toBeGreaterThan(mock.length);
    }
  });

  test("the `sysuse` note is the golden's own", () => {
    expect(golden).toContain((envelopes[0]?.raw.head ?? "").trimEnd());
  });
});

describe("the card prints the SAME strings the classic text does (A6)", () => {
  test("summarize: every display string is in that variable's golden row", () => {
    const p = payload<SummarizePayloadView>(envelopes[1] as ResultEnvelopeView, "summarize");
    const rows = (envelopes[1]?.raw.head ?? "").split("\n");
    for (const row of p.rows) {
      const line = rows.find((l) => l.trimStart().startsWith(`${row.var} |`));
      expect(line, `no classic row for ${row.var}`).toBeDefined();
      for (const value of Object.values(row.display)) {
        expect(line, `${row.var}: ${value} is not in the classic row`).toContain(value);
      }
    }
  });

  test("regress: every `display_num` cell is in that term's golden row", () => {
    const p = payload<EstimationPayloadView>(envelopes[2] as ResultEnvelopeView, "estimation");
    const rows = (envelopes[2]?.raw.head ?? "").split("\n");
    expect(p.terms).toHaveLength(4);
    for (const term of p.terms) {
      const line = rows.find((l) => l.trimStart().startsWith(`${term.display} |`));
      expect(line, `no classic row for ${term.display}`).toBeDefined();
      for (const cell of term.display_num) {
        expect(line, `${term.display}: ${cell} is not in the classic row`).toContain(cell);
      }
    }
  });

  test("regress: every `AnovaTable.display` cell is in the golden ANOVA block", () => {
    const p = payload<EstimationPayloadView>(envelopes[2] as ResultEnvelopeView, "estimation");
    const head = envelopes[2]?.raw.head ?? "";
    for (const cell of p.anova?.display ?? []) expect(head).toContain(cell);
  });
});

describe("the mock proves A22's negative case", () => {
  test("a build without `margins` sends no RunMargins on a regress envelope", () => {
    const tags = (envelopes[2]?.actions ?? []).map((a) => a.action);
    expect(tags).not.toContain("run_margins");
    expect(tags).not.toContain("compare_model");
    expect(tags.at(-1)).toBe("raw_output");
  });

  test("`e()` scalars arrive with no display string — the escalated gap", () => {
    const p = payload<EstimationPayloadView>(envelopes[2] as ResultEnvelopeView, "estimation");
    // Nine scalars, every one a bare f64. `F` is 23.2899 and the classic text
    // prints `23.29`; there is nothing on the wire that says so, which is why
    // the model strip omits it rather than calling `toFixed(2)`.
    expect(p.scalars).toHaveLength(9);
    for (const entry of p.scalars) {
      expect(entry).toHaveLength(2);
      expect(typeof entry[1]).toBe("number");
    }
    expect(p.scalars.find(([k]) => k === "F")?.[1]).toBe(23.2899);
    expect(envelopes[2]?.payloads.some((x) => x.kind === "scalars")).toBe(false);
  });
});
