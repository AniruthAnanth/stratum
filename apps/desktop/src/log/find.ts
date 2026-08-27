/**
 * The find bar — 06 §9.2.
 *
 * > Find bar toggled by `⌕` in the pane header and `Mod+F`; matches highlighted,
 * > `Enter`/`Shift+Enter` to step. **Search runs in Rust over the full 5 M-line
 * > buffer, not over the DOM.**
 *
 * That last sentence is the whole design. A `document.querySelectorAll` search
 * over a virtualised log finds matches in the 600 lines that happen to be
 * rendered and reports "1 of 3" on a log with four thousand hits — which is not
 * a slow search, it is a wrong one. So this module owns a query, a hit list
 * that came from `log_search`, and a cursor into it; it never reads the DOM.
 *
 * Highlighting is the renderer's job and is driven from {@link hitsOnLine},
 * which is a lookup in a per-line index built once per search rather than a
 * scan per rendered row: a 600-row window × 4 000 hits is 2.4 M comparisons on
 * a scroll frame, and that is the kind of loop 06 §15.1 forbids outright.
 */

import type { LogHitView, LogSearchOptions, LogWindow } from "./window";

/** Stata's own find bar has no regex toggle; ours does, and defaults to off. */
export const DEFAULT_SEARCH_OPTIONS: LogSearchOptions = {
  regex: false,
  caseSensitive: false,
  maxHits: 10_000,
};

export interface FindState {
  readonly query: string;
  readonly open: boolean;
  readonly hits: readonly LogHitView[];
  /** Index into `hits`, or `-1` when there is no current match. */
  readonly current: number;
  /** What Rust reports it found, which may exceed `hits.length` at `maxHits`. */
  readonly total: number;
  readonly running: boolean;
  /** Set when the query is a regex and it did not compile. */
  readonly error: string | undefined;
}

export const EMPTY_FIND: FindState = {
  query: "",
  open: false,
  hits: [],
  current: -1,
  total: 0,
  running: false,
  error: undefined,
};

export interface FindCounters {
  /** `log_search` round trips. Stepping through hits must add none. */
  searches: number;
  /** Per-line index rebuilds. One per completed search, never per frame. */
  indexBuilds: number;
}

const ZERO: FindCounters = { searches: 0, indexBuilds: 0 };
export const findCounters: FindCounters = { ...ZERO };
export function resetFindCounters(): void {
  Object.assign(findCounters, ZERO);
}

/**
 * The find model for one log surface.
 *
 * Superseding is by generation rather than by `AbortController`: `log_search`
 * is a Tauri command and Tauri commands have no cancellation, so the honest
 * mechanism is to ignore a stale reply rather than to pretend it was cancelled.
 * (The frame transport does have cancellation — that is `AbortController` over
 * `stratum-asset://` — and this is not that.)
 */
export class LogFind {
  private state: FindState = EMPTY_FIND;
  private generation = 0;
  private index = new Map<number, LogHitView[]>();

  constructor(
    private readonly window: LogWindow,
    private readonly onChange: () => void = () => {},
  ) {}

  get snapshot(): FindState {
    return this.state;
  }

  open(): void {
    this.set({ open: true });
  }

  close(): void {
    this.generation += 1;
    this.index = new Map();
    this.state = EMPTY_FIND;
    this.onChange();
  }

  /**
   * Run a query. Returns when the reply has been applied or discarded.
   *
   * An empty query clears rather than searching: `log_search("")` on a 5 M-line
   * buffer is a request for five million hits, and it is exactly what a user
   * produces by selecting the query and pressing Backspace.
   */
  async search(query: string, opts: Partial<LogSearchOptions> = {}): Promise<void> {
    const generation = ++this.generation;
    if (query === "") {
      this.index = new Map();
      this.set({ query, hits: [], current: -1, total: 0, running: false, error: undefined });
      return;
    }
    this.set({ query, running: true, error: undefined });
    findCounters.searches += 1;
    try {
      const reply = await this.window.searchLog(query, { ...DEFAULT_SEARCH_OPTIONS, ...opts });
      if (generation !== this.generation) return;
      this.index = buildIndex(reply.hits);
      this.set({
        hits: reply.hits,
        total: reply.total,
        current: reply.hits.length > 0 ? 0 : -1,
        running: false,
      });
      this.reveal();
    } catch (error) {
      if (generation !== this.generation) return;
      this.set({
        hits: [],
        total: 0,
        current: -1,
        running: false,
        error: error instanceof Error ? error.message : String(error),
      });
    }
  }

  /** `Enter`. Wraps, because a find bar that stops at the end is a bug report. */
  next(): void {
    if (this.state.hits.length === 0) return;
    this.set({ current: (this.state.current + 1) % this.state.hits.length });
    this.reveal();
  }

  /** `Shift+Enter`. */
  previous(): void {
    const n = this.state.hits.length;
    if (n === 0) return;
    this.set({ current: (this.state.current - 1 + n) % n });
    this.reveal();
  }

  /**
   * Hits on one rendered line, from the index.
   *
   * Returns the shared array rather than a copy: it is read on every rendered
   * row of every frame, and a copy per row is 600 allocations a frame for a
   * result the renderer only iterates.
   */
  hitsOnLine(line: number): readonly LogHitView[] {
    return this.index.get(line) ?? EMPTY_HITS;
  }

  /** Is this hit the one `Enter` most recently landed on? Drives the accent. */
  isCurrent(hit: LogHitView): boolean {
    return this.state.hits[this.state.current] === hit;
  }

  private reveal(): void {
    const hit = this.state.hits[this.state.current];
    if (hit === undefined) return;
    // Land the hit a third of the way down rather than at the top edge: a match
    // on the first visible row has no context above it, and context is the
    // reason someone is searching a log rather than grepping the file.
    this.window.scrollTo(hit.line - Math.floor(this.window.viewport / 3));
    void this.window.ensureVisible();
  }

  private set(patch: Partial<FindState>): void {
    this.state = { ...this.state, ...patch };
    this.onChange();
  }
}

const EMPTY_HITS: readonly LogHitView[] = [];

function buildIndex(hits: readonly LogHitView[]): Map<number, LogHitView[]> {
  findCounters.indexBuilds += 1;
  const index = new Map<number, LogHitView[]>();
  for (const hit of hits) {
    const list = index.get(hit.line);
    if (list === undefined) index.set(hit.line, [hit]);
    else list.push(hit);
  }
  return index;
}

/**
 * Split one line's text into alternating plain and matched pieces.
 *
 * Exported so the renderer never does interval arithmetic of its own. Hits are
 * sorted and clamped here, because `log_search` is allowed to return them in
 * whatever order its scan produced and an unsorted pair would render as
 * overlapping marks.
 */
export function segmentLine(
  text: string,
  hits: readonly LogHitView[],
): { text: string; hit: LogHitView | undefined }[] {
  if (hits.length === 0) return [{ text, hit: undefined }];
  const sorted = [...hits].sort((a, b) => a.col - b.col);
  const out: { text: string; hit: LogHitView | undefined }[] = [];
  let at = 0;
  for (const hit of sorted) {
    const from = Math.max(at, Math.min(hit.col, text.length));
    const to = Math.max(from, Math.min(hit.col + hit.len, text.length));
    if (from > at) out.push({ text: text.slice(at, from), hit: undefined });
    if (to > from) out.push({ text: text.slice(from, to), hit });
    at = to;
  }
  if (at < text.length) out.push({ text: text.slice(at), hit: undefined });
  return out;
}
