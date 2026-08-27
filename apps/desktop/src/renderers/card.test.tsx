/**
 * The card shell's acceptance — W14's first four bullets.
 *
 * Each `describe` below is one bullet, quoted where it starts. The tests are
 * written against the SHELL over every `ResultPayload` variant rather than
 * against each renderer, because that is what the bullets actually claim: not
 * "the summarize card has a Raw button" but "every card has `Raw ▸` in the same
 * position, always".
 */

import { render } from "solid-js/web";
import { afterEach, describe, expect, test } from "vitest";
import { orderedActions, rawOutputRepairs, resetRawOutputRepairs } from "./actions";
import { announcement, cardState, middleEllipsis } from "./card";
import { envelopeOf, payloadOfEveryKind } from "./fixtures";
import { durationLabel, readout } from "./readout";
import { HANDLED_KINDS, ResultCard } from "./registry";
import { type CardActionView, PAYLOAD_KINDS, type PayloadKind } from "./types";

const roots: (() => void)[] = [];

function mount(node: () => ReturnType<typeof ResultCard>): HTMLElement {
  const host = document.createElement("div");
  document.body.append(host);
  roots.push(render(node, host));
  return host;
}

afterEach(() => {
  while (roots.length > 0) roots.pop()?.();
  document.body.replaceChildren();
});

const KINDS = payloadOfEveryKind();

describe("every ResultPayload variant is handled", () => {
  test("the dispatch table covers §5.2 exactly, Unknown included", () => {
    expect([...HANDLED_KINDS].sort()).toEqual([...PAYLOAD_KINDS].sort());
    expect(Object.keys(KINDS).sort()).toEqual([...PAYLOAD_KINDS].sort());
    expect(PAYLOAD_KINDS).toHaveLength(10);
  });
});

describe("'Every card has Raw ▸ in the same position, always' (§17)", () => {
  test.each(PAYLOAD_KINDS)("%s renders Raw ▸ last", (kind: PayloadKind) => {
    const host = mount(() => <ResultCard envelope={envelopeOf(KINDS[kind])} />);
    const row = host.querySelector("[data-card-actions]");
    expect(row).not.toBeNull();
    const labels = [...(row?.querySelectorAll("button") ?? [])].map((b) => b.textContent);
    expect(labels.at(-1)).toBe("Raw ▸");
    expect(labels.filter((l) => l === "Raw ▸")).toHaveLength(1);
  });

  test("an envelope whose actions list is EMPTY still gets it", () => {
    resetRawOutputRepairs();
    const host = mount(() => <ResultCard envelope={envelopeOf(KINDS.unknown, [])} />);
    expect(host.querySelector('[data-action="raw_output"]')?.textContent).toBe("Raw ▸");
    // The repair is counted rather than hidden: an engine that omits a MANDATORY
    // field is a bug worth seeing, even though the card recovers.
    expect(rawOutputRepairs()).toBe(1);
  });

  test("an engine that sends it FIRST still gets it last", () => {
    const actions: CardActionView[] = [
      { action: "raw_output" },
      { action: "copy_table" },
      { action: "export", formats: ["csv"] },
    ];
    expect(orderedActions(actions).map((a) => a.action)).toEqual([
      "copy_table",
      "export",
      "raw_output",
    ]);
  });

  test("clicking it discloses the classic text, and again hides it", () => {
    const host = mount(() => <ResultCard envelope={envelopeOf(KINDS.summarize)} />);
    const button = host.querySelector<HTMLButtonElement>('[data-action="raw_output"]');
    expect(host.querySelector("[data-raw-text]")).toBeNull();
    button?.click();
    expect(host.querySelector("[data-raw-text]")?.textContent).toBe(
      envelopeOf(KINDS.summarize).raw.head,
    );
    expect(button?.getAttribute("aria-expanded")).toBe("true");
    button?.click();
    expect(host.querySelector("[data-raw-text]")).toBeNull();
  });
});

describe("'Card anatomy is identical across renderers'", () => {
  const SLOTS = [
    "data-card-rail",
    "data-card-glyph",
    "data-card-cmd",
    "data-card-readout",
    "data-card-body",
    "data-card-actions",
  ] as const;

  test.each(PAYLOAD_KINDS)("%s draws all six slots in one order", (kind: PayloadKind) => {
    const host = mount(() => <ResultCard envelope={envelopeOf(KINDS[kind])} />);
    const card = host.querySelector("[data-card]");
    expect(card).not.toBeNull();

    const found = SLOTS.map((slot) => card?.querySelector(`[${slot}]`) ?? null);
    expect(found.every((el) => el !== null)).toBe(true);

    // Document order, not merely presence. `compareDocumentPosition` returns
    // FOLLOWING (4) when the second argument comes after the first.
    for (let i = 1; i < found.length; i++) {
      const previous = found[i - 1];
      const current = found[i];
      if (previous === null || previous === undefined) throw new Error("missing slot");
      if (current === null || current === undefined) throw new Error("missing slot");
      expect(previous.compareDocumentPosition(current) & Node.DOCUMENT_POSITION_FOLLOWING).toBe(
        Node.DOCUMENT_POSITION_FOLLOWING,
      );
    }
  });

  test("the state readout is `E41 · D17 · 0.08s` (spec §13)", () => {
    const host = mount(() => <ResultCard envelope={envelopeOf(KINDS.summarize)} />);
    const text = host.querySelector("[data-card-readout]")?.textContent;
    expect(text).toBe("E41·D17·0.08s");
    expect(host.querySelector("[data-card-readout]")?.getAttribute("aria-label")).toBe(
      "execution 41, dataset state 17, 0.08s",
    );
  });

  test("the echoed command is the cmdline, with the whole of it on hover", () => {
    const long = `regress price ${"very_long_variable_name ".repeat(20)}foreign`;
    const host = mount(() => <ResultCard envelope={envelopeOf(KINDS.estimation, [], long)} />);
    const echo = host.querySelector("[data-card-cmd]");
    expect(echo?.getAttribute("title")).toBe(long);
    expect(echo?.textContent).toContain("…");
    expect(echo?.textContent?.length).toBe(120);
  });
});

describe("'The action row is data, not markup' (A22)", () => {
  test("the row is exactly `envelope.actions`, in order, plus the mandatory Raw", () => {
    const actions: CardActionView[] = [
      { action: "copy_table" },
      { action: "plot_coefficients" },
      { action: "ai_explain" },
      { action: "raw_output" },
    ];
    const host = mount(() => <ResultCard envelope={envelopeOf(KINDS.estimation, actions)} />);
    const tags = [...host.querySelectorAll("[data-action]")].map((b) =>
      b.getAttribute("data-action"),
    );
    expect(tags).toEqual(["copy_table", "plot_coefficients", "ai_explain", "raw_output"]);
  });

  test("an action the build does not implement is not drawn — because it is not sent", () => {
    // The regress card's own bullet: a build without `margins` sends no
    // `RunMargins`, so no button exists to fail on click.
    const host = mount(() => (
      <ResultCard
        envelope={envelopeOf(KINDS.estimation, [
          { action: "copy_table" },
          { action: "raw_output" },
        ])}
      />
    ));
    expect(host.querySelector('[data-action="run_margins"]')).toBeNull();
    expect(host.querySelector('[data-action="plot_coefficients"]')).toBeNull();
    expect(host.textContent).not.toContain("Run margins");
  });

  test("clicks report the whole action, payload included", () => {
    const seen: CardActionView[] = [];
    const actions: CardActionView[] = [
      { action: "export", formats: ["csv", "tex", "md"] },
      { action: "raw_output" },
    ];
    const host = mount(() => (
      <ResultCard envelope={envelopeOf(KINDS.summarize, actions)} onAction={(a) => seen.push(a)} />
    ));
    host.querySelector<HTMLButtonElement>('[data-action="export"]')?.click();
    expect(seen).toEqual([{ action: "export", formats: ["csv", "tex", "md"] }]);
  });
});

describe("'Cards appear with zero animation'", () => {
  test("nothing on the mount path is transitioned or animated", () => {
    const host = mount(() => <ResultCard envelope={envelopeOf(KINDS.summarize)} />);
    const card = host.querySelector<HTMLElement>("[data-card]");
    expect(card).not.toBeNull();
    // jsdom applies no stylesheet, so this asserts the far stronger property:
    // the component sets no inline transition/animation and adds no enter class.
    expect(card?.style.transition).toBe("");
    expect(card?.style.animation).toBe("");
    expect(card?.className).toBe("card");
    expect(host.querySelector(".spinner, [role='progressbar']")).toBeNull();
  });

  test("running draws a hairline on the rail, never a spinner", () => {
    const host = mount(() => (
      <ResultCard envelope={envelopeOf(KINDS.log)} ui={{ running: true, progress: 0.5 }} />
    ));
    const hairline = host.querySelector<HTMLElement>("[data-running-hairline]");
    expect(hairline).not.toBeNull();
    expect(hairline?.style.height).toBe("50%");
    expect(host.querySelector("[data-card]")?.getAttribute("data-state")).toBe("running");
  });

  test("progressless running is the indeterminate shuttle on the same hairline", () => {
    const host = mount(() => (
      <ResultCard envelope={envelopeOf(KINDS.log)} ui={{ running: true }} />
    ));
    expect(host.querySelector("[data-running-hairline]")?.hasAttribute("data-indeterminate")).toBe(
      true,
    );
  });
});

describe("stale rendering (spec §13)", () => {
  const stale = { upstream: "line 12 — drop if missing(income)", because: "code changed" };

  test("the rail dashes, the body dims, the header does not, and the reason names the block", () => {
    const host = mount(() => <ResultCard envelope={envelopeOf(KINDS.summarize)} ui={{ stale }} />);
    const card = host.querySelector<HTMLElement>("[data-card]");
    expect(card?.hasAttribute("data-stale")).toBe(true);
    expect(card?.getAttribute("data-state")).toBe("stale");

    const strip = host.querySelector("[data-card-stale]");
    expect(strip?.textContent).toContain("drop if missing(income)");
    expect(strip?.textContent).toContain("code changed");

    // The .62/1.0 split is a stylesheet rule, and it is asserted as one in
    // `contract.test.ts`; here we assert the hooks it keys on exist.
    expect(host.querySelector("[data-card-body]")).not.toBeNull();
    expect(host.querySelector(".card__header")).not.toBeNull();
  });

  test("nothing in the shell can re-run a stale block", () => {
    let ran = 0;
    const host = mount(() => (
      <ResultCard
        envelope={envelopeOf(KINDS.summarize)}
        ui={{ stale }}
        onAction={() => {
          ran += 1;
        }}
      />
    ));
    // Every button in a stale card, clicked, and none of them is a run.
    for (const button of host.querySelectorAll<HTMLButtonElement>("button")) button.click();
    // Only the action row reports, and only for the buttons that are actions.
    expect(ran).toBe(host.querySelectorAll("[data-action]").length);
    expect(host.querySelector("[data-card-stale] button")).toBeNull();
  });
});

describe("readout arithmetic is integral (no float formatter anywhere)", () => {
  test.each([
    [0, "0.00s"],
    [1_204, "0.00s"],
    [8_412, "0.01s"],
    [80_000, "0.08s"],
    [1_000_000, "1.00s"],
    [59_995_000, "1m 00.00s"],
    [95_600_000, "1m 35.60s"],
  ])("%i µs -> %s", (us: number, want: string) => {
    expect(durationLabel(us)).toBe(want);
  });

  test("readout parts carry their own accessible name", () => {
    const r = readout(41 as never, 17 as never, 80_000);
    expect([r.exec, r.dataset, r.duration]).toEqual(["E41", "D17", "0.08s"]);
  });
});

describe("small pure helpers", () => {
  test("cardState prefers running, then failure, then staleness", () => {
    const ok = envelopeOf(KINDS.summarize);
    expect(cardState(ok, {})).toBe("current");
    expect(cardState(ok, { stale: { upstream: "u", because: "b" } })).toBe("stale");
    expect(cardState(ok, { running: true })).toBe("running");
    expect(cardState({ ...ok, rc: 111 }, {})).toBe("failed");
    expect(cardState({ ...ok, rc: 111 }, { running: true })).toBe("running");
  });

  test("middleEllipsis keeps both informative ends", () => {
    expect(middleEllipsis("abcdef", 6)).toBe("abcdef");
    expect(middleEllipsis("abcdefgh", 5)).toBe("ab…gh");
    expect(middleEllipsis("abcdefgh", 5)).toHaveLength(5);
  });

  test("the announcement is a sentence, not a template with a number in it", () => {
    expect(announcement(envelopeOf(KINDS.summarize))).toBe("summarize finished, 0.08s");
    expect(announcement({ ...envelopeOf(KINDS.error), rc: 111 })).toBe(
      "summarize failed, r(111), 0.08s",
    );
  });
});
