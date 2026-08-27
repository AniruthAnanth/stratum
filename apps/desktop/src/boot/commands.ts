/**
 * The shell's own verbs — 06 §5.4, §8, §12.
 *
 * Only the ones the SHELL owns: layout, panes, view modes, theme. Every run
 * verb (`run.block`, `run.fromHere`, …) belongs to W13's editor and every result
 * verb to W14, and they register their own — which is the point of the registry.
 * A keystroke bound to a verb nobody has registered does nothing and falls
 * through to the platform, so the shell shipping first does not break the
 * keymap; it just means fewer keys do something until the panes land.
 */

import { isPresetId } from "../dock/layoutSpec";
import { isPaneComponentId } from "../dock/panes";
import { type CommandDescriptor, registerCommands } from "../keys/registry";
import { applyPreset, focusPane, layoutSpec, paneForIndex, togglePane } from "../state/layout";
import { clearResults } from "../state/results";
import { cycleInlineResults, updateSettings, userSettings } from "../state/settings";
import { applyTheme } from "./theme";

const asRecord = (args: unknown): Record<string, unknown> =>
  typeof args === "object" && args !== null ? (args as Record<string, unknown>) : {};

const SHELL_COMMANDS: readonly CommandDescriptor[] = [
  {
    id: "layout.apply",
    title: "Switch layout",
    category: "Layout",
    run(args) {
      const id = asRecord(args)["id"];
      if (typeof id === "string" && isPresetId(id)) applyPreset(id);
    },
  },
  {
    id: "pane.toggle",
    title: "Toggle pane",
    category: "Layout",
    run(args) {
      const record = asRecord(args);
      const explicit = record["paneId"];
      if (typeof explicit === "string" && isPaneComponentId(explicit)) {
        togglePane(explicit);
        return;
      }
      // `Mod+1..9` names an INDEX, not a pane: the user counts panes left to
      // right on screen, and which pane is third depends on the layout.
      const index = record["index"];
      if (typeof index !== "number") return;
      const pane = paneForIndex(index);
      if (pane !== undefined) togglePane(pane);
    },
  },
  {
    id: "pane.toggleAssistant",
    title: "Toggle Assistant",
    category: "Layout",
    // 06 §8.2: the Assistant is a tab and is not active on first launch. `Mod+J`
    // RAISES it rather than toggling its existence, because closing it would be
    // indistinguishable from the state it starts in.
    run: () => focusPane("assistant"),
  },
  {
    id: "view.cycleInlineResults",
    title: "Cycle inline results mode",
    category: "View",
    run: () => cycleInlineResults(layoutSpec().defaults.inlineResults),
  },
  {
    id: "view.setTheme",
    title: "Set theme",
    category: "View",
    run(args) {
      const theme = asRecord(args)["theme"];
      if (theme === "light" || theme === "dark" || theme === "system") {
        updateSettings({ theme });
        applyTheme(theme);
      }
    },
  },
  {
    id: "view.increaseCodeSize",
    title: "Increase editor font size",
    category: "View",
    run: () => updateSettings({ codeSizePx: userSettings().codeSizePx + 1 }),
  },
  {
    id: "view.decreaseCodeSize",
    title: "Decrease editor font size",
    category: "View",
    run: () => updateSettings({ codeSizePx: userSettings().codeSizePx - 1 }),
  },
  {
    id: "results.clearAll",
    title: "Clear results",
    category: "Results",
    // Clears this window's resident envelopes only. Rust's ResultStore is the
    // archive and is untouched, so "clear" is a view operation and never a
    // deletion — 06 §6.1's "everything goes to the scrollback, always".
    run: () => clearResults(),
  },
];

export function registerShellCommands(): () => void {
  return registerCommands(SHELL_COMMANDS);
}

export { SHELL_COMMANDS };
