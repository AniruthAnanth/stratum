/**
 * The deliberately naive line splitter behind the development stub.
 *
 * **This is not a segmenter.** It is a placeholder shaped like one, so the
 * editor can be built, laid out and tested in wave 1 while `stratum-parse`
 * (W04) and the real bindings (W11b) are still being written. It gets nested
 * `///`, `#delimit ;` interacting with `/* *​/`, compound quotes and Mata inside
 * `program define` **wrong**, on purpose and without apology — the moment it
 * looked almost right, someone would ship it.
 *
 * What it does get right is the *shape*: byte offsets, tiling outer spans,
 * stable hashes, and the flat row layout `types.ts` declares. That is what makes
 * it substitutable for the real module.
 *
 * It works on UTF-8 bytes, not on a JS string, for the same reason the Rust side
 * does: every structural character in Stata is ASCII and UTF-8 is
 * self-synchronising, so a byte scanner needs no character decoding and cannot
 * disagree with the real module about where a byte offset is.
 */

import {
  DELIM_CR,
  DELIM_SEMI,
  FLAG_ESTIMATION,
  FLAG_EXECUTABLE,
  FLAG_EXIT_SEMI,
  FLAG_MACRO_IN_HEAD,
  FLAG_SECTION_HEAD,
  NARRATIVE_BLOCK,
  NARRATIVE_LINE,
  TOKEN_TAGS,
  encodeKind,
} from "../types.ts";
import type { Diagnostic, RegionKind } from "../types.ts";

// --- byte constants ---------------------------------------------------------

const LF = 10;
const CR = 13;
const TAB = 9;
const SPACE = 32;
const SLASH = 47;
const STAR = 42;
const LBRACE = 123;
const RBRACE = 125;
const SEMI = 59;
const PIPE = 124;
const PERCENT = 37;
const DQUOTE = 34;
const BACKTICK = 96;
const DOLLAR = 36;
const UNDERSCORE = 95;
const HASH = 35;
const DOT = 46;
const ZERO = 48;
const NINE = 57;
const UPPER_A = 65;
const UPPER_Z = 90;
const LOWER_A = 97;
const LOWER_Z = 122;
const LOWER_D = 100;
const LOWER_M = 109;

/** Commands that open a `… end` block. */
const END_BLOCK_WORDS: Record<string, number> = {
  program: 0,
  input: 1,
  mata: 2,
  python: 3,
  java: 4,
};

/** Commands that habitually open a brace block, mapped to `BraceOpener`. */
const BRACE_WORDS: Record<string, number> = {
  foreach: 0,
  forvalues: 1,
  forval: 1,
  while: 2,
  if: 3,
  else: 3,
  capture: 4,
  cap: 4,
  quietly: 5,
  qui: 5,
  noisily: 6,
  noi: 6,
};

/**
 * Estimation commands the stub knows. A short, honest list: the real one is
 * `stratum-effects`' registry, and a long list here would only make the stub
 * look more authoritative than it is.
 */
const ESTIMATION_WORDS = new Set([
  "regress",
  "reg",
  "logit",
  "probit",
  "poisson",
  "areg",
  "ivregress",
  "xtreg",
  "mixed",
  "anova",
  "nbreg",
  "tobit",
  "heckman",
]);

/** Flat output of one segmentation pass. Byte offsets throughout. */
export interface NaiveSegmentation {
  /** Flat region rows, `REGION_STRIDE` each. */
  rows: number[];
  /** Flat hash rows, `REGION_HASH_STRIDE` each, as BigInts. */
  hashes: bigint[];
  /** Flat section rows, `SECTION_STRIDE` each. */
  sections: number[];
  /** Flat narrative rows, `NARRATIVE_STRIDE` each. */
  narrative: number[];
  /** Parse diagnostics. The stub emits one, for being the stub. */
  diagnostics: Diagnostic[];
}

function isSpace(b: number): boolean {
  return b === SPACE || b === TAB || b === CR;
}

function isWordByte(b: number): boolean {
  return (
    (b >= LOWER_A && b <= LOWER_Z) ||
    (b >= UPPER_A && b <= UPPER_Z) ||
    (b >= ZERO && b <= NINE) ||
    b === UNDERSCORE
  );
}

function lower(b: number): number {
  return b >= UPPER_A && b <= UPPER_Z ? b + 32 : b;
}

/** The ASCII word starting at the first non-blank byte of `[from, to)`. */
function firstWord(buf: Uint8Array, from: number, to: number): { word: string; at: number } {
  let i = from;
  while (i < to && isSpace(buf[i] as number)) i++;
  const at = i;
  let word = "";
  // `#delimit` leads with a non-word byte, so the sigil joins the word.
  if (i < to && buf[i] === HASH) {
    word += "#";
    i++;
  }
  while (i < to && isWordByte(buf[i] as number)) {
    word += String.fromCharCode(lower(buf[i] as number));
    i++;
  }
  return { word, at };
}

/** Index of the end of the physical line containing `from` (before its LF). */
function lineEnd(buf: Uint8Array, from: number): number {
  let i = from;
  while (i < buf.length && buf[i] !== LF) i++;
  return i;
}

/** True when the line's last non-blank bytes are `///`. */
function continues(buf: Uint8Array, from: number, to: number): boolean {
  let i = to;
  while (i > from && isSpace(buf[i - 1] as number)) i--;
  return i - from >= 3 && buf[i - 1] === SLASH && buf[i - 2] === SLASH && buf[i - 3] === SLASH;
}

/** Comment shape of a line, if it is nothing but a comment. */
type CommentShape =
  | { shape: "none" }
  | { shape: "blank" }
  | { shape: "line" }
  | { shape: "narrative-line" }
  | { shape: "section"; titleFrom: number; titleTo: number }
  | { shape: "block-open"; narrative: boolean };

function classifyComment(buf: Uint8Array, from: number, to: number): CommentShape {
  let i = from;
  while (i < to && isSpace(buf[i] as number)) i++;
  if (i >= to) return { shape: "blank" };

  const b0 = buf[i] as number;
  const b1 = i + 1 < to ? (buf[i + 1] as number) : -1;
  const b2 = i + 2 < to ? (buf[i + 2] as number) : -1;

  // `* …` is a comment only in command position, which is where we are.
  if (b0 === STAR) {
    const marker = sectionTitle(buf, i + 1, to);
    return marker ?? { shape: "line" };
  }
  if (b0 === SLASH && b1 === SLASH) {
    if (b2 === PIPE) return { shape: "narrative-line" };
    const marker = sectionTitle(buf, i + 2, to);
    return marker ?? { shape: "line" };
  }
  if (b0 === SLASH && b1 === STAR) {
    // `/*md` opens a narrative block; anything else is a plain comment.
    const narrative = b2 === LOWER_M && i + 3 < to && buf[i + 3] === LOWER_D;
    return { shape: "block-open", narrative };
  }
  return { shape: "none" };
}

/** `%% Label` after a comment sigil — 06 §4.8's section marker. */
function sectionTitle(
  buf: Uint8Array,
  from: number,
  to: number,
): { shape: "section"; titleFrom: number; titleTo: number } | null {
  let i = from;
  while (i < to && isSpace(buf[i] as number)) i++;
  if (i + 1 >= to || buf[i] !== PERCENT || buf[i + 1] !== PERCENT) return null;
  i += 2;
  while (i < to && isSpace(buf[i] as number)) i++;
  let end = to;
  while (end > i && isSpace(buf[end - 1] as number)) end--;
  return { shape: "section", titleFrom: i, titleTo: end };
}

// --- hashing ----------------------------------------------------------------
//
// A 32-bit FNV-1a run four times with different offset bases, packed into two
// 64-bit halves. It is not blake3 and does not pretend to be: nothing compares a
// stub hash with an engine `CodeHash`. What it must be is a stable, collision-shy
// function of the region's canonical text, because the editor keys result cards
// on it between edits.

const FNV_PRIME = 0x01000193;
const FNV_BASES = [0x811c9dc5, 0x1000193, 0x27d4eb2f, 0x165667b1];

function fnv(buf: Uint8Array, from: number, to: number, basis: number): number {
  let h = basis >>> 0;
  let pendingSpace = false;
  let started = false;
  for (let i = from; i < to; i++) {
    const b = buf[i] as number;
    if (isSpace(b) || b === LF) {
      // Insignificant whitespace collapses, so re-indenting a block does not
      // orphan its result card.
      if (started) pendingSpace = true;
      continue;
    }
    if (pendingSpace) {
      h = Math.imul(h ^ SPACE, FNV_PRIME) >>> 0;
      pendingSpace = false;
    }
    started = true;
    h = Math.imul(h ^ b, FNV_PRIME) >>> 0;
  }
  return h >>> 0;
}

/**
 * `[hash_lo, hash_hi]` in flat-row slot order — the order CONTRACTS §14 spells
 * the u64 view in, and the order `ReferenceSegmenter` fills them in.
 *
 * The names are the slots', not a claim about which half of some canonical
 * 128-bit value each is: there is no canonical value here, only four independent
 * 32-bit runs. Naming them for the slots is what makes the cross-backend
 * comparison in `conformance.ts` a check on the encoding rather than on two
 * conventions that happen to line up.
 */
function hashPair(buf: Uint8Array, from: number, to: number): [bigint, bigint] {
  const h = FNV_BASES.map((basis) => fnv(buf, from, to, basis));
  const lo = (BigInt(h[0] as number) << 32n) | BigInt(h[1] as number);
  const hi = (BigInt(h[2] as number) << 32n) | BigInt(h[3] as number);
  return [lo, hi];
}

// --- the splitter -----------------------------------------------------------

interface Pending {
  codeFrom: number;
  codeTo: number;
  kind: RegionKind;
  entryDelim: number;
  exitDelim: number;
  headLine: number;
  lastLine: number;
  executable: boolean;
  estimation: boolean;
  macroInHead: boolean;
  sectionHead: boolean;
}

/** Split `buf` into regions. Byte offsets throughout. */
export function segment(buf: Uint8Array): NaiveSegmentation {
  const pending: Pending[] = [];
  const sections: number[] = [];
  const narrative: number[] = [];
  let sectionId = 1;
  let delim = DELIM_CR;
  let pos = 0;
  let line = 0;

  while (pos < buf.length) {
    const startLine = line;
    const start = pos;
    const end = lineEnd(buf, pos);
    let logicalEnd = end;

    const comment = classifyComment(buf, start, end);

    if (comment.shape === "blank" || comment.shape === "line" || comment.shape === "section") {
      // Merge a run of consecutive blank/comment lines into one Trivia region,
      // so the gutter shows one inert band rather than a stripe per line.
      const hasMarker = comment.shape === "section";
      if (comment.shape === "section") {
        sections.push(start, 0, sectionId++, comment.titleFrom, comment.titleTo, startLine);
      }
      let cursor = end;
      let cursorLine = line;
      while (cursor < buf.length) {
        const nextStart = cursor + 1;
        if (nextStart > buf.length) break;
        const nextEnd = lineEnd(buf, nextStart);
        const nextShape = classifyComment(buf, nextStart, nextEnd);
        if (nextShape.shape === "blank" || nextShape.shape === "line") {
          cursor = nextEnd;
          cursorLine++;
          continue;
        }
        break;
      }
      pending.push({
        codeFrom: start,
        codeTo: cursor,
        kind: { kind: "trivia", has_marker: hasMarker },
        entryDelim: delim,
        exitDelim: delim,
        headLine: startLine,
        lastLine: cursorLine,
        executable: false,
        estimation: false,
        macroInHead: false,
        sectionHead: hasMarker,
      });
      pos = cursor + 1;
      line = cursorLine + 1;
      continue;
    }

    if (comment.shape === "narrative-line") {
      let cursor = end;
      let cursorLine = line;
      while (cursor < buf.length) {
        const nextStart = cursor + 1;
        const nextEnd = lineEnd(buf, nextStart);
        if (classifyComment(buf, nextStart, nextEnd).shape !== "narrative-line") break;
        cursor = nextEnd;
        cursorLine++;
      }
      narrative.push(start, cursor, NARRATIVE_LINE);
      pending.push({
        codeFrom: start,
        codeTo: cursor,
        kind: { kind: "trivia", has_marker: false },
        entryDelim: delim,
        exitDelim: delim,
        headLine: startLine,
        lastLine: cursorLine,
        executable: false,
        estimation: false,
        macroInHead: false,
        sectionHead: false,
      });
      pos = cursor + 1;
      line = cursorLine + 1;
      continue;
    }

    if (comment.shape === "block-open") {
      const close = findBlockClose(buf, start);
      const closed = close.at >= 0;
      const stop = closed ? close.at : buf.length;
      if (comment.narrative) narrative.push(start, stop, NARRATIVE_BLOCK);
      pending.push({
        codeFrom: start,
        codeTo: stop,
        kind: closed
          ? { kind: "trivia", has_marker: false }
          : { kind: "unterminated", expected: "block_comment" },
        entryDelim: delim,
        exitDelim: delim,
        headLine: startLine,
        lastLine: line + close.lines,
        executable: false,
        estimation: false,
        macroInHead: false,
        sectionHead: false,
      });
      pos = stop + 1;
      line += close.lines + 1;
      continue;
    }

    // A code line. Fold `///` continuations, then `#delimit ;` statements.
    let lines = 0;
    while (continues(buf, start, logicalEnd) && logicalEnd < buf.length) {
      const next = lineEnd(buf, logicalEnd + 1);
      logicalEnd = next;
      lines++;
    }
    if (delim === DELIM_SEMI) {
      while (logicalEnd < buf.length && !hasSemicolon(buf, start, logicalEnd)) {
        logicalEnd = lineEnd(buf, logicalEnd + 1);
        lines++;
      }
    }

    const head = firstWord(buf, start, logicalEnd);
    let kind: RegionKind = { kind: "simple" };
    let executable = true;
    let exitDelim = delim;

    if (head.word === "#delimit") {
      // `#delimit` is 8 bytes; what follows decides the mode.
      const arg = firstWord(buf, head.at + 8, logicalEnd);
      if (arg.word === "cr") {
        exitDelim = DELIM_CR;
        kind = { kind: "directive", directive: "delimit_cr" };
      } else if (nextNonBlank(buf, head.at + 8, logicalEnd) === SEMI) {
        exitDelim = DELIM_SEMI;
        kind = { kind: "directive", directive: "delimit_semi" };
      } else {
        kind = { kind: "directive", directive: "other" };
      }
    } else if (head.word in END_BLOCK_WORDS) {
      const close = findEnd(buf, logicalEnd);
      kind =
        close.at >= 0
          ? { kind: "end_block", opener: endBlockName(head.word), name: null }
          : { kind: "unterminated", expected: "end" };
      if (close.at >= 0) {
        logicalEnd = close.at;
        lines += close.lines;
      } else {
        logicalEnd = buf.length;
        lines += close.lines;
        executable = false;
      }
    } else {
      const depth = braceDelta(buf, start, logicalEnd);
      if (depth > 0) {
        const close = findBraceClose(buf, logicalEnd, depth);
        const opener = BRACE_WORDS[head.word];
        kind =
          close.at >= 0
            ? { kind: "brace", opener: braceOpenerName(opener) }
            : { kind: "unterminated", expected: "close_brace" };
        if (close.at >= 0) {
          logicalEnd = close.at;
          lines += close.lines;
        } else {
          logicalEnd = buf.length;
          lines += close.lines;
          executable = false;
        }
      }
    }

    const codeTo = trimEnd(buf, start, logicalEnd);
    pending.push({
      codeFrom: head.at,
      codeTo,
      kind,
      entryDelim: delim,
      exitDelim,
      headLine: startLine,
      lastLine: startLine + lines,
      executable,
      estimation: ESTIMATION_WORDS.has(head.word),
      macroInHead:
        nextNonBlank(buf, start, logicalEnd) === BACKTICK ||
        nextNonBlank(buf, start, logicalEnd) === DOLLAR,
      sectionHead: false,
    });
    delim = exitDelim;
    pos = logicalEnd + 1;
    line = startLine + lines + 1;
  }

  return flatten(buf, pending, sections, narrative);
}

function endBlockName(word: string): "program" | "input" | "mata" | "python" | "java" {
  switch (END_BLOCK_WORDS[word]) {
    case 1:
      return "input";
    case 2:
      return "mata";
    case 3:
      return "python";
    case 4:
      return "java";
    default:
      return "program";
  }
}

function braceOpenerName(
  code: number | undefined,
):
  | "foreach"
  | "forvalues"
  | "while"
  | "if_else_chain"
  | "capture"
  | "quietly"
  | "noisily"
  | "anonymous"
  | "other" {
  switch (code) {
    case 0:
      return "foreach";
    case 1:
      return "forvalues";
    case 2:
      return "while";
    case 3:
      return "if_else_chain";
    case 4:
      return "capture";
    case 5:
      return "quietly";
    case 6:
      return "noisily";
    default:
      return "anonymous";
  }
}

function nextNonBlank(buf: Uint8Array, from: number, to: number): number {
  let i = from;
  while (i < to && isSpace(buf[i] as number)) i++;
  return i < to ? (buf[i] as number) : -1;
}

function trimEnd(buf: Uint8Array, from: number, to: number): number {
  let i = Math.min(to, buf.length);
  while (i > from && (isSpace(buf[i - 1] as number) || buf[i - 1] === LF)) i--;
  return i;
}

function hasSemicolon(buf: Uint8Array, from: number, to: number): boolean {
  for (let i = from; i < to; i++) if (buf[i] === SEMI) return true;
  return false;
}

function braceDelta(buf: Uint8Array, from: number, to: number): number {
  let depth = 0;
  for (let i = from; i < to; i++) {
    if (buf[i] === LBRACE) depth++;
    else if (buf[i] === RBRACE) depth--;
  }
  return depth;
}

function findBraceClose(
  buf: Uint8Array,
  from: number,
  depth: number,
): { at: number; lines: number } {
  let d = depth;
  let lines = 0;
  for (let i = from; i < buf.length; i++) {
    const b = buf[i];
    if (b === LF) lines++;
    else if (b === LBRACE) d++;
    else if (b === RBRACE) {
      d--;
      if (d === 0) return { at: i + 1, lines };
    }
  }
  return { at: -1, lines };
}

function findBlockClose(buf: Uint8Array, from: number): { at: number; lines: number } {
  let lines = 0;
  for (let i = from + 2; i + 1 < buf.length; i++) {
    if (buf[i] === LF) lines++;
    if (buf[i] === STAR && buf[i + 1] === SLASH) return { at: i + 2, lines };
  }
  return { at: -1, lines };
}

function findEnd(buf: Uint8Array, from: number): { at: number; lines: number } {
  let lines = 0;
  let pos = from;
  while (pos < buf.length) {
    const start = pos + 1;
    if (start > buf.length) break;
    const end = lineEnd(buf, start);
    lines++;
    if (firstWord(buf, start, end).word === "end") return { at: end, lines };
    pos = end;
  }
  return { at: -1, lines };
}

/**
 * Turn the pending list into the flat rows the raw surface exposes.
 *
 * Outer spans are assigned here, and they **tile the document exactly**: each
 * region's outer runs from the previous region's outer end to the start of the
 * next region, with the last one reaching the end of the buffer. CONTRACTS §2
 * makes that tiling normative, and the editor's "which region is the cursor in"
 * lookup is a binary search that assumes it.
 */
function flatten(
  buf: Uint8Array,
  pending: Pending[],
  sections: number[],
  narrative: number[],
): NaiveSegmentation {
  const rows: number[] = [];
  const hashes: bigint[] = [];
  const ordinals = new Map<string, number>();

  for (let i = 0; i < pending.length; i++) {
    const p = pending[i] as Pending;
    const outerFrom = i === 0 ? 0 : (rows[(i - 1) * 9 + 3] as number);
    const outerTo = i === pending.length - 1 ? buf.length : (pending[i + 1] as Pending).codeFrom;
    const [lo, hi] = hashPair(buf, p.codeFrom, p.codeTo);
    const key = `${lo}:${hi}`;
    const ordinal = ordinals.get(key) ?? 0;
    ordinals.set(key, ordinal + 1);

    let flags = 0;
    if (p.executable) flags |= FLAG_EXECUTABLE;
    if (p.estimation) flags |= FLAG_ESTIMATION;
    if (p.macroInHead) flags |= FLAG_MACRO_IN_HEAD;
    if (p.exitDelim === DELIM_SEMI) flags |= FLAG_EXIT_SEMI;
    if (p.sectionHead) flags |= FLAG_SECTION_HEAD;

    rows.push(
      p.codeFrom,
      p.codeTo,
      outerFrom,
      outerTo,
      encodeKind(p.kind),
      p.entryDelim,
      p.headLine,
      p.lastLine,
      flags,
    );
    hashes.push(lo, hi, BigInt(ordinal));
  }

  // Sections run to the start of the next section, or to EOF.
  for (let i = 0; i < sections.length; i += 6) {
    const next = i + 6 < sections.length ? (sections[i + 6] as number) : buf.length;
    sections[i + 1] = next;
  }

  return { rows, hashes, sections, narrative, diagnostics: [stubDiagnostic()] };
}

/**
 * The stub announcing itself in the problems pane.
 *
 * Every field of `stratum_proto::Diagnostic` is spelled out, `null`s included:
 * the real module serialises the whole struct, so a stub that emits eight of the
 * twelve keys is one an editor can identify by `"related" in d`.
 */
function stubDiagnostic(): Diagnostic {
  return {
    severity: "warning",
    code: "WASM0900",
    stata_rc: null,
    message: "block segmentation is coming from the development stub, not from the Stata parser",
    file: null,
    span: null,
    offending_token: null,
    block: null,
    related: [],
    suggestions: [],
    notes: ["Regions, tokens and completions here are approximate by design."],
    confidence: "speculative",
  };
}

// --- tokens -----------------------------------------------------------------

const TAG = Object.fromEntries(TOKEN_TAGS.map((t, i) => [t, i])) as Record<
  (typeof TOKEN_TAGS)[number],
  number
>;

/** Naive tokens overlapping `[from, to)`. Byte offsets, flat triples. */
export function tokenize(buf: Uint8Array, from: number, to: number): number[] {
  const out: number[] = [];
  let i = Math.max(0, from);
  const end = Math.min(to, buf.length);
  while (i < end) {
    const b = buf[i] as number;
    const start = i;
    if (isSpace(b) || b === LF) {
      while (i < end && (isSpace(buf[i] as number) || buf[i] === LF)) i++;
      out.push(start, i, TAG.whitespace as number);
      continue;
    }
    if (b === SLASH && buf[i + 1] === SLASH) {
      while (i < end && buf[i] !== LF) i++;
      out.push(start, i, TAG.comment as number);
      continue;
    }
    if (b === SLASH && buf[i + 1] === STAR) {
      i += 2;
      while (i + 1 < end && !(buf[i] === STAR && buf[i + 1] === SLASH)) i++;
      i = Math.min(i + 2, end);
      out.push(start, i, TAG.comment as number);
      continue;
    }
    if (b === DQUOTE) {
      i++;
      while (i < end && buf[i] !== DQUOTE && buf[i] !== LF) i++;
      i = Math.min(i + 1, end);
      out.push(start, i, TAG.str_lit as number);
      continue;
    }
    if (b === BACKTICK || b === DOLLAR) {
      i++;
      while (i < end && (isWordByte(buf[i] as number) || buf[i] === LBRACE || buf[i] === RBRACE)) {
        i++;
      }
      if (i < end && buf[i] === 39) i++; // closing '
      out.push(start, i, TAG.macro_ref as number);
      continue;
    }
    if ((b >= ZERO && b <= NINE) || (b === DOT && isDigit(buf[i + 1]))) {
      while (
        i < end &&
        (((buf[i] as number) >= ZERO && (buf[i] as number) <= NINE) || buf[i] === DOT)
      ) {
        i++;
      }
      out.push(start, i, TAG.number as number);
      continue;
    }
    if (isWordByte(b)) {
      while (i < end && isWordByte(buf[i] as number)) i++;
      out.push(start, i, TAG.ident as number);
      continue;
    }
    i++;
    out.push(start, i, punctuationTag(b));
  }
  return out;
}

function isDigit(b: number | undefined): boolean {
  return b !== undefined && b >= ZERO && b <= NINE;
}

function punctuationTag(b: number): number {
  switch (b) {
    case 44:
      return TAG.comma as number;
    case 58:
      return TAG.colon as number;
    case 40:
      return TAG.l_paren as number;
    case 41:
      return TAG.r_paren as number;
    case LBRACE:
      return TAG.l_brace as number;
    case RBRACE:
      return TAG.r_brace as number;
    case 91:
      return TAG.l_bracket as number;
    case 93:
      return TAG.r_bracket as number;
    case SEMI:
      return TAG.statement_break as number;
    case HASH:
      return TAG.directive as number;
    default:
      return TAG.op as number;
  }
}
