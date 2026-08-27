/**
 * The e2e control protocol — the wire between `stratum-e2e` and this frontend.
 *
 * One JSON object per line, request/response, ids strictly increasing. It is
 * deliberately the same shape on both sides of the two tiers: tier 1 speaks it
 * over a loopback socket, and tier 2 calls {@link E2eBridge.dispatch} and
 * {@link E2eBridge.snapshot} through WebDriver's `executeScript`. One protocol
 * means the two tiers cannot drift into asking the app different questions —
 * what differs between them is how the *input* arrives, which is the only thing
 * that should differ.
 *
 * The types here mirror `crates/stratum-e2e/src/{actions,snapshot}.rs`. They are
 * hand-written rather than generated because this channel is dev-only and must
 * not appear in `src/ipc/types.ts`: a test-only IPC surface that ships is a
 * remote-control backdoor (ADR-011), and the `e2e` cargo feature fences the Rust
 * half for the same reason.
 */

/** Snapshot sections the harness can ask for. */
export type Section =
  | "doc"
  | "gutter"
  | "results"
  | "cards"
  | "panes"
  | "focus"
  | "layout"
  | "history"
  | "blocks";

/** A snapshot field the host can produce, or the unit that owes it. */
export type Field<T> =
  | { present: T }
  | { unavailable: { unit: string; why: string; witness: string } };

/** Say a field exists. */
export const present = <T>(value: T): Field<T> => ({ present: value });

/**
 * Say a field does not exist yet, who owes it, and **what would prove it wrong**.
 *
 * This is the single most important function in the bridge. Every field that
 * would otherwise be invented — a caret with no editor, a card with no
 * renderer — goes through here instead, so a scenario asserting on it is
 * reported as blocked on a named unit rather than passing against a stand-in.
 *
 * # Why `witness` is not optional
 *
 * Through wave 1 this took two arguments, and the prose in the second one went
 * stale: the bridge said W13 owed the document because "no editor is mounted"
 * and that W14 owed cards because "none is written", while both units had landed
 * in that same wave. Nothing failed, because a sentence cannot expire.
 *
 * `witness` is the repo-relative path whose ABSENCE is the claim — the module
 * that would answer the field if it existed. It turns the claim into something a
 * test can evaluate: `the_blocked_ledger_has_not_expired` in `serve.test.ts`
 * takes a full snapshot and fails if any witness is present in the tree, and
 * `tests/e2e/harness.rs` prints it beside every blocked step so the report says
 * what is missing rather than only who is blamed.
 */
export const owedBy = <T>(unit: string, why: string, witness: string): Field<T> => ({
  unavailable: { unit, why, witness },
});

/** `BlockStatus`'s discriminant, as `STATUS_RANK` in `src/ipc/hand.ts` spells it. */
export type Glyph =
  | "never_run"
  | "broken"
  | "failed"
  | "interrupted"
  | "stale"
  | "current_unverifiable"
  | "current"
  | "queued"
  | "running";

export interface DocView {
  path: string | null;
  text: string;
  caret: number;
  version: number;
}

export interface GutterRow {
  block: number;
  glyph: Glyph;
}

export interface ResultView {
  result: number;
  block: number | null;
  client_key: string;
  cmdline: string;
  rc: number;
  raw_head: string[];
  payloads: string[];
}

export interface Card {
  block: number | null;
  result: number;
  header: string;
  body: string[];
  rc: number;
}

export interface PaneView {
  id: string;
  visible: boolean;
  content: Field<string[]>;
}

export interface LayoutView {
  id: string;
  inline_results: string;
}

export interface HistoryRow {
  command: string;
  rc: number;
}

export interface BlockView {
  index: number;
  span: [number, number];
  status: Glyph;
  hash: string;
  /** The hash the engine reported for this block's last execution, if any. */
  engine_hash: string | null;
}

export interface Snapshot {
  host: string;
  doc: Field<DocView>;
  gutter: Field<GutterRow[]>;
  results: Field<ResultView[]>;
  cards: Field<Card[]>;
  panes: Field<PaneView[]>;
  focus: Field<string>;
  layout: Field<LayoutView>;
  history: Field<HistoryRow[]>;
  blocks: Field<BlockView[]>;
}

/** What the host did with an action. */
export interface Dispatched {
  /** `verb` | `chord` | `injection` | `observe` | `bridge`. */
  via: string;
  /** `ran` | `unknown` | `disabled`, straight from `runCommand`. */
  result: string;
  /** What the live keymap trie resolves the action's chord to. */
  chord_resolves_to: string | null;
  /** Engine events actually consumed. */
  events_applied: number;
}

/** Capability ids, as `crate::Capability` serialises them. */
export type Capability =
  | "commands"
  | "keymap"
  | "layout"
  | "settings"
  | "results"
  | "history"
  | "event_injection"
  | "editor"
  | "gutter"
  | "cards"
  | "panes"
  | "data_editor"
  | "engine";

/** An action, as `crate::Action` serialises it. Narrowed at the boundary. */
export interface Action {
  action: string;
  [field: string]: unknown;
}

/** Harness → host. */
export type Request =
  | { op: "hello"; id: number; harness: string }
  | { op: "dispatch"; id: number; action: Action }
  | { op: "snapshot"; id: number; what: Section[] }
  | { op: "quit"; id: number };

/** Host → harness. */
export interface Response {
  id: number;
  ok: boolean;
  error?: string;
  host?: string;
  capabilities?: Capability[];
  dispatched?: Dispatched;
  snapshot?: Snapshot;
}

/** The `window` global tier 2 reaches through `executeScript`. */
export const BRIDGE_GLOBAL = "__STRATUM_E2E__";

/** What that global is. */
export interface E2eBridge {
  capabilities(): Capability[];
  dispatch(action: Action): Dispatched;
  settle(): Dispatched;
  snapshot(what: Section[]): Snapshot;
  reset(): void;
}
