/**
 * Tab completion in the Command window — 06 §9.1, and [U] 10.6 on this machine:
 *
 * > Simply type the first few letters of the variable name in the Command
 * > window and press the Tab key. Stata will automatically type the rest of the
 * > variable name for you. If more than one variable name matches the letters
 * > you have typed, Stata will complete as much as it can and beep at you …
 * >
 * > The tab-completion feature also applies to typing filenames. If you start by
 * > typing a double quote, `"`, you can type the first few letters of a filename
 * > or directory and press the Tab key.
 *
 * Two rules, and both are literal:
 *
 *  1. **complete as much as it can** — the longest common prefix of the
 *     matches, not the first match. Inserting the first match and letting the
 *     user Tab through the rest is VS Code's rule, not Stata's, and it silently
 *     produces the wrong variable when the user types on past it.
 *  2. **a leading `"` switches to filenames.** Not "a path-shaped token", not
 *     "after a `using`" — the quote, exactly as the manual says, which is also
 *     what makes it predictable inside a macro or an option.
 *
 * 06 §9.1 adds the modern half of rule 1: "when several match it shows the list
 * and further typing narrows it". So an ambiguous completion inserts the common
 * prefix *and* reports the matches; the beep becomes a list.
 *
 * # Where candidates come from
 *
 * Behind a seam, because the two sources are owned elsewhere and arrive at
 * different times. The default reads the live variable list from
 * `state/vars.ts`; W13's segmenter-backed source (`CompletionKind` already has
 * `variable` and `path`) supersedes it through {@link setCandidateSource} as
 * soon as an environment is attached. Filenames have **no** source until one
 * exists: CONTRACTS §11 has no directory-listing command and `CompletionEnv`
 * carries `cwd` but no entries, so with nothing installed file completion
 * declines rather than inventing paths. Escalated in W16's return.
 */

import { variables } from "../state/vars";

export type CompletionTarget = "variable" | "file";

export interface TokenAtCaret {
  readonly target: CompletionTarget;
  /** Start of the text being completed. For a file, *after* the opening quote. */
  readonly from: number;
  readonly to: number;
  readonly prefix: string;
}

export interface CompletionOutcome extends TokenAtCaret {
  /** What replaces `[from, to)`: the longest common prefix of `matches`. */
  readonly insert: string;
  /** Every match, ordered as the source gave them. Drives the list. */
  readonly matches: readonly string[];
  /** More than one match — Stata beeps here; we show the list. */
  readonly ambiguous: boolean;
}

export interface CandidateSource {
  variables(prefix: string): readonly string[];
  files(prefix: string): readonly string[];
}

const defaultSource: CandidateSource = {
  variables: (prefix) =>
    variables.rows.map((row) => row.name).filter((name) => name.startsWith(prefix)),
  // No host command lists a directory (CONTRACTS §11). Declining is the honest
  // answer; a hardcoded list would be a feature that works only in a demo.
  files: () => [],
};

let source: CandidateSource = defaultSource;

export function setCandidateSource(next: CandidateSource | null): void {
  source = next ?? defaultSource;
}

export interface CompleteCounters {
  /** Tab presses served. */
  requests: number;
  /** Candidates examined. Bounded by the environment, never by the log. */
  candidates: number;
  /** Requests that inserted text. */
  inserts: number;
}

const ZERO: CompleteCounters = { requests: 0, candidates: 0, inserts: 0 };
export const completeCounters: CompleteCounters = { ...ZERO };
export function resetCompleteCounters(): void {
  Object.assign(completeCounters, ZERO);
}

/** Stata name characters. Digits are legal inside a name, never at the start. */
const NAME_CHAR = /[A-Za-z0-9_]/;

/**
 * The token the caret is in, and which kind of completion it wants.
 *
 * File mode is decided by scanning back to the nearest unbalanced `"` on the
 * line. Unbalanced, not "the previous character": the user is completing after
 * typing several characters of the path, which is the entire gesture the manual
 * describes, so the quote is usually well behind the caret.
 */
export function tokenAtCaret(text: string, caret: number): TokenAtCaret | null {
  const at = Math.max(0, Math.min(caret, text.length));
  const lineStart = text.lastIndexOf("\n", at - 1) + 1;
  const line = text.slice(lineStart, at);

  // Count quotes on the line: an odd number means the caret is inside one.
  let openQuote = -1;
  for (let i = 0; i < line.length; i++) {
    if (line[i] !== '"') continue;
    openQuote = openQuote < 0 ? i : -1;
  }
  if (openQuote >= 0) {
    const from = lineStart + openQuote + 1;
    return { target: "file", from, to: at, prefix: text.slice(from, at) };
  }

  let from = at;
  while (from > lineStart && NAME_CHAR.test(text[from - 1] as string)) from--;
  if (from === at) return null;
  return { target: "variable", from, to: at, prefix: text.slice(from, at) };
}

/** The longest string every candidate starts with. Rule 1, verbatim. */
export function longestCommonPrefix(candidates: readonly string[]): string {
  const first = candidates[0];
  if (first === undefined) return "";
  let end = first.length;
  for (const candidate of candidates) {
    let i = 0;
    while (i < end && i < candidate.length && candidate[i] === first[i]) i++;
    end = i;
    if (end === 0) break;
  }
  return first.slice(0, end);
}

/**
 * Complete at the caret.
 *
 * `null` means "nothing to offer", and the caller must then let Tab do whatever
 * Tab does otherwise — which in a Command window is nothing at all, not a tab
 * character: Stata's Command window has no tab stops.
 */
export function completeAt(text: string, caret: number): CompletionOutcome | null {
  completeCounters.requests += 1;
  const token = tokenAtCaret(text, caret);
  if (token === null) return null;

  const matches =
    token.target === "variable" ? source.variables(token.prefix) : source.files(token.prefix);
  completeCounters.candidates += matches.length;
  if (matches.length === 0) return null;

  const insert = longestCommonPrefix(matches);
  if (insert.length > token.prefix.length) completeCounters.inserts += 1;
  return {
    ...token,
    insert,
    matches,
    ambiguous: matches.length > 1,
  };
}
