/**
 * The Command pane — 06 §9.1, §10; spec §10.
 *
 * > Single logical input, 3 lines tall, auto-grows to 8. It is a **CodeMirror
 * > instance running the same Stata language mode** as the editor, so
 * > highlighting, bracket matching and completion are identical (Stata does this
 * > too).
 *
 * The manual agrees, which is why this is not a `<textarea>`: "The Command
 * window also uses the same syntax highlighting as the Do-file Editor"
 * ([GSM] 2). So the extension list below is W13's language mode — `blockField`,
 * `stataHighlight`, `stataHighlightTheme` — and nothing from W13's *editor*
 * chrome: no line numbers, no block gutter, no result widgets. A Command window
 * with a gutter would be the "Jupyter in a desktop shell" §39 forbids.
 *
 * # Keys, and who owns them
 *
 * Every binding here goes into a normal-precedence CM6 keymap that runs only
 * after the keyboard authority (`keys/authority.ts`) has declined the event.
 * The authority is `Prec.highest` and it decides first; a keystroke bound to a
 * registered, enabled command never reaches this map. So:
 *
 *  * under the **Stata preset**, `PageUp`/`PageDown`/`Tab` resolve through the
 *    trie to `history.previous` / `history.next` / `commandbar.complete`, which
 *    this unit registers in `commands.ts` and which call the same functions;
 *  * under **Modern** and **VS Code**, which bind none of those, the keymap
 *    below serves them — because 06 §9.1 calls PgUp/PgDn "non-negotiable" and a
 *    preset the user picked for the editor must not take the Command window's
 *    history away.
 *
 * `Mod+.` is the same shape: the presets bind it to `run.break`, whose W13
 * descriptor is enabled only when an editor is focused. In Classic the do-file
 * editor is a separate window (06 §9.6), so in the main window that verb is
 * disabled, the authority reports "ignored", and Break lands here — which is
 * exactly 06 §9.1's Break chord serving the Command pane. See `commands.ts` for
 * the interrupt sink, and `interrupt.ts` for why the per-platform spelling of
 * that chord is the host's to hand out and never this tree's to write down.
 */

import { EditorSelection, EditorState } from "@codemirror/state";
import { EditorView, drawSelection, highlightSpecialChars, keymap } from "@codemirror/view";
import { For, type JSX, Show, createSignal, onCleanup, onMount } from "solid-js";
import { blockField } from "../editor/blocks/blockField";
import { segmenterFacet } from "../editor/blocks/segmenter";
import { EditorSegmenter } from "../editor/blocks/segmenter";
import { stataHighlight, stataHighlightTheme } from "../editor/lang/highlight";
import { setKeyContext } from "../keys/context";
import { editorKeymapExtension } from "../keys/editor";
import type { StratumSegmenter } from "../wasm/types";
import { completeAt } from "./complete";
import { functionKeyAction } from "./fkeys";
import { type CommandBarHandle, setCommandBarHandle } from "./handle";
import { requestBreak } from "./interrupt";
import { addAsNewBlock, addToDoFile } from "./promote";
import { recallNext, recallPrevious, resetRecall } from "./recall";
import { lastSubmission, submitCommand } from "./submit";

import "./commandbar.css";

/** 06 §9.1: three lines tall, auto-grows to eight. */
export const MIN_ROWS = 3;
export const MAX_ROWS = 8;
/** 06 §10: the ghost row lives six seconds, or until the next keystroke. */
export const GHOST_MS = 6_000;

export interface CommandBarProps {
  /** Attached once wasm is ready; the bar is usable before that. */
  segmenter?: StratumSegmenter;
  /** Classic docks it as a pane; Modern docks it at the foot of the editor. */
  variant?: "pane" | "docked" | "overlay";
}

interface Suggestion {
  readonly items: readonly string[];
  readonly target: "variable" | "file";
}

export function CommandBar(props: CommandBarProps): JSX.Element {
  let host: HTMLDivElement | undefined;
  let view: EditorView | undefined;

  const [suggestions, setSuggestions] = createSignal<Suggestion | undefined>(undefined);
  const [ghost, setGhost] = createSignal<ReturnType<typeof lastSubmission>>(undefined);
  let ghostTimer: ReturnType<typeof setTimeout> | undefined;

  const text = (): string => view?.state.doc.toString() ?? "";

  const setText = (next: string): void => {
    if (view === undefined) return;
    view.dispatch({
      changes: { from: 0, to: view.state.doc.length, insert: next },
      selection: EditorSelection.cursor(next.length),
    });
  };

  const clearGhost = (): void => {
    if (ghostTimer !== undefined) clearTimeout(ghostTimer);
    ghostTimer = undefined;
    setGhost(undefined);
  };

  const showGhost = (): void => {
    setGhost(lastSubmission());
    if (ghostTimer !== undefined) clearTimeout(ghostTimer);
    ghostTimer = setTimeout(clearGhost, GHOST_MS);
  };

  /** Enter. Empty lines are ignored, as Stata's Command window ignores them. */
  const submit = (): boolean => {
    const command = text();
    setSuggestions(undefined);
    if (command.trim() === "") return true;
    setText("");
    resetRecall("");
    void submitCommand(command, "commandbar").then((outcome) => {
      if (outcome !== undefined) showGhost();
    });
    return true;
  };

  const stepHistory = (direction: -1 | 1): boolean => {
    const next = direction === -1 ? recallPrevious(text()) : recallNext();
    if (next === undefined) return true; // consumed: PgUp at the oldest does nothing
    setText(next);
    setSuggestions(undefined);
    return true;
  };

  /**
   * Tab. Inserts the longest common prefix and, when several names match,
   * leaves the list up so further typing narrows it (06 §9.1, [U] 10.6).
   */
  const complete = (): boolean => {
    if (view === undefined) return true;
    const caret = view.state.selection.main.head;
    const outcome = completeAt(view.state.doc.toString(), caret);
    if (outcome === null) {
      setSuggestions(undefined);
      return true;
    }
    if (outcome.insert.length > outcome.prefix.length) {
      view.dispatch({
        changes: { from: outcome.from, to: outcome.to, insert: outcome.insert },
        selection: EditorSelection.cursor(outcome.from + outcome.insert.length),
      });
    }
    setSuggestions(
      outcome.ambiguous ? { items: outcome.matches, target: outcome.target } : undefined,
    );
    return true;
  };

  const functionKey = (n: number): boolean => {
    const action = functionKeyAction(n);
    if (action === undefined || view === undefined) return false; // unbound: fall through
    const at = view.state.selection.main.head;
    view.dispatch({
      changes: { from: at, to: view.state.selection.main.anchor, insert: action.insert },
      selection: EditorSelection.cursor(at + action.insert.length),
    });
    if (action.submit) submit();
    return true;
  };

  onMount(() => {
    if (host === undefined) return;
    const seg = props.segmenter === undefined ? undefined : new EditorSegmenter(props.segmenter);

    view = new EditorView({
      parent: host,
      state: EditorState.create({
        doc: "",
        extensions: [
          ...(seg === undefined ? [] : [segmenterFacet.of(seg)]),
          blockField,
          stataHighlight,
          stataHighlightTheme,
          highlightSpecialChars(),
          drawSelection(),
          // The one keyboard authority decides first; see the file header.
          editorKeymapExtension(),
          keymap.of([
            { key: "Enter", run: submit },
            // A multi-line command — `#delimit ;` style paste (06 §9.1).
            { key: "Shift-Enter", run: (v) => insertNewline(v) },
            { key: "PageUp", run: () => stepHistory(-1) },
            { key: "PageDown", run: () => stepHistory(1) },
            { key: "Tab", run: complete },
            // [U] 10.3: "Esc … clears the Command window."
            {
              key: "Escape",
              run: () => {
                if (suggestions() !== undefined) {
                  setSuggestions(undefined);
                  return true;
                }
                setText("");
                resetRecall("");
                return true;
              },
            },
            {
              key: "Mod-.",
              run: () => {
                requestBreak();
                return true;
              },
            },
            ...[1, 2, 3, 4, 5, 6, 7, 8, 9, 10].map((n) => ({
              key: `F${n}`,
              run: () => functionKey(n),
            })),
          ]),
          EditorView.lineWrapping,
          EditorView.updateListener.of((update) => {
            if (update.docChanged) {
              // Any edit abandons an in-progress recall, so PgUp always starts
              // from the newest entry — [U] 10.5's behaviour.
              resetRecall(update.state.doc.toString());
              clearGhost();
            }
            if (update.focusChanged) {
              setKeyContext({ commandBarFocus: update.view.hasFocus });
            }
          }),
          EditorView.theme({
            "&": { minHeight: `calc(${MIN_ROWS} * var(--lh-code))` },
            ".cm-scroller": {
              fontFamily: "var(--font-mono)",
              fontSize: "var(--fs-code)",
              lineHeight: "var(--lh-code)",
              maxHeight: `calc(${MAX_ROWS} * var(--lh-code))`,
              overflowY: "auto",
            },
            ".cm-content": { padding: "0" },
            "&.cm-focused": { outline: "none" },
          }),
        ],
      }),
    });

    const handle: CommandBarHandle = {
      text,
      replace: (next) => {
        setText(next);
        resetRecall(next);
      },
      insertAtCaret: (insert) => {
        if (view === undefined) return;
        const at = view.state.selection.main.head;
        view.dispatch({
          changes: { from: at, to: view.state.selection.main.anchor, insert },
          selection: EditorSelection.cursor(at + insert.length),
        });
      },
      caret: () => view?.state.selection.main.head ?? 0,
      focus: () => view?.focus(),
      hasFocus: () => view?.hasFocus ?? false,
      clear: () => setText(""),
    };
    setCommandBarHandle(handle);

    onCleanup(() => {
      clearGhost();
      setCommandBarHandle(undefined);
      setKeyContext({ commandBarFocus: false });
      view?.destroy();
      view = undefined;
    });
  });

  return (
    <section class="cmdbar" data-pane="commandbar" data-variant={props.variant ?? "pane"}>
      <div class="cmdbar__row">
        {/* 06 §10: "Prompt is a `.` in `--text-meta` — a direct quotation of
            Stata, and the cheapest possible signal of what this input is." */}
        <span class="cmdbar__prompt" aria-hidden="true">
          .
        </span>
        <div class="cmdbar__input" ref={host} data-commandbar-input />
      </div>

      <Show when={suggestions()}>
        {(list) => (
          <ul class="cmdbar__list" data-commandbar-suggestions data-target={list().target}>
            <For each={list().items.slice(0, 40)}>
              {(item) => <li class="cmdbar__item">{item}</li>}
            </For>
            <Show when={list().items.length > 40}>
              <li class="cmdbar__more t-micro">{`${list().items.length - 40} more`}</li>
            </Show>
          </ul>
        )}
      </Show>

      {/* 06 §10's ghost row. `duration` is RECORDED, never asserted (ADR-017). */}
      <Show when={ghost()}>
        {(done) => (
          <div class="cmdbar__ghost" data-commandbar-ghost>
            <span class={done().rc === 0 ? "cmdbar__ok" : "cmdbar__bad"}>
              {done().rc === 0 ? "✓" : `r(${done().rc});`}
            </span>
            <Show when={done().durationMs !== undefined}>
              <span class="cmdbar__time t-micro">{`${((done().durationMs ?? 0) / 1000).toFixed(2)}s`}</span>
            </Show>
            <button
              type="button"
              class="cmdbar__promote"
              data-promote="do-file"
              onClick={() => addToDoFile(done().text)}
            >
              ↑ Add to do-file
            </button>
            <button
              type="button"
              class="cmdbar__promote"
              data-promote="block"
              onClick={() => addAsNewBlock(done().text)}
            >
              ⌥↑ Add as new block
            </button>
          </div>
        )}
      </Show>
    </section>
  );
}

/** `Shift+Enter`. Kept out of the keymap literal so the intent has a name. */
function insertNewline(view: EditorView): boolean {
  const at = view.state.selection.main.head;
  view.dispatch({
    changes: { from: at, to: view.state.selection.main.anchor, insert: "\n" },
    selection: EditorSelection.cursor(at + 1),
  });
  return true;
}
