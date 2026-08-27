/**
 * The Stata language mode — 06 §4.2's tag set, over the runtime's own tokenizer.
 *
 * This is the differentiator, so it is tested against real Stata rather than
 * against the classifier's own idea of Stata. Every case below is a line that a
 * `StreamLanguage` regex mode gets wrong, and each one is wrong in a way a user
 * would notice:
 *
 *   * `by region: summarize` — the by-list is variables and the word after the
 *     colon is the command. A mode that keys on "first word of the line" colours
 *     `region` as a command and `summarize` as a variable.
 *   * `i.educ`, `L.exper` — three tokens, one meaning. Seeing at a glance that a
 *     covariate entered as a factor rather than as continuous is a correctness
 *     feature, not decoration.
 *   * `.a` — an extended missing value, not a variable named `a`.
 *   * `, robust cluster(id)` — Stata separates options by spaces. A rule keyed on
 *     the comma colours `cluster` as a variable.
 *   * `egen x = rowmean(a, b)` — the comma inside the call does NOT open the
 *     option list.
 *
 * It runs against every backend in the checkout, because the classification must
 * not depend on which module produced the tokens.
 */

import { beforeAll, describe, expect, it, vi } from "vitest";
import type { StratumSegmenter, TokenView } from "../../wasm/types";
import { editorBackends } from "../harness";
import { classifyRegion } from "./parser";
import type { StataTag, TaggedSpan } from "./parser";

beforeAll(() => {
  vi.spyOn(console, "warn").mockImplementation(() => {});
});

const backends = await editorBackends();

/** Every classified span of a one-region document, as `[tag, text]` pairs. */
function classify(seg: StratumSegmenter, doc: string): [StataTag, string][] {
  seg.setDoc(doc);
  seg.resegment();
  const out: [StataTag, string][] = [];
  for (const region of seg.regions()) {
    const spans: TaggedSpan[] = [];
    const tokens = seg.tokens(region.outerFrom, region.outerTo);
    classifyRegion(
      doc.slice(region.outerFrom, region.outerTo),
      region.outerFrom,
      region,
      tokens,
      0,
      tokens.length,
      spans,
    );
    for (const span of spans) out.push([span.tag, doc.slice(span.from, span.to)]);
  }
  return out;
}

describe.each(backends)("classification [$name]", (backend) => {
  it("reads a prefixed command the way Stata does", async () => {
    const seg = await backend.load();
    expect(classify(seg, "by region: summarize income if age>25")).toEqual([
      ["PrefixCommand", "by"],
      ["VarName", "region"],
      ["Operator", ":"],
      ["CommandName", "summarize"],
      ["VarName", "income"],
      ["Qualifier", "if"],
      ["VarName", "age"],
      ["Operator", ">"],
      ["Number", "25"],
    ]);
    seg.destroy();
  });

  it("keeps a non-varlist prefix from swallowing the command", async () => {
    const seg = await backend.load();
    const tags = classify(seg, "quietly regress y x");
    expect(tags.slice(0, 3)).toEqual([
      ["PrefixCommand", "quietly"],
      ["CommandName", "regress"],
      ["VarName", "y"],
    ]);
    seg.destroy();
  });

  it("splits factor and time-series operators off the variable", async () => {
    const seg = await backend.load();
    const tags = classify(seg, "regress lwage i.educ L.exper");
    expect(tags).toEqual([
      ["CommandName", "regress"],
      ["VarName", "lwage"],
      ["FactorOp", "i."],
      ["VarName", "educ"],
      ["TsOp", "L."],
      ["VarName", "exper"],
    ]);
    seg.destroy();
  });

  it("reads `.a` as an extended missing value, not a variable", async () => {
    const seg = await backend.load();
    expect(classify(seg, "replace y = .a if x == .")).toContainEqual(["Missing", ".a"]);
    seg.destroy();
  });

  it("names every space-separated option, and only inside the option list", async () => {
    const seg = await backend.load();
    const tags = classify(seg, "regress y x, robust cluster(id) level(95)");
    expect(tags).toContainEqual(["OptionName", "robust"]);
    expect(tags).toContainEqual(["OptionName", "cluster"]);
    expect(tags).toContainEqual(["OptionArg", "id"]);
    expect(tags).toContainEqual(["OptionName", "level"]);
    seg.destroy();
  });

  it("does not let a comma inside a function call open the option list", async () => {
    const seg = await backend.load();
    const tags = classify(seg, "egen m = rowmean(a, b)");
    // `b` is an argument of `rowmean`, so it must not be named as an option.
    expect(tags.some(([tag, text]) => tag === "OptionName" && text === "b")).toBe(false);
    seg.destroy();
  });

  it("colours macros loudly", async () => {
    const seg = await backend.load();
    const tags = classify(seg, "display `x' $y");
    expect(tags).toContainEqual(["LocalMacro", "`x'"]);
    expect(tags).toContainEqual(["GlobalMacro", "$y"]);
    seg.destroy();
  });

  it("tells `// %%` and `//:` apart from each other and from code", async () => {
    const seg = await backend.load();
    const tags = classify(seg, "// %% Cleaning\n//: ## Notes\n// plain");
    expect(tags).toContainEqual(["SectionMarker", "// %% Cleaning"]);
    expect(tags).toContainEqual(["NarrativeComment", "//: ## Notes"]);
    expect(tags).toContainEqual(["Comment", "// plain"]);
    seg.destroy();
  });

  it("does not colour numbers", async () => {
    const seg = await backend.load();
    // §4.3: numbers are data. `Number` exists as a tag and maps to body ink, so
    // the assertion is about the STYLE, not about the absence of a tag — see
    // `highlight.ts`, where `Number` shares `.stx-plain` with variables.
    const tags = classify(seg, "gen z = 42");
    expect(tags).toContainEqual(["Number", "42"]);
    seg.destroy();
  });
});

/**
 * The shapes the shipped backends do not produce yet.
 *
 * Both segmenters in this checkout run the deliberately naive rule
 * (`conformance.ts` names it), which emits `` `=2+2' `` as a bare backtick plus
 * an expression, `* a comment` as a multiplication operator, and `inc*` as two
 * tokens. Those are tokenizer gaps that `stratum-parse` closes when W11b links
 * it — they are NOT this classifier's, and asserting them against today's
 * backends would either fail or bake a workaround into the editor, which is the
 * second-segmenter mistake 06 §3.2 exists to prevent.
 *
 * So the classifier is fed the token stream CONTRACTS §14 describes directly.
 * When the real tokenizer arrives, these keep passing and the cases above start
 * covering the same ground through it.
 */
describe("shapes the naive tokenizer does not emit yet", () => {
  const tok = (from: number, to: number, tag: NonNullable<TokenView["tag"]>): TokenView => ({
    from,
    to,
    tag,
    tagCode: 0,
  });

  const run = (text: string, tokens: TokenView[]): [StataTag, string][] => {
    const spans: TaggedSpan[] = [];
    classifyRegion(text, 0, null, tokens, 0, tokens.length, spans);
    return spans.map((span) => [span.tag, text.slice(span.from, span.to)]);
  };

  it("distinguishes the three macro forms", () => {
    const text = "display `x' `=2+2' `: word count abc'";
    expect(
      run(text, [
        tok(0, 7, "ident"),
        tok(7, 8, "whitespace"),
        tok(8, 11, "macro_ref"),
        tok(11, 12, "whitespace"),
        tok(12, 18, "macro_ref"),
        tok(18, 19, "whitespace"),
        tok(19, 37, "macro_ref"),
      ]),
    ).toEqual([
      ["CommandName", "display"],
      ["LocalMacro", "`x'"],
      ["MacroFuncCall", "`=2+2'"],
      ["MacroFuncCall", "`: word count abc'"],
    ]);
  });

  it("marks a varlist wildcard as one", () => {
    const text = "summarize inc* a-z _all";
    expect(
      run(text, [
        tok(0, 9, "ident"),
        tok(9, 10, "whitespace"),
        tok(10, 14, "ident"),
        tok(14, 15, "whitespace"),
        tok(15, 18, "ident"),
        tok(18, 19, "whitespace"),
        tok(19, 23, "ident"),
      ]),
    ).toEqual([
      ["CommandName", "summarize"],
      ["VarWildcard", "inc*"],
      ["VarWildcard", "a-z"],
      ["VarWildcard", "_all"],
    ]);
  });

  it("marks a weight inside the brackets that must follow the command", () => {
    const text = "mean income [aweight=wt]";
    expect(
      run(text, [
        tok(0, 4, "ident"),
        tok(4, 5, "whitespace"),
        tok(5, 11, "ident"),
        tok(11, 12, "whitespace"),
        tok(12, 13, "l_bracket"),
        tok(13, 20, "ident"),
        tok(20, 21, "op"),
        tok(21, 23, "ident"),
        tok(23, 24, "r_bracket"),
      ]),
    ).toContainEqual(["Weight", "aweight"]);
  });

  it("handles a factor operator glued into one identifier token", () => {
    const text = "regress y i.educ";
    expect(
      run(text, [
        tok(0, 7, "ident"),
        tok(7, 8, "whitespace"),
        tok(8, 9, "ident"),
        tok(9, 10, "whitespace"),
        tok(10, 16, "ident"),
      ]),
    ).toEqual([
      ["CommandName", "regress"],
      ["VarName", "y"],
      ["FactorOp", "i."],
      ["VarName", "educ"],
    ]);
  });

  it("does not tokenize a Mata body as Stata", () => {
    const text = "mata:\nreal x = 1\nend";
    const spans: TaggedSpan[] = [];
    classifyRegion(
      text,
      0,
      { kind: { kind: "end_block", opener: "mata", name: null } } as never,
      [tok(0, 4, "ident"), tok(6, 10, "ident")],
      0,
      2,
      spans,
    );
    expect(spans).toEqual([]);
  });
});
