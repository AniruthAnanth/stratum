/**
 * The Results pane — 06 §6.1, §9.2; spec §§4, 17, 18.
 *
 * The counter that matters here is an INTERACTION-PATH counter, not a duration
 * (ADR-017): appending a result must build one card and re-build none of the
 * ones already on screen. A pane that re-renders its scrollback on every
 * execution is the difference between a card appearing in a frame and a
 * researcher watching a 2 000-card list flicker eight hours a day.
 */

import { createSignal } from "solid-js";
import { render } from "solid-js/web";
import { afterEach, describe, expect, test } from "vitest";
import type { ResultId } from "../../ipc/hand";
import type { ResultEnvelopeView } from "../../renderers";
import { envelopeOf, payloadOfEveryKind, scenarioAEnvelopes } from "../../renderers/fixtures";
import { ResultsPane } from "./index";

const roots: (() => void)[] = [];

function mount(node: () => ReturnType<typeof ResultsPane>): HTMLElement {
  const host = document.createElement("div");
  document.body.append(host);
  roots.push(render(node, host));
  return host;
}

afterEach(() => {
  while (roots.length > 0) roots.pop()?.();
  document.body.replaceChildren();
});

const mock = scenarioAEnvelopes();
const KINDS = payloadOfEveryKind();

describe("everything goes to the scrollback, always (06 §6.1)", () => {
  test("one card per envelope, in submission order", () => {
    const host = mount(() => <ResultsPane envelopes={mock} />);
    const cards = [...host.querySelectorAll("[data-card-cmd]")].map((el) => el.textContent);
    expect(cards).toEqual([
      "sysuse auto, clear",
      "summarize price mpg",
      "regress price mpg weight foreign",
    ]);
  });

  test("every card in the pane still ends in Raw ▸ (§17)", () => {
    const host = mount(() => <ResultsPane envelopes={mock} />);
    for (const row of host.querySelectorAll("[data-card-actions]")) {
      expect([...row.querySelectorAll("button")].at(-1)?.textContent).toBe("Raw ▸");
    }
  });

  test("the empty pane says so rather than showing an empty card", () => {
    const host = mount(() => <ResultsPane envelopes={[]} />);
    expect(host.textContent).toContain("No results yet");
    expect(host.querySelector("[data-card]")).toBeNull();
  });
});

describe("COUNTER: appending a result rebuilds nothing", () => {
  test("N cards in, one new DOM subtree out, the other N untouched", () => {
    const [list, setList] = createSignal<readonly ResultEnvelopeView[]>(mock);
    const host = mount(() => <ResultsPane envelopes={list()} />);

    const before = [...host.querySelectorAll("[data-card]")];
    expect(before).toHaveLength(3);

    setList([...mock, envelopeOf(KINDS.tabulate)]);

    const after = [...host.querySelectorAll("[data-card]")];
    expect(after).toHaveLength(4);
    // Identity, not equality: the first three elements are the SAME nodes.
    for (let i = 0; i < before.length; i++) expect(after[i]).toBe(before[i]);
    expect(after[3]).not.toBe(before[2]);
  });
});

describe("classic presentation (06 §9.2)", () => {
  test("the echo and the classic output, byte for byte, with no wrapping", () => {
    const host = mount(() => <ResultsPane envelopes={mock} mode="classic" />);
    const pre = host.querySelector("[data-results-classic]");
    expect(pre?.querySelector("[data-results-raw='2']")?.textContent).toBe(mock[1]?.raw.head);
    expect(pre?.textContent).toContain(". summarize price mpg\n");
    // The echo is a distinct element so it can carry the command ink; the output
    // is one text node so a selection across it copies the original bytes.
    expect(pre?.querySelectorAll(".results__echo")).toHaveLength(3);
  });

  test("switching presentation loses nothing", () => {
    const [mode, setMode] = createSignal<"cards" | "classic">("cards");
    const host = mount(() => <ResultsPane envelopes={mock} mode={mode()} />);
    expect(host.querySelectorAll("[data-card]")).toHaveLength(3);
    setMode("classic");
    expect(host.querySelectorAll("[data-card]")).toHaveLength(0);
    expect(host.querySelector("[data-results-classic]")?.textContent).toContain("6165.257");
    setMode("cards");
    expect(host.querySelectorAll("[data-card]")).toHaveLength(3);
  });
});

describe("accessibility (06 §17)", () => {
  test("one live region per pane, polite on success and assertive on failure", () => {
    const host = mount(() => <ResultsPane envelopes={mock} />);
    const live = host.querySelector("[data-results-live]");
    expect(host.querySelectorAll("[data-results-live]")).toHaveLength(1);
    expect(live?.getAttribute("aria-live")).toBe("polite");
    expect(live?.textContent).toBe("regress finished, 0.00s");

    const failed = mount(() => <ResultsPane envelopes={[envelopeOf(KINDS.error)]} />);
    const liveFailed = failed.querySelector("[data-results-live]");
    expect(liveFailed?.getAttribute("aria-live")).toBe("assertive");
    expect(liveFailed?.textContent).toContain("failed, r(111)");
  });

  test("each card is focusable and names itself", () => {
    const host = mount(() => <ResultsPane envelopes={mock} />);
    const card = host.querySelector("[data-card]");
    expect(card?.getAttribute("tabindex")).toBe("0");
    expect(card?.getAttribute("aria-label")).toBe("Result for sysuse auto, clear");
  });
});

describe("per-result UI state is the host's, not the pane's", () => {
  test("stale is applied to exactly the result the host names", () => {
    const staleFor = 2 as unknown as ResultId;
    const host = mount(() => (
      <ResultsPane
        envelopes={mock}
        ui={(id) =>
          id === staleFor
            ? { stale: { upstream: "line 1 — sysuse auto, clear", because: "dataset D17 → D19" } }
            : undefined
        }
      />
    ));
    const stale = host.querySelectorAll("[data-card][data-stale]");
    expect(stale).toHaveLength(1);
    expect(stale[0]?.querySelector("[data-card-cmd]")?.textContent).toBe("summarize price mpg");
    expect(stale[0]?.querySelector("[data-card-stale]")?.textContent).toContain(
      "sysuse auto, clear",
    );
  });
});
