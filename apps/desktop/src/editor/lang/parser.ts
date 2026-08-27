/**
 * Stata token classification — 06 §4.2's tag set, over the runtime's own
 * tokenizer.
 *
 * # Why there is no Lezer tree here
 *
 * 06 §4.2 specifies a CM6 `Parser` over the wasm tokenizer, feeding
 * `syntaxHighlighting`. Both of those live in `@codemirror/language`, and this
 * unit owns no manifest: `apps/desktop/package.json` and `pnpm-lock.yaml` are
 * W12's, and R0 forbids reaching across for them. See the unit's return for the
 * escalation.
 *
 * The workaround is not a downgrade, and this is worth being precise about
 * rather than defensive. A Lezer tree here would exist solely to be walked back
 * out into styled spans: we have no error recovery to gain (our tokenizer IS the
 * runtime tokenizer, §4.2 says so), no incremental reuse to gain (the wasm side
 * already re-tokenizes only the byte range we ask for), and no grammar to gain.
 * What it would add is a `Tree.build` per viewport and an LRU keyed by
 * `code_hash`. Classifying the token stream straight into `Decoration.mark`
 * ranges skips the tree and is what `highlight.ts` does. If the packages land,
 * the exchange is `highlight.ts`'s decoration builder for a `HighlightStyle`;
 * nothing in this file changes.
 *
 * # What this file actually does
 *
 * The wasm tokenizer is *lexical*: it says `ident`, not `CommandName`. The
 * §4.2 tag set is *positional* — the same word is a command in one column and a
 * variable in the next — so the classification is a small state machine run per
 * region, seeded by the region's own kind. That is the whole differentiator:
 * a `StreamLanguage` cannot do it and neither can a regex mode.
 */

import type { RegionKind } from "../../wasm/types";
import type { TokenView } from "../../wasm/types";
import type { Block } from "../blocks/segmenter";

/** 06 §4.2's tag set, verbatim. The highlight style keys off these. */
export type StataTag =
  | "CommandName"
  | "PrefixCommand"
  | "Subcommand"
  | "OptionName"
  | "OptionArg"
  | "VarName"
  | "VarWildcard"
  | "FactorOp"
  | "TsOp"
  | "LocalMacro"
  | "GlobalMacro"
  | "MacroFuncCall"
  | "String"
  | "CompoundString"
  | "Number"
  | "Missing"
  | "Comment"
  | "NarrativeComment"
  | "SectionMarker"
  | "Continuation"
  | "DelimitDirective"
  | "Qualifier"
  | "Weight"
  | "Format"
  | "Operator"
  | "Brace"
  | "MataRegion"
  | "Error";

/** One classified span. Offsets are UTF-16 code units, like everything here. */
export interface TaggedSpan {
  from: number;
  to: number;
  tag: StataTag;
}

/**
 * Prefixes that take a `:` and then another command.
 *
 * Abbreviations are included because Stata accepts them and a user typing `qui:`
 * must not see it highlighted as a variable. The list is closed on purpose: an
 * open "anything before a colon is a prefix" rule mis-fires on `mata:` blocks
 * and on `label define x 1 "a":`-shaped mistakes.
 */
const PREFIX_COMMANDS = new Set([
  "by",
  "bys",
  "bysort",
  "capture",
  "cap",
  "quietly",
  "qui",
  "quie",
  "noisily",
  "noi",
  "nois",
  "svy",
  "xi",
  "bootstrap",
  "bs",
  "jackknife",
  "statsby",
  "rolling",
  "simulate",
  "permute",
  "nestreg",
  "stepwise",
  "sw",
  "mi",
  "fp",
  "version",
]);

/**
 * The prefixes that take a VARLIST before their colon.
 *
 * `by region: summarize` and `capture regress` are both prefixed commands and
 * they parse differently: the word after `by` is a variable, the word after
 * `capture` is the command. Getting this wrong colours the by-list as a command
 * name, which is the most visible possible mistake in the most common possible
 * line.
 */
const VARLIST_PREFIXES = new Set(["by", "bys", "bysort", "statsby", "rolling", "svy"]);

/** `i` `c` `o` `b` `bn` `b2` — a factor operator when glued to `.` and a name. */
const FACTOR_TOKEN = /^(?:[icoICO]|[bB]n?\d*|[oO]\d*)$/;
/** `L` `L2` `D` `F` `S` — a time-series operator under the same glue rule. */
const TS_TOKEN = /^[LDFSldfs]\d*$/;
/** The letter of an extended missing value, `.a` … `.z`. */
const MISSING_LETTER = /^[a-z]$/;

/**
 * Commands whose SECOND word is a subcommand rather than a variable.
 *
 * Restricted to the ones where the distinction is visible and common. A wrong
 * guess here mis-colours one word; leaving a command out costs nothing, so the
 * list stays short rather than aspiring to completeness it cannot verify.
 */
const SUBCOMMAND_HOSTS = new Set([
  "graph",
  "estimates",
  "est",
  "matrix",
  "mat",
  "label",
  "file",
  "ssc",
  "net",
  "webuse",
  "sysuse",
  "putexcel",
  "frame",
  "frames",
  "mi",
  "svyset",
  "snapshot",
  "postfile",
  "return",
  "ereturn",
  "sreturn",
  "cluster",
  "ml",
  "sem",
  "gsem",
  "margins",
  "marginsplot",
  "twoway",
  "serset",
  "window",
  "translate",
  "log",
  "cmdlog",
]);

/** `if` and `in`, the two restriction qualifiers (§4.2 "Qualifier"). */
const QUALIFIERS = new Set(["if", "in"]);

/** Weight kinds, recognised inside the `[ ]` that must follow a command. */
const WEIGHTS = new Set([
  "weight",
  "aweight",
  "fweight",
  "pweight",
  "iweight",
  "aw",
  "fw",
  "pw",
  "iw",
]);

/** `i.` `c.` `o.` `b.` and the `b#.`/`bn.` family. */
const FACTOR_PREFIX = /^(?:[icoIC]|[bB]n?\d*|[oO]\d*)\.(?=\S)/;
/** `L.` `L2.` `L(1/4).` `D.` `F.` `S.` — the time-series operators. */
const TS_PREFIX = /^(?:[LDFSldfs]\d*)\.(?=\S)/;
/** `%9.2f`, `%-10s`, `%tdD_m_Y`, `%21x`. */
const FORMAT = /^%-?\d*\.?\d*[a-zA-Z][a-zA-Z_0-9]*$/;
/** A varlist wildcard: `inc*`, `q?`, `a-z`, `_all`. */
const WILDCARD = /[*?]|^_all$|^[A-Za-z_][A-Za-z_0-9]*-[A-Za-z_][A-Za-z_0-9]*$/;
/** System missing and the extended missings `.a` … `.z`. */
const MISSING = /^\.[a-z]?$/;

/** `//:` narrative, `// %%` section, `///` continuation, or an ordinary comment. */
function commentTag(text: string): StataTag {
  const trimmed = text.trimStart();
  if (trimmed.startsWith("///")) return "Continuation";
  if (trimmed.startsWith("//:") || trimmed.startsWith("*:")) return "NarrativeComment";
  // `// %%`, `//%%`, `* %%` — all three of 06 §4.8's markers.
  if (/^(?:\/\/|\*)\s*%%/.test(trimmed)) return "SectionMarker";
  return "Comment";
}

/** Foreign-language bodies we deliberately do not tokenize (§4.2 `MataRegion`). */
function isForeignBody(kind: RegionKind | null): boolean {
  return (
    kind !== null &&
    kind.kind === "end_block" &&
    (kind.opener === "mata" || kind.opener === "python" || kind.opener === "java")
  );
}

/**
 * Where the machine is inside one region.
 *
 * `head` covers both prefix and command position, because in `by grp: summarize`
 * the machine has to still be in head when it reaches the colon — a separate
 * `command` phase would have to be re-entered from the prefix arm and would then
 * be indistinguishable from `head`.
 */
type Phase = "head" | "args" | "options";

/**
 * Classify the tokens of one region.
 *
 * `text` is a slice of the document starting at `base`, and `tokens[start..end)`
 * are that region's tokens in ascending order. Both the slice window and the
 * index window exist for the same reason: this runs per viewport per keystroke,
 * and neither a whole-document `sliceString` nor a per-region `Array.slice` may
 * appear on that path. Spans are emitted into a caller-supplied array for the
 * same reason.
 */
export function classifyRegion(
  text: string,
  base: number,
  block: Block | null,
  tokens: readonly TokenView[],
  start: number,
  end: number,
  out: TaggedSpan[],
): void {
  if (block !== null && isForeignBody(block.kind)) {
    // One span for the whole body. Highlighting Mata as if it were Stata is
    // worse than not highlighting it: it asserts a syntax that is not there.
    for (let i = start; i < end; i++) {
      const token = tokens[i];
      if (token?.tag === "comment") out.push({ from: token.from, to: token.to, tag: "Comment" });
    }
    return;
  }

  let phase: Phase = "head";
  let depth = 0;
  let commandName = "";
  let sawSubcommand = false;
  let inBracket = false;
  /** Inside a `by`-style prefix's varlist: idents are variables until the colon. */
  let inVarlistPrefix = false;

  for (let i = start; i < end; i++) {
    const token = tokens[i];
    if (token === undefined) break;
    const slice = text.slice(token.from - base, token.to - base);

    switch (token.tag) {
      case "whitespace":
        continue;
      case "comment":
        out.push({ from: token.from, to: token.to, tag: commentTag(slice) });
        continue;
      case "continuation":
        out.push({ from: token.from, to: token.to, tag: "Continuation" });
        continue;
      case "directive":
        out.push({ from: token.from, to: token.to, tag: "DelimitDirective" });
        continue;
      case "statement_break":
        phase = "head";
        depth = 0;
        commandName = "";
        sawSubcommand = false;
        inVarlistPrefix = false;
        continue;
      case "str_lit":
        out.push({ from: token.from, to: token.to, tag: "String" });
        continue;
      case "compound_quote":
        out.push({ from: token.from, to: token.to, tag: "CompoundString" });
        continue;
      case "macro_ref":
        out.push({ from: token.from, to: token.to, tag: macroTag(slice) });
        if (phase === "head" && !inVarlistPrefix) phase = "args";
        continue;
      case "number":
        out.push({
          from: token.from,
          to: token.to,
          tag: MISSING.test(slice) ? "Missing" : "Number",
        });
        continue;
      case "l_paren":
      case "l_bracket":
        depth += 1;
        if (token.tag === "l_bracket" && depth === 1) inBracket = true;
        out.push({ from: token.from, to: token.to, tag: "Operator" });
        continue;
      case "r_paren":
      case "r_bracket":
        depth = Math.max(0, depth - 1);
        if (token.tag === "r_bracket" && depth === 0) inBracket = false;
        out.push({ from: token.from, to: token.to, tag: "Operator" });
        continue;
      case "l_brace":
      case "r_brace":
        out.push({ from: token.from, to: token.to, tag: "Brace" });
        continue;
      case "comma":
        // Only a TOP-LEVEL comma opens the option list. A comma inside
        // `egen x = rowmean(a, b)` does not, and treating it as one is how
        // every naive Stata mode colours half an expression as options.
        if (depth === 0) phase = "options";
        out.push({ from: token.from, to: token.to, tag: "Operator" });
        continue;
      case "colon":
        // The prefix's colon. Everything resets: the word after it is a command.
        if (depth === 0 && (inVarlistPrefix || PREFIX_COMMANDS.has(commandName))) {
          phase = "head";
          commandName = "";
          inVarlistPrefix = false;
        }
        out.push({ from: token.from, to: token.to, tag: "Operator" });
        continue;
      case "op": {
        // `.a` … `.z` arrive as two tokens — an operator and a one-letter name —
        // and are a MISSING VALUE, not a variable called `a`. The tell is that
        // nothing is glued to the left of the dot; `i.educ` has an `i` there and
        // is handled in the identifier arm.
        const after = tokens[i + 1];
        if (
          slice === "." &&
          after !== undefined &&
          after.tag === "ident" &&
          after.from === token.to &&
          MISSING_LETTER.test(text.slice(after.from - base, after.to - base))
        ) {
          out.push({ from: token.from, to: after.to, tag: "Missing" });
          i += 1;
          continue;
        }
        out.push({
          from: token.from,
          to: token.to,
          tag: MISSING.test(slice) ? "Missing" : "Operator",
        });
        continue;
      }
      case "unknown":
        out.push({ from: token.from, to: token.to, tag: "Error" });
        continue;
      default:
        break;
    }

    if (token.tag !== "ident") continue;

    // --- identifiers: the part that actually needs the machine ---------------
    if (phase === "head") {
      if (inVarlistPrefix) {
        // The by-list. `by region area: summarize` has two variables in it.
        pushVarLike(slice, token, out);
        continue;
      }
      commandName = slice.toLowerCase();
      const isPrefix = PREFIX_COMMANDS.has(commandName);
      out.push({ from: token.from, to: token.to, tag: isPrefix ? "PrefixCommand" : "CommandName" });
      if (!isPrefix) {
        phase = "args";
      } else if (VARLIST_PREFIXES.has(commandName)) {
        inVarlistPrefix = true;
      }
      // A non-varlist prefix (`capture`, `quietly`) stays in `head`, so the very
      // next word is the command whether or not a colon separates them.
      continue;
    }

    if (phase === "args") {
      if (QUALIFIERS.has(slice.toLowerCase()) && depth === 0) {
        out.push({ from: token.from, to: token.to, tag: "Qualifier" });
        continue;
      }
      if (inBracket && WEIGHTS.has(slice.toLowerCase())) {
        out.push({ from: token.from, to: token.to, tag: "Weight" });
        continue;
      }
      if (!sawSubcommand && SUBCOMMAND_HOSTS.has(commandName)) {
        sawSubcommand = true;
        out.push({ from: token.from, to: token.to, tag: "Subcommand" });
        continue;
      }
      i = pushOperatorPrefixed(text, base, tokens, i, slice, token, out);
      continue;
    }

    // phase === "options". Stata separates options by SPACES, not commas, so
    // every top-level identifier here opens one; anything inside its parentheses
    // is its argument. A rule keyed on "first word after the comma" colours
    // `robust cluster(id)` as one option and one variable, which is wrong twice.
    if (depth === 0) {
      out.push({ from: token.from, to: token.to, tag: "OptionName" });
      continue;
    }
    if (FORMAT.test(slice)) {
      out.push({ from: token.from, to: token.to, tag: "Format" });
      continue;
    }
    out.push({ from: token.from, to: token.to, tag: "OptionArg" });
  }
}

/**
 * `i.educ`, `L.exper` — an identifier glued to a `.` and another identifier.
 *
 * The tokenizer emits three tokens because lexically that is what they are; the
 * meaning is positional, which is exactly the kind of thing this file exists to
 * recover. Returns the index the caller's loop should continue from.
 */
function pushOperatorPrefixed(
  text: string,
  base: number,
  tokens: readonly TokenView[],
  i: number,
  slice: string,
  token: TokenView,
  out: TaggedSpan[],
): number {
  const dot = tokens[i + 1];
  const name = tokens[i + 2];
  const glued =
    dot !== undefined &&
    name !== undefined &&
    dot.tag === "op" &&
    dot.from === token.to &&
    name.tag === "ident" &&
    name.from === dot.to &&
    text.slice(dot.from - base, dot.to - base) === ".";

  if (glued) {
    const isFactor = FACTOR_TOKEN.test(slice);
    if (isFactor || TS_TOKEN.test(slice)) {
      out.push({ from: token.from, to: dot.to, tag: isFactor ? "FactorOp" : "TsOp" });
      pushVarLike(text.slice(name.from - base, name.to - base), name, out);
      return i + 2;
    }
  }
  pushVarLike(slice, token, out);
  return i;
}

/**
 * A variable reference, splitting off any factor or time-series prefix.
 *
 * `i.region` is ONE token from the tokenizer and TWO spans on screen, because
 * §4.3 colours the operator loudly and the variable in body ink — the whole
 * point being that you can see at a glance that `region` entered the model as a
 * factor rather than as a continuous covariate.
 */
function pushVarLike(slice: string, token: TokenView, out: TaggedSpan[]): void {
  // A tokenizer that emits `i.educ` as ONE identifier is also legal under
  // CONTRACTS §14 — the tag set says nothing about where an identifier ends — so
  // both shapes are handled. This is the glued one.
  const factor = FACTOR_PREFIX.exec(slice);
  if (factor !== null) {
    const cut = token.from + factor[0].length;
    out.push({ from: token.from, to: cut, tag: "FactorOp" });
    pushVarLike(slice.slice(factor[0].length), { ...token, from: cut }, out);
    return;
  }
  const ts = TS_PREFIX.exec(slice);
  if (ts !== null) {
    const cut = token.from + ts[0].length;
    out.push({ from: token.from, to: cut, tag: "TsOp" });
    pushVarLike(slice.slice(ts[0].length), { ...token, from: cut }, out);
    return;
  }
  if (FORMAT.test(slice)) {
    out.push({ from: token.from, to: token.to, tag: "Format" });
    return;
  }
  out.push({
    from: token.from,
    to: token.to,
    tag: WILDCARD.test(slice) ? "VarWildcard" : "VarName",
  });
}

/**
 * Which of the three macro forms this is.
 *
 * `` `=exp' `` and `` `: subcmd' `` are macro FUNCTION calls — they run code at
 * expansion time — and colouring them the same as a plain `` `x' `` hides the
 * single most common source of surprising Stata behaviour.
 */
function macroTag(slice: string): StataTag {
  if (slice.startsWith("`=") || slice.startsWith("`:")) return "MacroFuncCall";
  if (slice.startsWith("$") || slice.startsWith("${")) return "GlobalMacro";
  return "LocalMacro";
}
