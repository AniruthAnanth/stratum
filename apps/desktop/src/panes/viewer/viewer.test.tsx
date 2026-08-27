/**
 * The Viewer — 06 §9.8's toolbar button, over the §9.2 log surface.
 *
 * The property worth testing is the one that keeps Classic honest: opening a
 * help topic is a *command*, so it is in History and in the log like everything
 * else. A Viewer with a private fetch path would be the one surface in this
 * layout whose contents cannot be explained from the log.
 */

import { render } from "solid-js/web";
import { afterEach, beforeEach, describe, expect, test } from "vitest";
import { resetCommandBarHandle } from "../../commandbar/handle";
import { recordedSubmissions, resetSubmitState } from "../../commandbar/submit";
import { ViewerPane, commandForTopic, resetViewerCounters, viewerCounters } from "./index";

const roots: (() => void)[] = [];

function mount(): HTMLElement {
  const host = document.createElement("div");
  document.body.append(host);
  roots.push(render(() => <ViewerPane />, host));
  return host;
}

function ask(host: HTMLElement, text: string): void {
  const input = host.querySelector<HTMLInputElement>("[data-viewer-topic]");
  if (input === null) throw new Error("no topic field");
  input.value = text;
  input.dispatchEvent(new Event("input", { bubbles: true }));
  const form = input.closest("form");
  form?.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true }));
}

beforeEach(() => {
  resetSubmitState();
  resetViewerCounters();
});

afterEach(() => {
  while (roots.length > 0) roots.pop()?.();
  document.body.replaceChildren();
  resetCommandBarHandle();
});

describe("a topic is a command", () => {
  test("a bare topic gets `help` in front of it; a command is left alone", () => {
    expect(commandForTopic("regress")).toBe("help regress");
    expect(commandForTopic("  regress  ")).toBe("help regress");
    expect(commandForTopic("help regress")).toBe("help regress");
    expect(commandForTopic("search heteroskedasticity")).toBe("search heteroskedasticity");
    expect(commandForTopic("")).toBeUndefined();
  });

  test("submitting the topic field runs the command", async () => {
    const host = mount();
    ask(host, "regress");
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(recordedSubmissions().map((s) => s.text)).toEqual(["help regress"]);
    expect(host.querySelector("[data-viewer-current]")?.textContent).toBe("help regress");
  });
});

describe("Back and Forward", () => {
  test("they re-issue rather than replaying a cached page", async () => {
    const host = mount();
    ask(host, "regress");
    ask(host, "summarize");
    await new Promise((resolve) => setTimeout(resolve, 0));

    const back = host.querySelector<HTMLButtonElement>("[data-viewer-back]");
    const forward = host.querySelector<HTMLButtonElement>("[data-viewer-forward]");
    expect(forward?.disabled).toBe(true);

    back?.click();
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(recordedSubmissions().map((s) => s.text)).toEqual([
      "help regress",
      "help summarize",
      "help regress",
    ]);
    expect(viewerCounters.navigations).toBe(1);
    expect(viewerCounters.opens).toBe(3);
  });

  test("Back is disabled at the start of the stack", () => {
    const host = mount();
    expect(host.querySelector<HTMLButtonElement>("[data-viewer-back]")?.disabled).toBe(true);
    ask(host, "regress");
    expect(host.querySelector<HTMLButtonElement>("[data-viewer-back]")?.disabled).toBe(true);
    ask(host, "summarize");
    expect(host.querySelector<HTMLButtonElement>("[data-viewer-back]")?.disabled).toBe(false);
  });

  test("a new topic truncates the forward history", async () => {
    const host = mount();
    ask(host, "regress");
    ask(host, "summarize");
    host.querySelector<HTMLElement>("[data-viewer-back]")?.click();
    ask(host, "tabulate");
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(host.querySelector<HTMLButtonElement>("[data-viewer-forward]")?.disabled).toBe(true);
    expect(recordedSubmissions().at(-1)?.text).toBe("help tabulate");
  });
});

describe("it is the log surface, not a second renderer", () => {
  test("the pane mounts one scrollback view with its own label", () => {
    const host = mount();
    const view = host.querySelector("[data-log-view]");
    expect(view).not.toBeNull();
    expect(host.querySelector('[data-log-viewport][aria-label="Viewer"]')).not.toBeNull();
    // Wrapping off, as in Stata and as in the Results pane.
    expect(view?.getAttribute("data-wrap")).toBe("off");
  });
});
