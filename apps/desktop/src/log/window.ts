/**
 * The virtualised scrollback — 06 §9.2 and §15.2.
 *
 * > The log lives in Rust as a ring of `Arc<str>` chunks, default cap 5 M lines
 * > … The frontend holds a window of ~600 rendered lines plus a 2 000-line
 * > prefetch on each side.
 *
 * # What this is, and what it deliberately is not
 *
 * It is the *model*: which lines are resident, which line is at the top, how
 * tall a line is, and when to ask Rust for more. It renders nothing. That split
 * is what lets the Results pane (W14), the Viewer and a detached log window all
 * be different components over one scrollback, and it is what makes the whole
 * thing testable without a DOM.
 *
 * It is **not** a card feed and it is not append-and-forget. 06 §9.2: "A
 * scrollback log, not a card feed. Append-only, monospace, fixed line-height,
 * `white-space: pre`, no wrapping by default." Append-only is a property this
 * module enforces: {@link LogWindow.append} may only add at the tail, and there
 * is no method that edits a line that has already been shown.
 *
 * # The two height paths (06 §15.2)
 *
 * * **Fast path — wrapping off, the default, as in Stata.** Every line is
 *   exactly `lineHeight` tall, so `top = index * lineHeight` and indexing is
 *   O(1) with no measurement at all. This is the path a classic user is on all
 *   day and it must cost nothing.
 * * **Wrap path.** Heights vary, so a Fenwick tree over 4 096-line chunks holds
 *   measured-or-estimated chunk heights; measured chunks replace estimates as
 *   they scroll through and the scrollbar corrects smoothly. A Fenwick rather
 *   than a running array because a measurement in the middle of a 5 M-line log
 *   must not be an O(n) prefix rewrite on a scroll frame.
 *
 * Neither path ever produces a pixel height for the whole document: see
 * `scrollbar.ts` for why there is no spacer div.
 *
 * # Why lines are split here rather than trusted from the wire
 *
 * `log_range` answers `{ runs, lineStarts }` (CONTRACTS §11). `lineStarts` is a
 * `Vec<u32>` and the repo's other producer of that name —
 * `stratum_workspace::Document::line_starts` — means *byte* offsets with a
 * sentinel. Byte offsets and the UTF-16 code units a JS string is indexed in
 * are not the same number the moment a log contains a non-ASCII character, and
 * a log of a `label var` in French contains one. So the line split here is done
 * from the run text itself, which cannot be ambiguous, and `lineStarts` is used
 * only as a **cross-check**: a disagreement bumps
 * {@link LogCounters.lineStartMismatches} rather than corrupting the window.
 * Flagged in W16's return; when the contract pins the unit, this becomes an
 * assertion instead of a counter.
 */

import type { SessionId } from "../ipc/hand";
import { bridge } from "../platform/bridge";
import type { StyledRunView } from "../renderers/types";
import {
  DEFAULT_LOG_CAP_LINES,
  type ScrollKey,
  applyKey,
  clampPosition,
  maxPosition,
} from "./scrollbar";

// ---------------------------------------------------------------------------
// Shapes
// ---------------------------------------------------------------------------

/** One physical line of the scrollback. Terminators are not stored. */
export interface LogLine {
  readonly runs: readonly StyledRunView[];
}

/** `log_range`'s reply, as the structural minimum this module reads (§12). */
export interface LogRangeReply {
  readonly fromLine: number;
  readonly runs: readonly StyledRunView[];
  readonly lineStarts?: readonly number[];
}

/** CONTRACTS §11 `log_copy`. */
export type CopyFormat = "text" | "tsv" | "html" | "latex";

export interface LogHitView {
  readonly line: number;
  readonly col: number;
  readonly len: number;
  readonly preview: string;
}

export interface LogSearchOptions {
  readonly regex: boolean;
  readonly caseSensitive: boolean;
  readonly maxHits: number;
}

/**
 * The IPC boundary, injected exactly as W13 injects its `RunSink`.
 *
 * The default reaches `bridge()`; with no host every call rejects and the
 * window shows placeholders, which is the correct behaviour in a browser tab
 * and the reason this unit is developable before W17 exists.
 */
export interface LogSource {
  range(fromLine: number, toLine: number): Promise<LogRangeReply>;
  copy(fromLine: number, toLine: number, format: CopyFormat): Promise<string>;
  search(
    query: string,
    opts: LogSearchOptions,
  ): Promise<{ hits: readonly LogHitView[]; total: number }>;
}

export function hostLogSource(session: () => SessionId | undefined): LogSource {
  const need = (): SessionId => {
    const s = session();
    if (s === undefined) throw new Error("no session: the log is engine-side");
    return s;
  };
  return {
    range: (fromLine, toLine) =>
      bridge().invoke<LogRangeReply>("log_range", { session: need(), fromLine, toLine }),
    copy: (fromLine, toLine, format) =>
      bridge().invoke<string>("log_copy", { session: need(), fromLine, toLine, format }),
    search: (query, opts) =>
      bridge().invoke<{ hits: LogHitView[]; total: number }>("log_search", {
        session: need(),
        query,
        opts,
      }),
  };
}

// ---------------------------------------------------------------------------
// Constants (06 §15.2)
// ---------------------------------------------------------------------------

/** Rendered lines held in the DOM. */
export const WINDOW_LINES = 600;
/** Fetched beyond the rendered window on each side. */
export const PREFETCH_LINES = 2_000;
/** Fenwick granularity on the wrap path. */
export const CHUNK_LINES = 4_096;

// ---------------------------------------------------------------------------
// Counters (ADR-017 — assert these, never a duration)
// ---------------------------------------------------------------------------

export interface LogCounters {
  /** `log_range` calls. A scroll inside the resident window must add none. */
  fetches: number;
  /** Lines split out of run text. The cost that scales with the log. */
  linesDecoded: number;
  /** Times the resident window moved. */
  windowShifts: number;
  /** Fenwick point updates. Zero on the fast path, always. */
  heightUpdates: number;
  /** `lineStarts` disagreed with the newline split. See the header. */
  lineStartMismatches: number;
  /** Lines appended by a streaming `Output` event, with no round trip. */
  appended: number;
}

const ZERO: LogCounters = {
  fetches: 0,
  linesDecoded: 0,
  windowShifts: 0,
  heightUpdates: 0,
  lineStartMismatches: 0,
  appended: 0,
};

export const counters: LogCounters = { ...ZERO };

export function resetLogCounters(): void {
  Object.assign(counters, ZERO);
}

// ---------------------------------------------------------------------------
// Splitting runs into lines
// ---------------------------------------------------------------------------

/**
 * Splits a flat run list into physical lines.
 *
 * A run may contain any number of newlines, and a line may span any number of
 * runs — the two are independent, which is exactly why the frontend must never
 * assume "one run per line". A run boundary in the middle of a line survives as
 * two sibling spans so that copying the selection yields the original bytes
 * (06 §9.2), so a zero-length fragment is dropped rather than emitted.
 */
export function splitRunsIntoLines(runs: readonly StyledRunView[]): LogLine[] {
  const lines: LogLine[] = [];
  let current: StyledRunView[] = [];

  for (const run of runs) {
    let start = 0;
    for (;;) {
      const nl = run.text.indexOf("\n", start);
      if (nl < 0) {
        if (start < run.text.length) {
          current.push(start === 0 ? run : { text: run.text.slice(start), style: run.style });
        }
        break;
      }
      if (nl > start) current.push({ text: run.text.slice(start, nl), style: run.style });
      lines.push({ runs: current });
      current = [];
      start = nl + 1;
    }
  }
  // A trailing fragment with no terminator is still a line: the last line of a
  // log that has not yet ended in a newline is on screen and must be.
  if (current.length > 0) lines.push({ runs: current });

  counters.linesDecoded += lines.length;
  return lines;
}

/** The plain text of a line. The single flattening rule, mirroring `to_plain`. */
export function lineText(line: LogLine): string {
  let out = "";
  for (const run of line.runs) out += run.text;
  return out;
}

// ---------------------------------------------------------------------------
// Fenwick tree over chunks (the wrap path only)
// ---------------------------------------------------------------------------

/**
 * A Fenwick tree of chunk heights.
 *
 * Point update and prefix sum are both O(log n) over `ceil(5e6 / 4096) = 1 221`
 * chunks, so a measurement costs eleven adds. The array is 1-based, which is
 * the form the algorithm is actually correct in; index 0 is unused.
 */
class Fenwick {
  private readonly tree: Float64Array;
  private readonly values: Float64Array;

  constructor(readonly size: number) {
    this.tree = new Float64Array(size + 1);
    this.values = new Float64Array(size);
  }

  set(index: number, value: number): void {
    if (index < 0 || index >= this.size) return;
    const delta = value - (this.values[index] as number);
    if (delta === 0) return;
    this.values[index] = value;
    for (let i = index + 1; i <= this.size; i += i & -i) {
      this.tree[i] = (this.tree[i] as number) + delta;
    }
    counters.heightUpdates += 1;
  }

  get(index: number): number {
    return index < 0 || index >= this.size ? 0 : (this.values[index] as number);
  }

  /** Sum of chunks `[0, count)`. */
  prefix(count: number): number {
    let sum = 0;
    for (let i = Math.min(count, this.size); i > 0; i -= i & -i) {
      sum += this.tree[i] as number;
    }
    return sum;
  }

  /** The first chunk whose prefix sum exceeds `height`, and the leftover px. */
  locate(height: number): { chunk: number; rest: number } {
    let idx = 0;
    let remaining = height;
    let step = 1;
    while (step * 2 <= this.size) step *= 2;
    for (let bit = step; bit > 0; bit >>= 1) {
      const next = idx + bit;
      if (next <= this.size && (this.tree[next] as number) <= remaining) {
        idx = next;
        remaining -= this.tree[next] as number;
      }
    }
    return { chunk: Math.min(idx, this.size - 1), rest: remaining };
  }
}

// ---------------------------------------------------------------------------
// The window
// ---------------------------------------------------------------------------

export interface LogWindowOptions {
  readonly source?: LogSource;
  /** `--lh-code`, 20 px in the shipped tokens. */
  readonly lineHeight?: number;
  /** 06 §15.2: wrapping off is the default and it is the fast path. */
  readonly wrap?: boolean;
  readonly cap?: number;
  /** Notified whenever the resident window or the total changed. */
  readonly onChange?: () => void;
}

interface Resident {
  from: number;
  lines: LogLine[];
}

/**
 * The scrollback model for one surface.
 *
 * One instance per rendered log. Two windows on one session each hold their own
 * because "which lines are on screen" is a property of a viewport, not of the
 * session — 06 §13.2's rule that a webview holds no authoritative state applies
 * to the *content*, which stays in Rust, not to the viewport.
 */
export class LogWindow {
  private readonly source: LogSource;
  private readonly cap: number;
  private readonly onChange: () => void;
  private resident: Resident = { from: 0, lines: [] };
  private inflight: { from: number; to: number } | undefined;
  private heights: Fenwick | undefined;
  private total = 0;
  private pos = 0;
  private viewportRows = WINDOW_LINES;
  private stickToTail = true;

  lineHeight: number;
  wrap: boolean;

  constructor(options: LogWindowOptions = {}) {
    this.source = options.source ?? hostLogSource(() => undefined);
    this.lineHeight = options.lineHeight ?? 20;
    this.wrap = options.wrap ?? false;
    this.cap = options.cap ?? DEFAULT_LOG_CAP_LINES;
    this.onChange = options.onChange ?? ((): void => {});
  }

  // -- geometry ------------------------------------------------------------

  get totalLines(): number {
    return this.total;
  }

  get position(): number {
    return this.pos;
  }

  get viewport(): number {
    return this.viewportRows;
  }

  setViewport(rows: number): void {
    this.viewportRows = Math.max(1, rows);
    this.pos = clampPosition(this.metrics(), this.pos);
  }

  metrics(): { total: number; viewport: number; position: number } {
    return { total: this.total, viewport: this.viewportRows, position: this.pos };
  }

  /**
   * Move the viewport. Any explicit move breaks the tail lock, because a user
   * who has scrolled up to read something must not be yanked back down by the
   * next `Output` event — the single most-hated behaviour in a streaming log.
   */
  scrollTo(row: number): void {
    const next = clampPosition(this.metrics(), row);
    if (next === this.pos) return;
    this.pos = next;
    this.stickToTail = next >= maxPosition(this.metrics()) - 0.5;
    this.onChange();
  }

  scrollBy(rows: number): void {
    this.scrollTo(this.pos + rows);
  }

  key(k: ScrollKey): void {
    this.scrollTo(applyKey(this.metrics(), k));
  }

  /** Follow the tail again — the `⤓` affordance and what a fresh run does. */
  scrollToEnd(): void {
    this.pos = maxPosition(this.metrics());
    this.stickToTail = true;
    this.onChange();
  }

  get followingTail(): boolean {
    return this.stickToTail;
  }

  /**
   * Pixel offset of a line from the top of the document.
   *
   * The fast path is one multiply and is the only arithmetic a non-wrapping log
   * ever does. It is separate from the wrap path rather than a special case
   * inside it, because "is this the O(1) path" has to be answerable by reading
   * the code.
   */
  topOf(index: number): number {
    if (!this.wrap) return index * this.lineHeight;
    const fenwick = this.fenwick();
    const chunk = Math.floor(index / CHUNK_LINES);
    const within = index - chunk * CHUNK_LINES;
    const chunkHeight = fenwick.get(chunk);
    const perLine = chunkHeight > 0 ? chunkHeight / CHUNK_LINES : this.lineHeight;
    return fenwick.prefix(chunk) + within * perLine;
  }

  /** The inverse of {@link topOf}. */
  lineAtOffset(offsetPx: number): number {
    if (!this.wrap) return Math.floor(offsetPx / this.lineHeight);
    const fenwick = this.fenwick();
    const { chunk, rest } = fenwick.locate(offsetPx);
    const chunkHeight = fenwick.get(chunk);
    const perLine = chunkHeight > 0 ? chunkHeight / CHUNK_LINES : this.lineHeight;
    return Math.min(this.total, chunk * CHUNK_LINES + Math.floor(rest / perLine));
  }

  /**
   * Record a measured height for the chunk containing `index`.
   *
   * Only meaningful on the wrap path; on the fast path every line is the same
   * height by construction and a measurement would be a fact we already know.
   */
  measureChunk(index: number, heightPx: number): void {
    if (!this.wrap) return;
    this.fenwick().set(Math.floor(index / CHUNK_LINES), heightPx);
    this.onChange();
  }

  private fenwick(): Fenwick {
    this.heights ??= new Fenwick(Math.ceil(this.cap / CHUNK_LINES) + 1);
    return this.heights;
  }

  // -- content -------------------------------------------------------------

  /**
   * The total line count, from `SessionSnapshot.log_lines` or a `Cleared`
   * event. Growing keeps a tail-locked viewport at the tail.
   */
  setTotal(lines: number): void {
    const next = Math.max(0, Math.min(lines, this.cap));
    if (next === this.total) return;
    this.total = next;
    if (this.stickToTail) this.pos = maxPosition(this.metrics());
    else this.pos = clampPosition(this.metrics(), this.pos);
    this.onChange();
  }

  /**
   * Append streamed output at the tail, with no round trip.
   *
   * 06 §15.1 budgets 50 ms from the first log bytes to paint. A round trip to
   * ask Rust for lines it just sent us cannot fit in that, and would also make
   * the log's own arrival order depend on request scheduling. So `Output`
   * events land here directly and `log_range` is only ever used to go *back*.
   */
  append(runs: readonly StyledRunView[]): void {
    const lines = splitRunsIntoLines(runs);
    if (lines.length === 0) return;
    counters.appended += lines.length;

    const residentEnd = this.resident.from + this.resident.lines.length;
    if (residentEnd === this.total) {
      this.resident.lines.push(...lines);
      this.trimResident();
    }
    this.total = Math.min(this.cap, this.total + lines.length);
    if (this.stickToTail) this.pos = maxPosition(this.metrics());
    this.onChange();
  }

  /** Right-click ▸ Clear results. Not undoable, and Stata says so too. */
  clear(): void {
    this.resident = { from: 0, lines: [] };
    this.total = 0;
    this.pos = 0;
    this.stickToTail = true;
    this.heights = undefined;
    this.onChange();
  }

  /**
   * A resident line, or `undefined` when it is outside the window.
   *
   * `undefined` is the placeholder signal, and the caller renders a blank row
   * of the right height rather than waiting: 06 §15.1's "log jump of 10 k lines
   * ≤ 16 ms to placeholders, ≤ 80 ms filled" is only achievable if a jump
   * paints before the fetch resolves.
   */
  lineAt(index: number): LogLine | undefined {
    const at = index - this.resident.from;
    return at < 0 ? undefined : this.resident.lines[at];
  }

  get residentRange(): { from: number; to: number } {
    return { from: this.resident.from, to: this.resident.from + this.resident.lines.length };
  }

  /** The rendered slice for the current position, oldest first. */
  visibleLines(): { first: number; lines: (LogLine | undefined)[] } {
    const first = Math.max(0, Math.floor(this.pos));
    const count = Math.min(Math.ceil(this.viewportRows) + 1, Math.max(0, this.total - first));
    const lines: (LogLine | undefined)[] = [];
    for (let i = 0; i < count; i++) lines.push(this.lineAt(first + i));
    return { first, lines };
  }

  /**
   * Make sure the current viewport plus its prefetch margins are resident.
   *
   * Returns the promise so a test can await it; callers on a scroll frame
   * deliberately do not. A request that is already in flight for a covering
   * range is not reissued — the 2 000-line margin exists so that a wheel gesture
   * inside it costs zero `log_range` calls, and `counters.fetches` is where
   * that claim is checked.
   */
  async ensureVisible(): Promise<void> {
    const first = Math.max(0, Math.floor(this.pos));
    const last = Math.min(this.total, first + Math.ceil(this.viewportRows) + 1);
    const { from, to } = this.residentRange;
    if (this.total === 0) return;
    if (first >= from && last <= to) return;

    const want = {
      from: Math.max(0, first - PREFETCH_LINES),
      to: Math.min(this.total, last + PREFETCH_LINES),
    };
    const busy = this.inflight;
    if (busy !== undefined && want.from >= busy.from && want.to <= busy.to) return;

    this.inflight = want;
    counters.fetches += 1;
    try {
      const reply = await this.source.range(want.from, want.to);
      const lines = splitRunsIntoLines(reply.runs);
      this.checkLineStarts(reply, lines);
      this.resident = { from: reply.fromLine, lines };
      this.trimResident();
      counters.windowShifts += 1;
      this.onChange();
    } catch {
      // No host, or the range fell off the head of the ring. Placeholders are
      // the honest answer; a thrown error here would take down a scroll frame.
    } finally {
      if (this.inflight === want) this.inflight = undefined;
    }
  }

  /**
   * Copy ALWAYS goes through Rust (06 §15.2), so text outside the rendered
   * window is included and the copied bytes are identical to the log file.
   * There is deliberately no local fallback that concatenates resident lines:
   * a fallback that silently returns the visible 600 of a 40 000-line selection
   * is worse than an error, because the user does not find out until they paste.
   */
  copy(fromLine: number, toLine: number, format: CopyFormat = "text"): Promise<string> {
    return this.source.copy(fromLine, toLine, format);
  }

  searchLog(
    query: string,
    opts: LogSearchOptions,
  ): Promise<{ hits: readonly LogHitView[]; total: number }> {
    return this.source.search(query, opts);
  }

  /** Keeps the resident array bounded; the ring in Rust is the archive. */
  private trimResident(): void {
    const budget = WINDOW_LINES + 2 * PREFETCH_LINES;
    const excess = this.resident.lines.length - budget;
    if (excess <= 0) return;
    this.resident.lines.splice(0, excess);
    this.resident.from += excess;
  }

  private checkLineStarts(reply: LogRangeReply, lines: readonly LogLine[]): void {
    const starts = reply.lineStarts;
    if (starts === undefined) return;
    // A sentinel entry is permitted (stratum_workspace's `line_starts` emits
    // one), so both counts are accepted before this is called a mismatch.
    if (starts.length !== lines.length && starts.length !== lines.length + 1) {
      counters.lineStartMismatches += 1;
    }
  }
}
