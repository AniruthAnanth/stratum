/**
 * Context keys and the `when` expression language — 06 §12.1.
 *
 * `when` is a boolean expression over context keys. It is compiled once at
 * keymap-load time into a closure, never `eval`'d and never re-parsed per
 * keystroke: the resolver runs inside the 16 ms budget from `Mod+Enter` to a
 * glyph on screen (06 §15.1), and `script-src 'self'` (CONTRACTS §10.2) makes
 * `eval` unavailable in the packaged app anyway.
 */

import { createSignal } from "solid-js";

export type ContextValue = boolean | string | number;

/** The context keys 06 §12.1 enumerates. Extra keys are permitted; unknown keys read as `false`. */
export interface KeyContext {
  readonly editorFocus?: boolean;
  readonly commandBarFocus?: boolean;
  readonly historyFocus?: boolean;
  readonly variablesFocus?: boolean;
  readonly dataEditorFocus?: boolean;
  readonly cardFocus?: boolean;
  readonly selectionEmpty?: boolean;
  readonly blockHasResult?: boolean;
  readonly anyStale?: boolean;
  readonly running?: boolean;
  readonly layout?: string;
  readonly docView?: boolean;
  readonly inlineMode?: string;
  readonly platform?: string;
  readonly [key: string]: ContextValue | undefined;
}

export const CONTEXT_KEYS = [
  "editorFocus",
  "commandBarFocus",
  "historyFocus",
  "variablesFocus",
  "dataEditorFocus",
  "cardFocus",
  "selectionEmpty",
  "blockHasResult",
  "anyStale",
  "running",
  "layout",
  "docView",
  "inlineMode",
  "platform",
] as const;

export type CompiledWhen = (ctx: KeyContext) => boolean;

export class WhenParseError extends Error {}

// ---------------------------------------------------------------------------
// Tokenizer
// ---------------------------------------------------------------------------

type Token =
  | { t: "ident"; v: string }
  | { t: "string"; v: string }
  | { t: "number"; v: number }
  | { t: "op"; v: "&&" | "||" | "!" | "==" | "!=" | "(" | ")" };

function tokenize(src: string): Token[] {
  const out: Token[] = [];
  let i = 0;
  while (i < src.length) {
    const c = src[i] as string;
    if (/\s/.test(c)) {
      i++;
      continue;
    }
    if (src.startsWith("&&", i) || src.startsWith("||", i)) {
      out.push({ t: "op", v: src.slice(i, i + 2) as "&&" | "||" });
      i += 2;
      continue;
    }
    if (src.startsWith("==", i) || src.startsWith("!=", i)) {
      out.push({ t: "op", v: src.slice(i, i + 2) as "==" | "!=" });
      i += 2;
      continue;
    }
    if (c === "!" || c === "(" || c === ")") {
      out.push({ t: "op", v: c });
      i++;
      continue;
    }
    if (c === "'" || c === '"') {
      const end = src.indexOf(c, i + 1);
      if (end < 0) throw new WhenParseError(`unterminated string in ${src}`);
      out.push({ t: "string", v: src.slice(i + 1, end) });
      i = end + 1;
      continue;
    }
    const word = /^[A-Za-z_][A-Za-z0-9_.]*/.exec(src.slice(i));
    if (word !== null) {
      out.push({ t: "ident", v: word[0] });
      i += word[0].length;
      continue;
    }
    const num = /^[0-9]+/.exec(src.slice(i));
    if (num !== null) {
      out.push({ t: "number", v: Number(num[0]) });
      i += num[0].length;
      continue;
    }
    throw new WhenParseError(`unexpected ${JSON.stringify(c)} in ${src}`);
  }
  return out;
}

// ---------------------------------------------------------------------------
// Recursive-descent parser -> closure
// ---------------------------------------------------------------------------

const truthy = (v: ContextValue | undefined): boolean =>
  v === undefined ? false : typeof v === "boolean" ? v : v !== "" && v !== 0;

export function compileWhen(source: string): CompiledWhen {
  const tokens = tokenize(source);
  let pos = 0;

  const peek = (): Token | undefined => tokens[pos];
  const eat = (v: string): boolean => {
    const t = peek();
    if (t?.t === "op" && t.v === v) {
      pos++;
      return true;
    }
    return false;
  };

  function or(): CompiledWhen {
    let left = and();
    while (eat("||")) {
      const right = and();
      const l = left;
      left = (ctx) => l(ctx) || right(ctx);
    }
    return left;
  }

  function and(): CompiledWhen {
    let left = unary();
    while (eat("&&")) {
      const right = unary();
      const l = left;
      left = (ctx) => l(ctx) && right(ctx);
    }
    return left;
  }

  function unary(): CompiledWhen {
    if (eat("!")) {
      const inner = unary();
      return (ctx) => !inner(ctx);
    }
    return primary();
  }

  function primary(): CompiledWhen {
    if (eat("(")) {
      const inner = or();
      if (!eat(")")) throw new WhenParseError(`missing ) in ${source}`);
      return inner;
    }
    const t = peek();
    if (t === undefined) throw new WhenParseError(`unexpected end of ${source}`);
    if (t.t !== "ident") throw new WhenParseError(`expected a context key in ${source}`);
    pos++;
    const key = t.v;

    const next = peek();
    if (next?.t === "op" && (next.v === "==" || next.v === "!=")) {
      pos++;
      const rhs = peek();
      if (rhs === undefined || rhs.t === "op") {
        throw new WhenParseError(`expected a value after ${next.v} in ${source}`);
      }
      pos++;
      const want: ContextValue = rhs.t === "ident" ? rhs.v : rhs.v;
      const negate = next.v === "!=";
      return (ctx) => (ctx[key] === want) !== negate;
    }
    return (ctx) => truthy(ctx[key]);
  }

  const compiled = or();
  if (pos !== tokens.length) throw new WhenParseError(`trailing input in ${source}`);
  return compiled;
}

// ---------------------------------------------------------------------------
// The live context
// ---------------------------------------------------------------------------

const [context, setContext] = createSignal<KeyContext>({});

/** The context the resolver reads. One store; panes contribute keys to it. */
export const keyContext = context;

/** Merge keys in. Panes set their own focus key on focus and clear it on blur. */
export function setKeyContext(patch: Partial<Record<string, ContextValue | undefined>>): void {
  setContext((prev) => {
    const next: Record<string, ContextValue | undefined> = { ...prev };
    let changed = false;
    for (const [k, v] of Object.entries(patch)) {
      if (next[k] === v) continue;
      next[k] = v;
      changed = true;
    }
    return changed ? (next as KeyContext) : prev;
  });
}

/** Test seam and window teardown. */
export function resetKeyContext(): void {
  setContext({});
}
