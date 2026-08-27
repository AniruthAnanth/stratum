/**
 * Contextual help — spec §2 (`help` is how Stata users read documentation) and
 * 06 §4.1.
 *
 * Two affordances over one resolver:
 *
 *  * `F1` on a command runs `help <command>` in the engine, which is what a
 *    Stata user's hands already do;
 *  * hovering a token shows the segmenter's own diagnostics for that position,
 *    because the most useful thing to say about a token that is underlined is
 *    why it is underlined.
 *
 * The hover deliberately does NOT show a variable's label. A11 is explicit that
 * a label is not detail text, and a tooltip that shows one on hover teaches the
 * user to trust a display the completion popup will not repeat.
 */

import type { EditorState } from "@codemirror/state";
import { EditorView, hoverTooltip } from "@codemirror/view";
import type { Tooltip } from "@codemirror/view";
import { blockAt } from "../blocks/blockField";
import { stateSegmenter } from "../blocks/blockField";
import { classifyRegion } from "./parser";
import type { TaggedSpan } from "./parser";

/** A help topic — what `help <topic>` would be asked for. */
export interface HelpTopic {
  /** The topic string, e.g. `regress`. */
  readonly topic: string;
  /** The span the topic was read from, for highlighting. */
  readonly from: number;
  readonly to: number;
}

/**
 * The help topic at a position, or `null`.
 *
 * Resolution is by TAG, not by word: `regress` in command position is the
 * `regress` help file, and `regress` as a variable name is not a help topic at
 * all. That distinction is only available because the classifier ran.
 */
export function helpTopicAt(state: EditorState, pos: number): HelpTopic | null {
  const seg = stateSegmenter(state);
  const block = blockAt(state, pos);
  if (seg === null || block === null) return null;

  const from = block.outerFrom;
  const to = Math.min(block.outerTo, state.doc.length);
  const spans: TaggedSpan[] = [];
  classifyRegion(
    state.doc.sliceString(from, to),
    from,
    block,
    seg.tokens(from, to),
    0,
    Number.MAX_SAFE_INTEGER,
    spans,
  );

  let best: TaggedSpan | null = null;
  for (const span of spans) {
    if (span.from > pos || span.to < pos) continue;
    if (span.tag === "CommandName" || span.tag === "PrefixCommand" || span.tag === "Subcommand") {
      best = span;
    }
  }
  if (best === null) return null;
  return {
    topic: state.doc.sliceString(best.from, best.to),
    from: best.from,
    to: best.to,
  };
}

/** Runs `help <topic>`. W16 owns the Viewer pane; this is the seam into it. */
export type HelpOpener = (topic: string) => void;

let opener: HelpOpener | null = null;

export function setHelpOpener(next: HelpOpener | null): void {
  opener = next;
}

export function openHelpAt(state: EditorState, pos: number): boolean {
  const topic = helpTopicAt(state, pos);
  if (topic === null || opener === null) return false;
  opener(topic.topic);
  return true;
}

/**
 * Hover: the segmenter's diagnostics for the position under the pointer.
 *
 * `diagnostics()` drains splice faults, so it is called once per hover and never
 * on the typing path — a hover is a deliberate, low-frequency gesture and this
 * is the one place in the editor where a slightly expensive call is correct.
 */
export const stataHoverHelp = hoverTooltip((view, pos): Tooltip | null => {
  const seg = stateSegmenter(view.state);
  if (seg === null) return null;

  const diagnostics = seg.raw().lints();
  const hit = diagnostics.find((d) => d.span !== null && d.span.start <= pos && pos <= d.span.end);
  const topic = helpTopicAt(view.state, pos);
  if (hit === undefined && topic === null) return null;

  const from = hit?.span?.start ?? topic?.from ?? pos;
  const to = hit?.span?.end ?? topic?.to ?? pos;

  return {
    pos: from,
    end: to,
    above: true,
    create() {
      const dom = document.createElement("div");
      dom.className = "cm-helpTip";
      if (hit !== undefined) {
        const line = document.createElement("div");
        line.className = "cm-helpDiagnostic";
        line.dataset["severity"] = hit.severity;
        line.textContent = `${hit.code}: ${hit.message}`;
        dom.append(line);
        for (const note of hit.notes) {
          const el = document.createElement("div");
          el.className = "cm-helpNote";
          el.textContent = note;
          dom.append(el);
        }
      }
      if (topic !== null) {
        const el = document.createElement("div");
        el.className = "cm-helpTopic";
        el.textContent = `help ${topic.topic}`;
        dom.append(el);
      }
      return { dom };
    },
  };
});

export const helpTheme = EditorView.baseTheme({
  ".cm-helpTip": {
    font: "var(--fs-small, 12px)/var(--lh-small, 16px) var(--font-sans)",
    padding: "var(--sp-6, 6px) var(--sp-8, 8px)",
    maxWidth: "52ch",
    background: "var(--overlay)",
    color: "var(--text-body)",
    border: "var(--hairline, 1px) solid var(--border-strong)",
  },
  ".cm-helpDiagnostic[data-severity='error']": { color: "var(--state-failed)" },
  ".cm-helpDiagnostic[data-severity='warning']": { color: "var(--state-stale)" },
  ".cm-helpNote, .cm-helpTopic": { color: "var(--text-meta)" },
});
