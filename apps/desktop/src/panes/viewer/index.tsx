/**
 * The Viewer — 06 §8.3 (the Classic toolbar's second button) and §9.8.
 *
 * Stata's Viewer is where `help`, `view` and `search` land, and a classic user
 * reaches it constantly. Two decisions make it a small file:
 *
 *  * **It is a log surface, not a document viewer.** 06 §9.2's rule that SMCL is
 *    translated to styled runs *in Rust* applies to help topics exactly as it
 *    applies to results, so the Viewer mounts the same {@link LogView} over the
 *    same {@link LogWindow} the Results pane uses. "The Results pane, the Viewer
 *    and a detached log window are the same pixels" is a property of there being
 *    one component, not of three of them agreeing.
 *  * **Opening a topic is a command.** `help regress` is a Stata command; typing
 *    it in the Viewer's topic field submits it through the same
 *    `submitCommand` as everything else, so it appears in History with its `_rc`
 *    and in the log. A private "fetch help" path would be the one place in
 *    Classic where what the user did is not recoverable from the log.
 *
 * Back/Forward are the Viewer's own — [GSM] 2 describes them as history over the
 * topics you have visited — and they re-issue the command rather than caching a
 * rendered page, which is what keeps a `help` after an `ado` update honest.
 */

import { type JSX, Show, createSignal } from "solid-js";
import { render } from "solid-js/web";
import { submitCommand } from "../../commandbar/submit";
import { registerPane } from "../../dock/panes";
import { LogView, createLogWindow } from "../../log/view";
import type { LogWindow, LogWindowOptions } from "../../log/window";
import { Icon, PaneHeader } from "../../ui";

import "./viewer.css";

export interface ViewerCounters {
  /** Topics opened. Back and Forward each add one — they re-issue. */
  opens: number;
  /** Back/Forward presses that moved within the stack. */
  navigations: number;
}

const ZERO: ViewerCounters = { opens: 0, navigations: 0 };
export const viewerCounters: ViewerCounters = { ...ZERO };
export function resetViewerCounters(): void {
  Object.assign(viewerCounters, ZERO);
}

/** How a topic becomes a command. `help` unless the user typed a full command. */
export function commandForTopic(topic: string): string | undefined {
  const text = topic.trim();
  if (text === "") return undefined;
  // `view browse …`, `search …`, `net …` and `help …` are all Viewer commands in
  // Stata; anything else is a topic and gets `help` put in front of it. Checking
  // the first word rather than parsing is deliberate — the parser is W04's and
  // this is a routing decision, not a syntax decision.
  const verb = text.split(/\s+/u)[0] ?? "";
  return ["help", "view", "search", "net", "ssc", "about", "which"].includes(verb)
    ? text
    : `help ${text}`;
}

export interface ViewerPaneProps {
  /** A pre-built window, so a host can feed one Viewer from one session. */
  window?: LogWindow;
  /** Its change signal. Required with `window`: the view redraws on this. */
  revision?: () => number;
  options?: LogWindowOptions;
  /** Opening seam. Defaults to submitting the command the topic maps to. */
  onOpen?: (command: string) => void;
}

export function ViewerPane(props: ViewerPaneProps): JSX.Element {
  const created = createLogWindow(props.options ?? {});
  const win = (): LogWindow => props.window ?? created.window;
  const revision = (): number =>
    props.window === undefined ? created.revision() : (props.revision?.() ?? 0);

  const [topic, setTopic] = createSignal("");
  const [stack, setStack] = createSignal<readonly string[]>([]);
  const [at, setAt] = createSignal(-1);

  const current = (): string | undefined => stack()[at()];

  const issue = (command: string): void => {
    viewerCounters.opens += 1;
    if (props.onOpen !== undefined) props.onOpen(command);
    else void submitCommand(command, "menu");
  };

  const open = (text: string): void => {
    const command = commandForTopic(text);
    if (command === undefined) return;
    // A new topic truncates the forward history, which is what every Back/
    // Forward stack in the product does and what the user's hand expects.
    setStack([...stack().slice(0, at() + 1), command]);
    setAt(stack().length - 1);
    issue(command);
  };

  const go = (delta: -1 | 1): void => {
    const next = at() + delta;
    const command = stack()[next];
    if (command === undefined) return;
    setAt(next);
    viewerCounters.navigations += 1;
    issue(command);
  };

  return (
    <section class="viewer" data-pane="viewer">
      <PaneHeader
        title="Viewer"
        actions={
          <div class="viewer__nav">
            <button
              type="button"
              class="viewer__button"
              aria-label="Back"
              data-viewer-back
              disabled={at() <= 0}
              onClick={() => go(-1)}
            >
              ◀
            </button>
            <button
              type="button"
              class="viewer__button"
              aria-label="Forward"
              data-viewer-forward
              disabled={at() >= stack().length - 1}
              onClick={() => go(1)}
            >
              ▶
            </button>
          </div>
        }
      />

      <form
        class="viewer__topic"
        onSubmit={(event) => {
          event.preventDefault();
          open(topic());
          setTopic("");
        }}
      >
        <Icon name="search" />
        <input
          class="viewer__query"
          type="search"
          value={topic()}
          placeholder="help regress"
          aria-label="Viewer topic"
          data-viewer-topic
          onInput={(event) => setTopic(event.currentTarget.value)}
        />
      </form>

      <Show when={current() !== undefined}>
        <p class="viewer__current" data-viewer-current>
          {current()}
        </p>
      </Show>

      <div class="viewer__body">
        <LogView window={win()} revision={revision} label="Viewer" />
      </div>
    </section>
  );
}

/** Registers the pane with W12's dock. Returns the disposer. */
export function registerViewerPane(props: ViewerPaneProps = {}): () => void {
  return registerPane(
    "viewer",
    (host, register) => {
      register(render(() => <ViewerPane {...props} />, host));
    },
    "Viewer",
  );
}
