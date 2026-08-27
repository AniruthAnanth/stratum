/**
 * Turning a selection into a varlist, and a varlist into a command — [GSM] 2:
 *
 * > • **Keep only variable “varname”** (or **Keep only selected variables** …)
 * >   to keep just the selected variables in the dataset in memory. You will be
 * >   asked for confirmation.
 * > • **Drop variable “varname”** (or **Drop selected variables** …)
 * > • **Copy varlist** to copy the selected variable names to the clipboard.
 * > • **Send varlist to Command window** to send all selected variables to the
 * >   Command window.
 * > • **Output compact varlist** (e.g., v1-v4) toggles the preference for
 * >   shortening the list of variables by variable ranges in place of individual
 * >   variable names.
 * >
 * > **Items from the contextual menu issue standard Stata commands, so working
 * > by right-clicking is just like working directly in the Command window.**
 *
 * That last sentence is the whole reason this file exists as a pure module: the
 * menu does not "apply a change", it *composes a command string* which then goes
 * through the same `submitCommand` the Command window uses. So the text is
 * testable without a pane, and the pane cannot acquire a private mutation path —
 * which is what would break the reproducibility property 06 §9.4 and §9.5 both
 * rest on.
 *
 * # The range form
 *
 * `v1-v4` in Stata means "every variable from `v1` to `v4` **in dataset
 * order**", so a range may only be emitted for names that are adjacent in the
 * dataset — never in the pane's display order, which can be sorted or filtered.
 * Collapsing a run of two (`a b` → `a-b`) saves nothing and loses the ability to
 * read the list, so the threshold is three.
 */

/** How a varlist is spelled. The `Output compact varlist` preference. */
export type VarlistStyle = "explicit" | "compact";

/**
 * The selected names as a varlist.
 *
 * `all` is the dataset order — the frame's variable order, not the pane's — and
 * adjacency is judged in it. Names not present in `all` are emitted verbatim in
 * the order given: that only happens when a variable was dropped out from under
 * the selection, and dropping it from the command silently would issue a
 * *different command from the one the menu item promised*.
 */
export function varlistText(
  selected: readonly string[],
  all: readonly string[],
  style: VarlistStyle = "explicit",
): string {
  if (selected.length === 0) return "";
  if (style === "explicit") return selected.join(" ");

  const index = new Map<string, number>();
  all.forEach((name, i) => index.set(name, i));

  // Dataset order first: a range is only legal over consecutive positions, and
  // the user may have selected them bottom-up or while the pane was sorted.
  const positioned = selected
    .map((name) => ({ name, at: index.get(name) }))
    .filter((e): e is { name: string; at: number } => e.at !== undefined)
    .sort((a, b) => a.at - b.at);
  const unknown = selected.filter((name) => !index.has(name));

  const parts: string[] = [];
  let runStart = 0;
  for (let i = 1; i <= positioned.length; i++) {
    const previous = positioned[i - 1];
    const current = positioned[i];
    const contiguous =
      previous !== undefined && current !== undefined && current.at === previous.at + 1;
    if (contiguous) continue;

    const run = positioned.slice(runStart, i);
    const first = run[0];
    const last = run.at(-1);
    if (first === undefined || last === undefined) continue;
    if (run.length >= 3) parts.push(`${first.name}-${last.name}`);
    else for (const entry of run) parts.push(entry.name);
    runStart = i;
  }

  return [...parts, ...unknown].join(" ");
}

/**
 * `keep` / `drop`, exactly as the user would have typed them.
 *
 * No `capture`, no `quietly`: the point of issuing a real command is that its
 * output and its `_rc` land in Results, History and the log like any other, and
 * a swallowed error would be a right-click that silently did nothing.
 */
export function keepCommand(
  selected: readonly string[],
  all: readonly string[],
  style: VarlistStyle = "explicit",
): string {
  return `keep ${varlistText(selected, all, style)}`;
}

export function dropCommand(
  selected: readonly string[],
  all: readonly string[],
  style: VarlistStyle = "explicit",
): string {
  return `drop ${varlistText(selected, all, style)}`;
}

/**
 * The menu item's own label, which names the variable when there is exactly one.
 *
 * [GSM] 2 spells it with typographic quotes — `Keep only variable “price”` — and
 * so do we: this is a menu label, not code, and matching the manual's own
 * wording is what makes a twenty-year user recognise the item without reading
 * it.
 */
export function keepLabel(selected: readonly string[]): string {
  const only = selected.length === 1 ? selected[0] : undefined;
  return only === undefined ? "Keep only selected variables" : `Keep only variable “${only}”`;
}

export function dropLabel(selected: readonly string[]): string {
  const only = selected.length === 1 ? selected[0] : undefined;
  return only === undefined ? "Drop selected variables" : `Drop variable “${only}”`;
}

/**
 * The confirmation sentence.
 *
 * "This affects only the dataset in memory, not the dataset as saved on your
 * disk" is in the manual for both verbs, and it is the sentence that stops the
 * dialog from being frightening. It is repeated here rather than paraphrased.
 */
export function confirmationText(verb: "keep" | "drop", command: string): string {
  const what =
    verb === "keep"
      ? "Keep only these variables in the dataset in memory?"
      : "Drop these variables from the dataset in memory?";
  return `${what} This affects only the dataset in memory, not the dataset as saved on your disk.\n\n. ${command}`;
}
