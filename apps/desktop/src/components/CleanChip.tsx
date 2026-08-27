/**
 * Interactive vs clean, made unmissable — spec §15; 06 §5.3.
 *
 * > "confusing an interactive run with a clean run is the single most expensive
 * > mistake in this product" — 06 §5.3
 *
 * Expensive because the two runs answer different questions. An interactive run
 * says "given everything I have done in this session, here is the number". A
 * clean run says "given only this file, here is the number" — and only the
 * second one is a claim you can put in a paper. A researcher who reads the first
 * as the second has published a result that does not reproduce, and they will
 * find out from a referee.
 *
 * So the distinction is carried on **three** channels, not one:
 *
 *  1. a `CLEAN` chip in the top bar, present for the whole duration of the run;
 *  2. **neutral ink instead of teal on every state glyph** inside a clean scope
 *     — the CSS in `exec.css`, driven by the `data-clean` attribute
 *     {@link CleanScope} sets;
 *  3. the seed and the entry file named in the chip's tooltip, because "clean"
 *     is a claim about a specific fresh environment (ARCHITECTURE §7.7's
 *     16-item checklist) and not a mood.
 *
 * Colour alone would fail 06 §17 (colour is never the only channel) and would
 * also fail the greyscale-screenshot-in-a-bug-report test.
 *
 * # Wiring
 *
 * `ui/chrome.tsx`'s `TopBar` (W12) draws the readout from plain props and knows
 * nothing about runs, which is correct — it renders in windows with no engine.
 * The host therefore passes `trailing={<CleanChip />}`, or uses this unit's
 * connected {@link StateReadout}, which already contains one. Either way the
 * chip reads the store rather than a prop, so no caller can forget to update it.
 */

import { type JSX, Show } from "solid-js";
import { runCommand } from "../keys/registry";
import { cleanRunActive, runState } from "../state/exec";
import { Button, Chip } from "../ui";

import "./exec.css";

export interface CleanChipProps {
  /** Overrides the store. The top bar passes nothing; tests and previews pass this. */
  active?: boolean;
  seed?: number;
  source?: string;
}

/**
 * `CLEAN`, in the top bar, for the duration.
 *
 * Deliberately a `Chip` with `tone="neutral"` and an icon: the neutral tone is
 * the same ink the glyphs switch to, so the chip and the gutter say the same
 * thing in the same colour, and the icon keeps it legible without colour at all.
 */
export function CleanChip(props: CleanChipProps): JSX.Element {
  const active = (): boolean => props.active ?? cleanRunActive();
  const seed = (): number | undefined => props.seed ?? runState().seed;
  const source = (): string | undefined => props.source ?? runState().source;

  const title = (): string => {
    const parts = ["Clean run: a fresh session, not this one."];
    if (source() !== undefined) parts.push(`Entry: ${String(source())}`);
    if (seed() !== undefined) parts.push(`Seed: ${String(seed())}`);
    return parts.join(" ");
  };

  return (
    <Show when={active()}>
      <span class="clean-chip" data-clean-chip title={title()}>
        <Chip tone="neutral" icon="check">
          CLEAN
        </Chip>
      </span>
    </Show>
  );
}

/**
 * Marks a subtree as belonging to a clean run.
 *
 * Everything inside renders its state glyphs in neutral ink instead of the
 * accent — one attribute, one CSS rule, no component in the tree below needs to
 * know. Applied by the top bar for the duration of the run and by a card whose
 * block last executed under `Isolation`.
 */
export function CleanScope(props: { clean: boolean; children: JSX.Element }): JSX.Element {
  return (
    <div class="clean-scope" data-clean={props.clean ? "" : undefined}>
      {props.children}
    </div>
  );
}

export interface CleanRunButtonProps {
  onAction?: (command: string) => void;
  /** `run.entryPoint` instead of `run.fileClean` — the project-scoped verb (A23). */
  entryPoint?: boolean;
}

/**
 * "Run do-file from clean state" — spec §15's **prominent** command.
 *
 * §15 says prominent, so it is a labelled button in the chrome and not a
 * palette-only verb. It dispatches the command id rather than calling a run
 * function, which is what makes the same words reachable from the palette, the
 * native menu and a keybinding without three code paths (06 §5.4).
 */
export function CleanRunButton(props: CleanRunButtonProps): JSX.Element {
  const command = (): string => (props.entryPoint === true ? "run.entryPoint" : "run.fileClean");
  return (
    <Button
      variant="default"
      icon="rerun"
      class="clean-run"
      data-clean-run
      data-exec-action={command()}
      onClick={() => {
        if (props.onAction === undefined) runCommand(command());
        else props.onAction(command());
      }}
    >
      {props.entryPoint === true ? "Run entry point from clean state" : "Run from clean state"}
    </Button>
  );
}
