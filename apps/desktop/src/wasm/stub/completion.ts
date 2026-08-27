/**
 * The stub's completion source.
 *
 * Prefix matching over whatever the engine pushed in `CompletionEnv`, plus a
 * short keyword list. No command table, no varlist grammar, no expression
 * context — those are W04b and W20. What it does implement faithfully is the
 * *shape*: a replaced byte range, a total order over the items, and the
 * `truncated`/`offered`/`total` triple the popup renders as "2 048 of 32 767"
 * (A11), so the popup can be built and reviewed before the real source exists.
 */

import type { CompletionItem, CompletionKind, CompletionList } from "../types.ts";
import type { StubEnv } from "./msgpack.ts";

const UNDERSCORE = 95;
const ZERO = 48;
const NINE = 57;
const UPPER_A = 65;
const UPPER_Z = 90;
const LOWER_A = 97;
const LOWER_Z = 122;
const BACKTICK = 96;
const DOLLAR = 36;

/** Commands the stub offers in command position. Deliberately short. */
const KEYWORDS = [
  "generate",
  "replace",
  "summarize",
  "tabulate",
  "regress",
  "list",
  "keep",
  "drop",
  "merge",
  "append",
  "sort",
  "use",
  "save",
  "import",
  "export",
  "label",
  "egen",
  "collapse",
  "reshape",
  "foreach",
  "forvalues",
  "while",
  "program",
  "display",
];

function isWordByte(b: number): boolean {
  return (
    (b >= LOWER_A && b <= LOWER_Z) ||
    (b >= UPPER_A && b <= UPPER_Z) ||
    (b >= ZERO && b <= NINE) ||
    b === UNDERSCORE
  );
}

/** The identifier-ish token the cursor sits in, as a byte range. */
function tokenAt(buf: Uint8Array, pos: number): { from: number; to: number; text: string } {
  let from = Math.max(0, Math.min(pos, buf.length));
  while (from > 0 && isWordByte(buf[from - 1] as number)) from--;
  let to = Math.max(0, Math.min(pos, buf.length));
  while (to < buf.length && isWordByte(buf[to] as number)) to++;
  let text = "";
  for (let i = from; i < to; i++) text += String.fromCharCode(buf[i] as number);
  return { from, to, text };
}

/** The sigil immediately before the token, which decides what we complete. */
function sigil(buf: Uint8Array, from: number): number {
  return from > 0 ? (buf[from - 1] as number) : -1;
}

function item(label: string, kind: CompletionKind, rank: number, detail?: string): CompletionItem {
  return { label, kind, detail: detail ?? null, insert: null, rank };
}

/** Complete at a byte offset. */
export function completeAt(buf: Uint8Array, pos: number, env: StubEnv | null): CompletionList {
  const token = tokenAt(buf, pos);
  const prefix = token.text.toLowerCase();
  const before = sigil(buf, token.from);

  const candidates: CompletionItem[] = [];
  if (before === BACKTICK) {
    for (const name of env?.locals ?? []) candidates.push(item(name, "local", 0));
  } else if (before === DOLLAR) {
    for (const name of env?.globals ?? []) candidates.push(item(name, "global", 0));
  } else {
    for (const name of env?.varnames ?? []) candidates.push(item(name, "variable", 0));
    for (const name of env?.scalars ?? []) candidates.push(item(name, "scalar", 1));
    for (const name of env?.matrices ?? []) candidates.push(item(name, "matrix", 1));
    for (const name of env?.frames ?? []) candidates.push(item(name, "frame", 2));
    for (const name of env?.programs ?? []) candidates.push(item(name, "command", 3));
    for (const name of KEYWORDS) candidates.push(item(name, "command", 4));
  }

  const items = candidates
    .filter((c) => c.label.toLowerCase().startsWith(prefix))
    // A total order, so the popup is reproducible: rank, then label, then kind.
    // Two items that tie on all three are the same item.
    .sort(
      (a, b) => a.rank - b.rank || a.label.localeCompare(b.label) || a.kind.localeCompare(b.kind),
    );

  // `truncated` is about the ENVIRONMENT, not about this list: the engine
  // already shed entries before sending (A11), and the popup says so.
  const truncated = env?.truncated === true;
  const varTotal = env?.varTotal ?? 0;
  const varOffered = env?.varnames.length ?? 0;

  return {
    from: token.from,
    to: token.to,
    items,
    truncated,
    offered: truncated ? varOffered : items.length,
    total: truncated ? varTotal : items.length,
  };
}
