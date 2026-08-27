/**
 * Boot: role dispatch, theme application, and the shell's own verbs.
 */

import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { clearPaneRegistry, disposePaneHosts } from "../dock/panes";
import { clearCommands, getCommand, listCommands, runCommand } from "../keys/registry";
import { applyPreset, layoutSpec, resetLayoutState } from "../state/layout";
import { resetSettings, userSettings } from "../state/settings";
import { SHELL_COMMANDS, registerShellCommands } from "./commands";
import { parseIdentity } from "./role";
import { applyScale, applyTheme, resolvedTheme } from "./theme";

describe("role dispatch", () => {
  it("reads the pane window's whole contract off the query string", () => {
    expect(parseIdentity("?role=pane&paneId=results&label=proj:pane:results")).toEqual({
      role: "pane",
      label: "proj:pane:results",
      paneId: "results",
      project: "proj",
    });
  });

  it("falls back to the document's own role attribute", () => {
    expect(parseIdentity("", "pane").role).toBe("main");
    expect(parseIdentity("", "main").role).toBe("main");
    expect(parseIdentity("?role=viewer").role).toBe("viewer");
  });

  it("degrades a pane window with no valid pane id", () => {
    // Falling back to the main shell would open a second dock; claiming some
    // other role would be a lie. A main-role window at least shows the error.
    expect(parseIdentity("?role=pane&label=proj:pane:nope").role).toBe("main");
    expect(parseIdentity("?role=pane&paneId=notapane").role).toBe("main");
  });

  it("ignores a role it does not know", () => {
    expect(parseIdentity("?role=administrator").role).toBe("main");
  });

  it("derives the project prefix from the label", () => {
    expect(parseIdentity("?label=myproj:editor:2").project).toBe("myproj");
    expect(parseIdentity("?label=main").project).toBe("stratum");
  });
});

describe("theme application", () => {
  let root: HTMLElement;

  beforeEach(() => {
    root = document.createElement("html");
  });

  it("is one attribute, in the three states the generated CSS encodes", () => {
    applyTheme("dark", root);
    expect(root.dataset["theme"]).toBe("dark");
    applyTheme("light", root);
    expect(root.dataset["theme"]).toBe("light");
    // "system" is the ABSENCE of the attribute, which is what lets
    // `prefers-color-scheme` decide in `tokens.generated.css`.
    applyTheme("system", root);
    expect(root.hasAttribute("data-theme")).toBe(false);
  });

  it("resolves an explicit choice without consulting the OS", () => {
    applyTheme("dark", root);
    expect(resolvedTheme(root)).toBe("dark");
    applyTheme("light", root);
    expect(resolvedTheme(root)).toBe("light");
  });

  it("sets the scale as multipliers, not as sizes", () => {
    // 06 §14.3 / §17: the whole type scale multiplies through `--fs-root` so OS
    // text scaling moves everything together and column alignment survives it.
    applyScale(1.25, 15, root);
    expect(root.style.getPropertyValue("--fs-root")).toBe("1.25");
    expect(root.style.getPropertyValue("--code-size")).toBe("15px");
  });
});

describe("the shell's verbs", () => {
  let dispose: (() => void) | undefined;

  beforeEach(() => {
    clearCommands();
    resetSettings();
    resetLayoutState();
    clearPaneRegistry();
    disposePaneHosts();
    dispose = registerShellCommands();
  });

  afterEach(() => {
    dispose?.();
    clearCommands();
    resetSettings();
    resetLayoutState();
    disposePaneHosts();
    clearPaneRegistry();
  });

  it("registers every verb it declares, and they are all dotted ids", () => {
    for (const descriptor of SHELL_COMMANDS) {
      expect(getCommand(descriptor.id), descriptor.id).toBeDefined();
      expect(descriptor.id).toMatch(/^[a-z]+\.[a-zA-Z]+$/);
      expect(descriptor.title.length).toBeGreaterThan(0);
    }
    expect(listCommands().length).toBe(SHELL_COMMANDS.length);
  });

  it("switches layout from the args a keystroke carries", () => {
    expect(runCommand("layout.apply", { id: "classic" })).toBe("ran");
    expect(layoutSpec().id).toBe("classic");
    // An id that is not a preset is ignored rather than applied blindly.
    runCommand("layout.apply", { id: "definitely-not-a-preset" });
    expect(layoutSpec().id).toBe("classic");
  });

  it("cycles the inline-results mode against the live layout default", () => {
    applyPreset("focus"); // defaults.inlineResults === "always"
    runCommand("view.cycleInlineResults");
    expect(userSettings().inlineResults).toBe("editor-run");
  });

  it("clamps the editor size through the settings store", () => {
    for (let i = 0; i < 20; i++) runCommand("view.increaseCodeSize");
    expect(userSettings().codeSizePx).toBe(18);
    for (let i = 0; i < 20; i++) runCommand("view.decreaseCodeSize");
    expect(userSettings().codeSizePx).toBe(11);
  });

  it("reports an unknown verb instead of throwing inside a keydown handler", () => {
    expect(runCommand("no.such.verb")).toBe("unknown");
  });

  it("unregisters cleanly, so a pane's verbs die with the pane", () => {
    expect(runCommand("results.clearAll")).toBe("ran");
    dispose?.();
    dispose = undefined;
    expect(runCommand("results.clearAll")).toBe("unknown");
  });
});
