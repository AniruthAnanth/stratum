/**
 * The Sections pane — `PaneId "sections"` (A34).
 *
 * Two things are worth testing about an outline: that it registers into W12's
 * dock and survives being mounted before any editor exists, and that it is a
 * VIEW — clicking a row runs a command id, it does not reach into the editor.
 */

import { beforeEach, describe, expect, it, vi } from "vitest";
import { disposePaneHosts, paneHost } from "../../dock/panes";
import { setActiveEditor } from "../../editor/commands";
import { mountEditor } from "../../editor/harness";
import { registerSectionsPane } from "./index";

beforeEach(() => {
  vi.spyOn(console, "warn").mockImplementation(() => {});
  disposePaneHosts();
  setActiveEditor(null);
});

describe("the Sections pane", () => {
  it("mounts into the dock host with no editor open", () => {
    const dispose = registerSectionsPane();
    const host = paneHost("sections");
    expect(host.dataset["pane"]).toBe("sections");
    expect(host.hasAttribute("data-unregistered")).toBe(false);
    // The empty state explains the marker syntax rather than saying "no data".
    expect(host.textContent).toContain("// %% Data loading");
    dispose();
    disposePaneHosts();
  });

  it("lists the sections of the active editor with their labels", async () => {
    const h = await mountEditor(
      [
        "// %% Data loading",
        "use survey.dta, clear",
        "// %% Cleaning",
        "drop if missing(income)",
      ].join("\n"),
    );
    setActiveEditor(h.view);
    const dispose = registerSectionsPane();
    const host = paneHost("sections");

    // The row model is read on mount; the pane polls rather than subscribing to
    // every transaction, because an outline changes when markers do and not when
    // a character does.
    expect(host.textContent).toContain("Data loading");
    expect(host.textContent).toContain("Cleaning");
    expect(host.querySelectorAll(".pane-sectionRow")).toHaveLength(2);

    dispose();
    disposePaneHosts();
    setActiveEditor(null);
    h.destroy();
  });
});
