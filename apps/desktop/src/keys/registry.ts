/**
 * The command registry — 06 §5.4 ("all verbs are commands with ids").
 *
 * Keystrokes, the command palette, the top-bar buttons and the host's native
 * menus all name commands by id and never call a handler directly. That is what
 * lets `menu_accelerator` label a native menu item with the accelerator the trie
 * actually holds, and it is why a pane that has not loaded yet can still appear
 * in the palette as a disabled row instead of not appearing.
 */

import { createSignal } from "solid-js";
import type { KeyContext } from "./context";

export interface CommandDescriptor {
  id: string;
  /** Palette label. Sentence case, no trailing punctuation. */
  title: string;
  category?: string;
  /** Palette and menu enablement. Absent means always enabled. */
  enabled?: (ctx: KeyContext) => boolean;
  run: (args?: unknown) => void | Promise<void>;
}

const commands = new Map<string, CommandDescriptor>();
const [version, bump] = createSignal(0);

/** Returns a disposer, so a pane unregisters its verbs when it is destroyed. */
export function registerCommand(descriptor: CommandDescriptor): () => void {
  commands.set(descriptor.id, descriptor);
  bump((v) => v + 1);
  return () => {
    if (commands.get(descriptor.id) === descriptor) {
      commands.delete(descriptor.id);
      bump((v) => v + 1);
    }
  };
}

export function registerCommands(descriptors: readonly CommandDescriptor[]): () => void {
  const disposers = descriptors.map(registerCommand);
  return () => {
    for (const d of disposers) d();
  };
}

export function getCommand(id: string): CommandDescriptor | undefined {
  return commands.get(id);
}

/** Reactive: the palette re-reads when a pane registers or disposes its verbs. */
export function listCommands(): CommandDescriptor[] {
  version();
  return [...commands.values()].sort((a, b) => a.id.localeCompare(b.id));
}

export type CommandResult = "ran" | "unknown" | "disabled";

/**
 * Runs a command. Never throws for an unknown id: a keymap may name a verb a
 * pane has not registered yet, and that must read as "nothing happened", not as
 * an unhandled rejection in a keydown handler.
 */
export function runCommand(id: string, args?: unknown, ctx: KeyContext = {}): CommandResult {
  const cmd = commands.get(id);
  if (cmd === undefined) return "unknown";
  if (cmd.enabled !== undefined && !cmd.enabled(ctx)) return "disabled";
  void Promise.resolve(cmd.run(args)).catch((error: unknown) => {
    reportCommandError(id, error);
  });
  return "ran";
}

type ErrorSink = (id: string, error: unknown) => void;
let sink: ErrorSink = (id, error) => {
  console.error(`command ${id} failed`, error);
};

/** `boot/errors.ts` installs the real sink so a failed verb reaches the status bar. */
export function setCommandErrorSink(next: ErrorSink): void {
  sink = next;
}

function reportCommandError(id: string, error: unknown): void {
  sink(id, error);
}

/** Test seam. */
export function clearCommands(): void {
  commands.clear();
  bump((v) => v + 1);
}
