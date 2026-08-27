/**
 * The narrative renderer — spec §24, 06 §4.9.
 *
 * # Why it builds DOM instead of HTML
 *
 * Nothing here ever assigns `innerHTML`. The text being rendered is the user's
 * own do-file, so this is not an injection story about a hostile author — it is
 * about the packaged app's Content-Security-Policy, which `cargo xtask
 * csp-check` enforces, and about the fact that a markdown renderer that emits
 * HTML strings is one bug away from executing whatever a shared `.do` contains.
 * Building nodes makes the class of bug unreachable rather than unlikely.
 *
 * # Why it is small on purpose
 *
 * Commonmark is not the goal. The goal is spec §24: prose, emphasis, headings,
 * lists, inline code and fenced code, rendered well enough that a methods
 * section reads as a methods section. Anything beyond that is a dependency, a
 * bundle and a surface, and the source is still ordinary comments in a `.do`
 * that must open in Stata.
 *
 * # The source map
 *
 * Every produced node carries `data-src`, the document offset of the line it
 * came from, so 06 §4.9's "clicking rendered prose places the caret in the
 * corresponding source line" is a lookup rather than a guess.
 */

/** One line of narrative, with the document offset it came from. */
export interface NarrativeLine {
  /** The text after the `//:` (or `*:`) prefix, with one leading space eaten. */
  readonly text: string;
  /** Document offset of the start of this line's content. */
  readonly at: number;
}

/** Strip the narrative prefix from a raw source line. `null` if it has none. */
export function stripNarrativePrefix(line: string): { text: string; skip: number } | null {
  const match = /^(\s*)(\/\/:|\*:)( ?)/.exec(line);
  if (match === null) return null;
  const skip = match[0].length;
  return { text: line.slice(skip), skip };
}

/**
 * Render narrative lines into a container.
 *
 * Block structure is decided line by line — a heading, a list item, a fence, or
 * a paragraph continuation — which is all §24 needs and is O(lines) with no
 * backtracking.
 */
export function renderNarrative(container: HTMLElement, lines: readonly NarrativeLine[]): void {
  container.replaceChildren();
  let paragraph: HTMLParagraphElement | null = null;
  let list: HTMLUListElement | HTMLOListElement | null = null;
  let fence: HTMLPreElement | null = null;

  const endParagraph = (): void => {
    paragraph = null;
  };
  const endList = (): void => {
    list = null;
  };

  for (const line of lines) {
    const text = line.text;

    if (/^\s*```/.test(text)) {
      if (fence !== null) {
        fence = null;
      } else {
        endParagraph();
        endList();
        fence = document.createElement("pre");
        fence.className = "cm-mdCode";
        fence.dataset["src"] = String(line.at);
        container.append(fence);
      }
      continue;
    }
    if (fence !== null) {
      fence.append(document.createTextNode(`${text}\n`));
      continue;
    }

    const heading = /^(#{1,6})\s+(.*)$/.exec(text);
    if (heading !== null) {
      endParagraph();
      endList();
      const level = (heading[1] as string).length;
      const el = document.createElement(`h${Math.min(level + 1, 6)}`);
      el.className = "cm-mdHeading";
      el.dataset["src"] = String(line.at);
      inline(el, heading[2] as string);
      container.append(el);
      continue;
    }

    const bullet = /^\s*[-*+]\s+(.*)$/.exec(text);
    const numbered = /^\s*\d+[.)]\s+(.*)$/.exec(text);
    if (bullet !== null || numbered !== null) {
      endParagraph();
      const ordered = numbered !== null;
      if (list === null || (list.tagName === "OL") !== ordered) {
        list = document.createElement(ordered ? "ol" : "ul");
        list.className = "cm-mdList";
        container.append(list);
      }
      const item = document.createElement("li");
      item.dataset["src"] = String(line.at);
      inline(item, (bullet?.[1] ?? numbered?.[1]) as string);
      list.append(item);
      continue;
    }

    if (text.trim() === "") {
      endParagraph();
      endList();
      continue;
    }

    endList();
    if (paragraph === null) {
      paragraph = document.createElement("p");
      paragraph.className = "cm-mdParagraph";
      paragraph.dataset["src"] = String(line.at);
      container.append(paragraph);
    } else {
      paragraph.append(document.createTextNode(" "));
    }
    inline(paragraph, text);
  }
}

/**
 * Inline spans: `**strong**`, `*em*`, `` `code` ``.
 *
 * One pass, no nesting beyond one level. Nested emphasis in a Stata comment is
 * vanishingly rare and supporting it costs a real parser.
 */
function inline(parent: HTMLElement, text: string): void {
  const pattern = /(\*\*[^*]+\*\*|\*[^*]+\*|`[^`]+`)/g;
  let last = 0;
  for (let match = pattern.exec(text); match !== null; match = pattern.exec(text)) {
    if (match.index > last) parent.append(document.createTextNode(text.slice(last, match.index)));
    const token = match[0];
    if (token.startsWith("**")) {
      const el = document.createElement("strong");
      el.textContent = token.slice(2, -2);
      parent.append(el);
    } else if (token.startsWith("`")) {
      const el = document.createElement("code");
      el.className = "cm-mdInlineCode";
      el.textContent = token.slice(1, -1);
      parent.append(el);
    } else {
      const el = document.createElement("em");
      el.textContent = token.slice(1, -1);
      parent.append(el);
    }
    last = match.index + token.length;
  }
  if (last < text.length) parent.append(document.createTextNode(text.slice(last)));
}
