/**
 * The magnifier filter, shared by History and Variables — 06 §9.3, §9.4.
 *
 * Both panes describe the same control, and the shipped manual describes it the
 * same way twice ([GSM] 2, *The History window* and *The Variables window*):
 *
 * > By default, the filter ignores case and finds any commands containing any of
 * > the words in the filter. Clicking on the arrow by the magnifying glass
 * > allows you to change this behavior.
 *
 * > The filter is applied to all visible columns and shows all variables that
 * > match the criteria in at least one column. By default, the filter will
 * > ignore case and show any variables for which at least one column contains
 * > any of the words in the filter.
 *
 * So one matcher, two panes. It lives under `panes/history/` because that is
 * where the manual introduces it and because both directories belong to this
 * unit; `panes/variables/` imports it rather than growing a second copy, and a
 * second copy is precisely how "match any word" comes to mean two things in one
 * product.
 *
 * # Any word, not substring
 *
 * `"reg mpg"` matches `regress price` (it contains `reg`) and it matches
 * `summarize mpg` (it contains `mpg`). A substring match on the whole query
 * would match neither, which is what makes a filter feel broken: the user typed
 * two words they can both see on screen and got nothing.
 */

export type FilterMode = "any" | "all" | "name";

export const FILTER_MODES: readonly { value: FilterMode; label: string }[] = [
  { value: "any", label: "Match any word" },
  { value: "all", label: "Match all words" },
  { value: "name", label: "Match name only" },
];

export interface FilterSpec {
  readonly query: string;
  readonly mode: FilterMode;
}

export const NO_FILTER: FilterSpec = { query: "", mode: "any" };

/** Words, lowercased. Whitespace-separated, as the manual's "words" implies. */
export function filterWords(query: string): string[] {
  return query
    .toLowerCase()
    .split(/\s+/u)
    .filter((w) => w !== "");
}

/**
 * Does a row match?
 *
 * `columns` is every **visible** column's text — the manual is explicit that the
 * filter follows the columns on screen, so hiding the Label column narrows what
 * the filter can see, and that is the behaviour rather than an oversight.
 * `name` is the first column, which is the only one `mode: "name"` consults.
 */
export function matchesFilter(
  spec: FilterSpec,
  name: string,
  columns: readonly string[] = [name],
): boolean {
  const words = filterWords(spec.query);
  if (words.length === 0) return true;

  const haystacks = (spec.mode === "name" ? [name] : columns).map((c) => c.toLowerCase());
  const hit = (word: string): boolean => haystacks.some((h) => h.includes(word));

  return spec.mode === "all" ? words.every(hit) : words.some(hit);
}
