/**
 * F-keys — 06 §9.1 ("Function keys insert their macro text at the caret") and
 * [U] 10.2 on this machine, which is where the defaults and the semicolon rule
 * come from:
 *
 * > By default, Stata defines the F-keys to mean
 * >   F1  help advice;   F2  describe;   F7  save   F8  use
 * > The semicolons at the end of some entries indicate an implied Enter.
 *
 * and, on redefining them:
 *
 * > . global F9 "list "
 * > … The space at the end of list is important.
 * > You can use the F-keys any way you desire: they contain a string of
 * > characters, and pressing the F-key is equivalent to typing those characters.
 *
 * Three facts follow, and all three are implemented literally:
 *
 *  1. An F-key is a **global macro** named `F1`…`F10`. It is not a keybinding
 *     with a command behind it, which is why redefining one is a `global` and
 *     not a preferences dialog — and why {@link setFunctionKey} exists for the
 *     macro-list sync to call rather than a settings screen.
 *  2. Pressing it **types the characters at the caret**. Not "runs a command":
 *     `F7` types `save ` and waits for a filename.
 *  3. A trailing `;` is an implied Enter — so the semicolon is removed and the
 *     line is submitted.
 *
 * F3 and F10 are reserved by Windows and cannot be programmed there ([U] 10.2);
 * we do not bind them anywhere, so nothing to suppress.
 */

/** [U] 10.2's table, exactly. Unlisted keys are empty, as in Stata. */
const DEFAULTS: Readonly<Record<number, string>> = {
  1: "help advice;",
  2: "describe;",
  7: "save ",
  8: "use ",
};

const macros = new Map<number, string>(Object.entries(DEFAULTS).map(([n, v]) => [Number(n), v]));

/** The macro text for `Fn`, or `""` when the key is undefined. */
export function functionKeyText(n: number): string {
  return macros.get(n) ?? "";
}

/**
 * Set `Fn`.
 *
 * Called by whatever is syncing Stata's global macros into the frontend — the
 * completion environment carries `globals` (CONTRACTS §9), and `global F9
 * "list "` in a `profile.do` must reach this table or the F-keys stop being
 * Stata's F-keys and become ours.
 */
export function setFunctionKey(n: number, text: string): void {
  macros.set(n, text);
}

/** Restore [U] 10.2's defaults. Test seam, and "reset preferences". */
export function resetFunctionKeys(): void {
  macros.clear();
  for (const [n, v] of Object.entries(DEFAULTS)) macros.set(Number(n), v);
}

export interface FunctionKeyAction {
  /** Characters to type at the caret. Never contains the implied-Enter `;`. */
  readonly insert: string;
  /** The trailing `;` was present: type it and press Enter. */
  readonly submit: boolean;
}

/**
 * What pressing `Fn` does. `undefined` for an unbound key, which must fall
 * through rather than swallow the keystroke — F5 is Reload in a webview and
 * F6/F7/F8 are media keys on a Mac, and eating them for a macro nobody defined
 * would be a regression a user cannot diagnose.
 */
export function functionKeyAction(n: number): FunctionKeyAction | undefined {
  const text = functionKeyText(n);
  if (text === "") return undefined;
  return text.endsWith(";")
    ? { insert: text.slice(0, -1), submit: true }
    : { insert: text, submit: false };
}
