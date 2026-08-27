/**
 * Syntax highlighting — 06 §4.3, restrained, five hues, functional.
 *
 * Two properties matter more than the colours.
 *
 * **It is viewport-scoped.** The token query, the classification and the
 * decoration build all run over the visible range widened to whole regions, and
 * nothing here ever walks the document. `counters.highlightRangesBuilt` and
 * `counters.tokensDecoded` are asserted to be independent of document size in
 * `editor.perf.test.ts`: the same edit in a 40-line file and in a 6 000-line
 * file builds the same number of spans.
 *
 * **Numbers are not coloured, macros are.** 06 §4.3, and it is a correctness
 * argument rather than a taste one: numerals sit in result tables three lines
 * below the code and a coloured numeral in the editor fights them, whereas an
 * unintended macro expansion is the single most common Stata bug and `` `x' ``
 * being loud is what makes it visible.
 *
 * The palette is declared here as `--stx-*` custom properties rather than taken
 * from `resources/tokens.generated.css`, because that file — generated from
 * `design/tokens.json`, both W00's — carries no syntax hues at all. The values
 * below are 06 §4.3's table verbatim. This is a gap in the token source, not a
 * second palette; see the unit's return for the escalation. The three-state
 * theme pattern is the same one the generated file uses, so an explicit
 * `data-theme` still wins in both directions.
 */

import type { Range } from "@codemirror/state";
import { Decoration, EditorView, ViewPlugin } from "@codemirror/view";
import type { DecorationSet, ViewUpdate } from "@codemirror/view";
import { blockField } from "../blocks/blockField";
import { counters, segmenterOf } from "../blocks/segmenter";
import type { Block, EditorSegmenter } from "../blocks/segmenter";
import { classifyRegion } from "./parser";
import type { StataTag, TaggedSpan } from "./parser";

/** One `Decoration.mark` per tag, built once at module load and reused forever. */
const MARKS: Readonly<Record<StataTag, Decoration>> = {
  CommandName: Decoration.mark({ class: "stx-command" }),
  PrefixCommand: Decoration.mark({ class: "stx-command" }),
  Subcommand: Decoration.mark({ class: "stx-subcommand" }),
  OptionName: Decoration.mark({ class: "stx-option" }),
  OptionArg: Decoration.mark({ class: "stx-plain" }),
  VarName: Decoration.mark({ class: "stx-plain" }),
  VarWildcard: Decoration.mark({ class: "stx-wildcard" }),
  FactorOp: Decoration.mark({ class: "stx-factor" }),
  TsOp: Decoration.mark({ class: "stx-factor" }),
  LocalMacro: Decoration.mark({ class: "stx-local" }),
  GlobalMacro: Decoration.mark({ class: "stx-global" }),
  MacroFuncCall: Decoration.mark({ class: "stx-macrofn" }),
  String: Decoration.mark({ class: "stx-string" }),
  CompoundString: Decoration.mark({ class: "stx-string" }),
  Number: Decoration.mark({ class: "stx-plain" }),
  Missing: Decoration.mark({ class: "stx-missing" }),
  Comment: Decoration.mark({ class: "stx-comment" }),
  NarrativeComment: Decoration.mark({ class: "stx-narrative" }),
  SectionMarker: Decoration.mark({ class: "stx-sectionmarker" }),
  Continuation: Decoration.mark({ class: "stx-continuation" }),
  DelimitDirective: Decoration.mark({ class: "stx-directive" }),
  Qualifier: Decoration.mark({ class: "stx-qualifier" }),
  Weight: Decoration.mark({ class: "stx-qualifier" }),
  Format: Decoration.mark({ class: "stx-format" }),
  Operator: Decoration.mark({ class: "stx-plain" }),
  Brace: Decoration.mark({ class: "stx-brace" }),
  MataRegion: Decoration.mark({ class: "stx-foreign" }),
  Error: Decoration.mark({ class: "stx-error" }),
};

/**
 * Build the highlight decorations for the visible ranges.
 *
 * Widened to whole regions on both ends. Classification is a state machine
 * seeded at a region's first token, so starting it half way down a `foreach`
 * body would tag the first identifier in view as a command name — a wrong
 * colour that appears and disappears as you scroll, which is worse than none.
 */
function buildHighlight(view: EditorView, seg: EditorSegmenter): DecorationSet {
  const text = view.state.doc;
  const ranges: Range<Decoration>[] = [];
  const spans: TaggedSpan[] = [];

  for (const visible of view.visibleRanges) {
    const blocks = seg.blocksTouching(visible.from, visible.to);
    if (blocks.length === 0) continue;
    const first = blocks[0] as Block;
    const last = blocks[blocks.length - 1] as Block;
    const from = Math.max(0, first.outerFrom);
    const to = Math.min(text.length, last.outerTo);
    if (to <= from) continue;

    const tokens = seg.tokens(from, to);
    // One rope walk per visible range, over the visible range only. Slicing per
    // token would turn a 500-token viewport into 500 rope walks; slicing the
    // whole document would put O(document) work on the typing path, which is the
    // exact thing this unit is measured on.
    const slice = text.sliceString(from, to);

    let cursor = 0;
    for (const block of blocks) {
      const start = cursor;
      while (cursor < tokens.length) {
        const token = tokens[cursor];
        if (token === undefined || token.from >= block.outerTo) break;
        cursor += 1;
      }
      if (cursor > start) {
        spans.length = 0;
        classifyRegion(slice, from, block, tokens, start, cursor, spans);
        for (const span of spans) {
          if (span.to <= span.from) continue;
          const mark = MARKS[span.tag];
          ranges.push(mark.range(span.from, span.to));
        }
      }
    }
  }

  counters.highlightRangesBuilt += ranges.length;
  return Decoration.set(ranges, true);
}

/**
 * The highlighter.
 *
 * A `ViewPlugin` and not a `StateField` on purpose: the decorations depend on
 * the VIEWPORT, which is not part of the state, and a state field would have to
 * cover the whole document to be correct.
 */
export const stataHighlight = ViewPlugin.fromClass(
  class {
    decorations: DecorationSet;
    private gen = -1;

    constructor(readonly view: EditorView) {
      const seg = segmenterOf(view.state);
      this.decorations = seg === null ? Decoration.none : buildHighlight(view, seg);
      this.gen = view.state.field(blockField, false)?.gen ?? -1;
    }

    update(update: ViewUpdate): void {
      const seg = segmenterOf(update.state);
      if (seg === null) {
        this.decorations = Decoration.none;
        return;
      }
      const gen = update.state.field(blockField, false)?.gen ?? -1;
      // A selection-only transaction changes neither the text nor what is on
      // screen, and rebuilding for it is the difference between highlighting
      // costing something per keystroke and costing something per event.
      if (gen === this.gen && !update.viewportChanged && !update.docChanged) return;
      this.gen = gen;
      this.decorations = buildHighlight(update.view, seg);
    }
  },
  { decorations: (plugin) => plugin.decorations },
);

/**
 * 06 §4.3's palette, in the three-state theme pattern the generated tokens use.
 *
 * Light is the unconditional definition; dark applies under the OS preference
 * unless the document pinned light; an explicit `data-theme="dark"` wins in both
 * directions. Every rule is `&`-relative so CodeMirror's own scoping applies and
 * these cannot leak into the rest of the product.
 */
const LIGHT = {
  "--stx-command": "#1B3A6B",
  "--stx-subcommand": "#1B3A6B",
  "--stx-option": "#0F6B68",
  "--stx-local": "#8A5300",
  "--stx-global": "#7A3E00",
  "--stx-string": "#4A6212",
  "--stx-comment": "#7C8792",
  "--stx-narrative": "#5C636D",
  "--stx-sectionmarker": "#8A9099",
  "--stx-factor": "#8C3A6B",
  "--stx-missing": "#8A9099",
  "--stx-error": "#B3261E",
};

const DARK = {
  "--stx-command": "#8FB4E8",
  "--stx-subcommand": "#8FB4E8",
  "--stx-option": "#57C0B6",
  "--stx-local": "#E0A653",
  "--stx-global": "#D08A45",
  "--stx-string": "#A6C05B",
  "--stx-comment": "#6E7A86",
  "--stx-narrative": "#9AA4B0",
  "--stx-sectionmarker": "#6E7A86",
  "--stx-factor": "#D48CB8",
  "--stx-missing": "#7C8794",
  "--stx-error": "#F2726A",
};

export const stataHighlightTheme = EditorView.baseTheme({
  "&": LIGHT,
  "@media (prefers-color-scheme: dark)": {
    ":root:not([data-theme='light']) &": DARK,
  },
  ":root[data-theme='dark'] &": DARK,

  ".stx-command": { color: "var(--stx-command)", fontWeight: "600" },
  ".stx-subcommand": { color: "var(--stx-subcommand)", fontWeight: "500" },
  ".stx-option": { color: "var(--stx-option)", fontWeight: "500" },
  ".stx-local": { color: "var(--stx-local)", fontWeight: "500" },
  ".stx-global": { color: "var(--stx-global)", fontWeight: "500" },
  ".stx-macrofn": { color: "var(--stx-local)", fontWeight: "600" },
  ".stx-string": { color: "var(--stx-string)" },
  ".stx-comment": { color: "var(--stx-comment)", fontStyle: "italic" },
  ".stx-narrative": { color: "var(--stx-narrative)", fontStyle: "italic" },
  ".stx-sectionmarker": { color: "var(--stx-sectionmarker)", fontWeight: "500" },
  ".stx-continuation": { color: "var(--stx-comment)" },
  ".stx-directive": { color: "var(--stx-option)", fontWeight: "600" },
  ".stx-factor": { color: "var(--stx-factor)", fontWeight: "600" },
  // §4.3: `if`/`in` and weights are body ink at 600 — the shape of the
  // restriction is what you need to see, not a colour telling you it is a
  // keyword.
  ".stx-qualifier": { fontWeight: "600" },
  ".stx-format": { color: "var(--stx-option)" },
  ".stx-missing": { color: "var(--stx-missing)" },
  ".stx-wildcard": { fontStyle: "italic" },
  ".stx-brace": { color: "var(--stx-comment)" },
  ".stx-foreign": { color: "var(--stx-comment)" },
  ".stx-error": { textDecoration: "underline wavy var(--stx-error)" },
  // Numbers, variables and operators are deliberately absent: they are body ink
  // and inherit it. A `.stx-plain` rule that set `color: inherit` would be a
  // no-op that looked like a decision.
});
