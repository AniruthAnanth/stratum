/**
 * The keystroke trie — 06 §12.1.
 *
 * There is exactly one keyboard authority in the app. That is a claim about
 * this file: the window-level capture listener (`listener.ts`) and the CM6
 * compartment (`editor.ts`) are two entry points into one `resolve`, over one
 * trie, with one `when` evaluation. A second table anywhere would be a second
 * authority, and the symptom would be a binding that works everywhere except
 * inside the editor — which is the bug every IDE ships at least once.
 */

import type { HostPlatform } from "../platform/bridge";
import { type CompiledWhen, type KeyContext, compileWhen } from "./context";

export interface KeyBinding {
  command: string; // "run.block"
  key: string; // "Mod+Enter", chords: "Mod+K Mod+S"
  when?: string; // boolean expr over context keys
  args?: unknown;
  source: "preset" | "user";
}

// ---------------------------------------------------------------------------
// Keystroke normalisation
// ---------------------------------------------------------------------------

/**
 * Keys matched by `KeyboardEvent.code` — the physical position. A binding on
 * `Mod+1` must be the key labelled 1 whatever the layout does with it, and
 * `Enter` is a position, not a character.
 */
const PHYSICAL: Readonly<Record<string, string>> = {
  enter: "Enter",
  return: "Enter",
  escape: "Escape",
  esc: "Escape",
  tab: "Tab",
  backspace: "Backspace",
  delete: "Delete",
  space: "Space",
  up: "ArrowUp",
  down: "ArrowDown",
  left: "ArrowLeft",
  right: "ArrowRight",
  pageup: "PageUp",
  pagedown: "PageDown",
  home: "Home",
  end: "End",
  insert: "Insert",
  ...Object.fromEntries(Array.from({ length: 24 }, (_, i) => [`f${i + 1}`, `F${i + 1}`] as const)),
  ...Object.fromEntries(Array.from({ length: 10 }, (_, i) => [`${i}`, `Digit${i}`] as const)),
};

export interface Keystroke {
  ctrl: boolean;
  alt: boolean;
  shift: boolean;
  meta: boolean;
  /** `code` for a physical key, the lowercased `key` for a character. */
  id: string;
  kind: "code" | "char";
}

export class KeyParseError extends Error {}

/** `Mod` is Cmd on macOS and Ctrl elsewhere — 06 §12.1, spec §33. */
function applyMod(k: Keystroke, platform: HostPlatform): void {
  if (platform === "macos") k.meta = true;
  else k.ctrl = true;
}

/** Parses one keystroke, e.g. `Mod+Shift+K`. Chords are split by the caller. */
export function parseKeystroke(text: string, platform: HostPlatform): Keystroke {
  const parts = text.split("+").map((p) => p.trim());
  const last = parts.pop();
  if (last === undefined || last === "") throw new KeyParseError(`empty keystroke in ${text}`);

  const k: Keystroke = { ctrl: false, alt: false, shift: false, meta: false, id: "", kind: "char" };
  for (const raw of parts) {
    switch (raw.toLowerCase()) {
      case "mod":
        applyMod(k, platform);
        break;
      case "cmd":
      case "meta":
      case "super":
      case "win":
        k.meta = true;
        break;
      case "ctrl":
      case "control":
        k.ctrl = true;
        break;
      case "alt":
      case "option":
        k.alt = true;
        break;
      case "shift":
        k.shift = true;
        break;
      default:
        throw new KeyParseError(`unknown modifier ${raw} in ${text}`);
    }
  }

  const physical = PHYSICAL[last.toLowerCase()];
  if (physical !== undefined) {
    k.kind = "code";
    k.id = physical;
  } else if (last.length === 1) {
    k.kind = "char";
    k.id = last.toLowerCase();
  } else {
    throw new KeyParseError(`unknown key ${last} in ${text}`);
  }
  return k;
}

/** A stable string identity for a keystroke; the trie's edge label. */
export function keystrokeId(k: Keystroke): string {
  return `${k.ctrl ? "c" : ""}${k.alt ? "a" : ""}${k.shift ? "s" : ""}${k.meta ? "m" : ""}:${k.kind}:${k.id}`;
}

const MODIFIER_KEYS = new Set(["Control", "Alt", "Shift", "Meta"]);

/** `.code` values we treat as positions rather than as characters. */
const PHYSICAL_CODES = /^(Digit\d|F\d{1,2}|Arrow(Up|Down|Left|Right))$/;
const NAMED_CODES = new Set([
  "Enter",
  "NumpadEnter",
  "Escape",
  "Tab",
  "Backspace",
  "Delete",
  "Space",
  "PageUp",
  "PageDown",
  "Home",
  "End",
  "Insert",
]);

/**
 * The candidate identities of a real `KeyboardEvent`, most specific first.
 *
 * A keystroke has TWO defensible identities and 06 §12.1 asks for both: "key
 * names normalise via `KeyboardEvent.code` for physical keys and `.key` for
 * characters". On a US layout the two never disagree, which is exactly why
 * choosing one of them up front is a bug you cannot see. On a German layout `/`
 * is Shift+`Digit7`, so a code-first rule makes `Mod+/` unreachable; on AZERTY
 * `Digit1` produces `&`, so a key-first rule makes `Mod+1` unreachable.
 *
 * So we produce both and let the trie decide: the CHARACTER first, because a
 * binding written `Mod+/` is about the glyph on the keycap, then the POSITION,
 * because a binding written `Mod+1` is about where the finger goes. A binding
 * can never be written as a character that `parseKeystroke` maps to a position
 * (digits and named keys all resolve to `code`), so the two candidate sets are
 * disjoint and the order is a tiebreak that never actually ties.
 */
export function eventKeystrokes(e: KeyboardEvent): Keystroke[] {
  if (MODIFIER_KEYS.has(e.key)) return [];

  const base = { ctrl: e.ctrlKey, alt: e.altKey, shift: e.shiftKey, meta: e.metaKey };
  const out: Keystroke[] = [];

  if (e.key.length === 1) out.push({ ...base, kind: "char", id: e.key.toLowerCase() });

  const code = e.code;
  if (code !== "" && (PHYSICAL_CODES.test(code) || NAMED_CODES.has(code))) {
    out.push({ ...base, kind: "code", id: code === "NumpadEnter" ? "Enter" : code });
  }

  // A named key we do not bind (`Dead`, `Compose`, a media key): matched by name
  // so a future binding for it needs no change here.
  if (out.length === 0) out.push({ ...base, kind: "code", id: code === "" ? e.key : code });
  return out;
}

/** The primary identity, for callers that only need one (accelerator capture). */
export function eventKeystroke(e: KeyboardEvent): Keystroke | undefined {
  return eventKeystrokes(e)[0];
}

// ---------------------------------------------------------------------------
// The trie
// ---------------------------------------------------------------------------

interface Compiled {
  binding: KeyBinding;
  when: CompiledWhen | undefined;
  /** Insertion index, so the "last one wins" rule is deterministic. */
  order: number;
}

interface Node {
  children: Map<string, Node>;
  bindings: Compiled[];
}

const node = (): Node => ({ children: new Map(), bindings: [] });

export type Resolution =
  | { readonly kind: "none" }
  /** A chord prefix matched; the next keystroke continues it. */
  | { readonly kind: "pending"; readonly prefix: readonly string[] }
  | { readonly kind: "command"; readonly command: string; readonly args?: unknown };

export interface ConflictReport {
  key: string;
  command: string;
  /** The command that wins this keystroke under the same `when`. */
  shadowedBy: string;
}

export class KeyTrie {
  private readonly root = node();
  private counter = 0;
  private readonly all: Compiled[] = [];
  readonly parseErrors: { binding: KeyBinding; message: string }[] = [];

  constructor(
    bindings: readonly KeyBinding[],
    private readonly platform: HostPlatform,
  ) {
    // `source: "user"` sorts after presets so a user override wins the
    // last-one-wins tiebreak regardless of file order (06 §12.1).
    const ordered = [...bindings].sort(
      (a, b) => (a.source === "user" ? 1 : 0) - (b.source === "user" ? 1 : 0),
    );
    for (const b of ordered) this.add(b);
  }

  private add(binding: KeyBinding): void {
    let cursor = this.root;
    try {
      for (const stroke of binding.key.trim().split(/\s+/)) {
        const id = keystrokeId(parseKeystroke(stroke, this.platform));
        let next = cursor.children.get(id);
        if (next === undefined) {
          next = node();
          cursor.children.set(id, next);
        }
        cursor = next;
      }
      const compiled: Compiled = {
        binding,
        when: binding.when === undefined ? undefined : compileWhen(binding.when),
        order: this.counter++,
      };
      cursor.bindings.push(compiled);
      this.all.push(compiled);
    } catch (error) {
      // A malformed binding is a bad line in a JSON file a user may have edited.
      // It must not take the other 60 bindings down with it.
      this.parseErrors.push({
        binding,
        message: error instanceof Error ? error.message : String(error),
      });
    }
  }

  /**
   * Walks `prefix` then `stroke`. `prefix` is the caller's pending chord state;
   * `listener.ts` owns it so the two entry points cannot disagree about it.
   */
  resolve(
    prefix: readonly string[],
    stroke: Keystroke | readonly Keystroke[],
    ctx: KeyContext,
  ): Resolution {
    let cursor: Node | undefined = this.root;
    for (const id of prefix) {
      cursor = cursor.children.get(id);
      if (cursor === undefined) return { kind: "none" };
    }

    for (const candidate of Array.isArray(stroke) ? stroke : [stroke as Keystroke]) {
      const id = keystrokeId(candidate);
      const next: Node | undefined = cursor.children.get(id);
      if (next === undefined) continue;

      // A prefix that is also a complete binding waits for the next key. No
      // preset has that shape; the rule is stated so a user overlay that
      // introduces one behaves predictably rather than racing a timer.
      if (next.children.size > 0) return { kind: "pending", prefix: [...prefix, id] };

      const winner = this.pick(next.bindings, ctx);
      if (winner === undefined) continue;
      return winner.binding.args === undefined
        ? { kind: "command", command: winner.binding.command }
        : { kind: "command", command: winner.binding.command, args: winner.binding.args };
    }
    return { kind: "none" };
  }

  private pick(candidates: readonly Compiled[], ctx: KeyContext): Compiled | undefined {
    let best: Compiled | undefined;
    for (const c of candidates) {
      if (c.when !== undefined && !c.when(ctx)) continue;
      if (best === undefined || c.order > best.order) best = c;
    }
    return best;
  }

  /**
   * What the keymap editor's "Show conflicts" filter renders. Conflicts are not
   * errors (06 §12.1) — two bindings on one key with disjoint `when` clauses are
   * the normal case — so this reports only the pairs that actually shadow under
   * an all-true context.
   */
  conflicts(): ConflictReport[] {
    const byKey = new Map<string, Compiled[]>();
    for (const c of this.all) {
      const list = byKey.get(c.binding.key);
      if (list === undefined) byKey.set(c.binding.key, [c]);
      else list.push(c);
    }
    const out: ConflictReport[] = [];
    for (const [key, list] of byKey) {
      if (list.length < 2) continue;
      const winner = list.reduce((a, b) => (b.order > a.order ? b : a));
      for (const c of list) {
        if (c === winner) continue;
        if (c.binding.when !== winner.binding.when) continue;
        out.push({ key, command: c.binding.command, shadowedBy: winner.binding.command });
      }
    }
    return out;
  }

  /** Every keystroke sequence bound to `command`, for accelerator rendering. */
  keysFor(command: string): string[] {
    return this.all.filter((c) => c.binding.command === command).map((c) => c.binding.key);
  }

  get size(): number {
    return this.all.length;
  }
}
