/**
 * The Variables window — spec §7, §20; 06 §9.4.
 *
 * Four gestures out of [GSM] 2 on this machine, and they are the pane:
 *
 * > Click once on a variable in the Variables window to select it. …
 * > Double-clicking on a variable in the Variables window inserts it into the
 * > Command window at the insertion point. …
 * > To the left of each variable is a small area that, when clicked, will paste
 * > the variable into the Command window. …
 * > You can change the display order of the variables in the Variables window by
 * > clicking on any column header. The first click sorts in ascending order, the
 * > second click sorts in descending order, and the third click puts the
 * > variables back in dataset order.
 *
 * Everything mechanical lives beside this file — `sort.ts` owns the three-state
 * rule and the permutation, `selection.ts` owns the set and its primary,
 * `varlist.ts` composes the commands, `columns.ts` owns the columns and the
 * filter haystack — so this file is layout, gestures and counters. That split is
 * what lets the same behaviours be asserted without a DOM and lets the Data
 * Editor's sidebar (06 §9.7, "one implementation, two mounts") mount the very
 * same component.
 *
 * # Why the rows are windowed
 *
 * `set maxvar` reaches 120 000 and a real analysis file routinely carries a few
 * thousand variables. A `<For>` over every row rebuilds thousands of DOM nodes
 * on a hover, a selection or a filter keystroke, which is the O(rows)
 * interaction-path work PRODUCT_SPEC §0a forbids outright. So rows are a fixed
 * `--h-grid-row` tall (the same 22 px the History pane uses), the visible slice
 * is computed from `scrollTop`, and {@link variablesCounters.rowsRendered}
 * proves the claim: filtering a 5 000-variable frame renders a screenful, not
 * five thousand.
 *
 * Unlike the log this pane *does* use a spacer div, and deliberately: 120 000 ×
 * 22 px is 2.6 M px, two orders of magnitude below the ~33.5 M px element cap
 * `log/scrollbar.ts` documents. A synthetic scrollbar here would be complexity
 * bought for a limit that cannot be reached.
 */

import { For, type JSX, Show, createEffect, createMemo, createSignal, onCleanup } from "solid-js";
import { render } from "solid-js/web";
import { insertVarlist, sendToCommand } from "../../commandbar/handle";
import { submitCommand } from "../../commandbar/submit";
import { registerPane } from "../../dock/panes";
import { setKeyContext } from "../../keys/context";
import { type VariableRow, variables } from "../../state/vars";
import { Icon, Menu, PaneHeader, Popover } from "../../ui";
import { FILTER_MODES, type FilterMode, type FilterSpec, matchesFilter } from "../history/filter";
import {
  DEFAULT_VISIBLE_COLUMNS,
  VAR_COLUMNS,
  type VarColumn,
  type VarColumnId,
  haystackOf,
  visibleColumns,
} from "./columns";
import {
  clearSelection,
  extendSelection,
  pruneSelection,
  selectAll,
  selectOnly,
  toggleSelection,
  variableSelection,
} from "./selection";
import { DATASET_ORDER, type SortState, ariaSort, countSort, displayOrder, nextSort } from "./sort";
import {
  type VarlistStyle,
  confirmationText,
  dropCommand,
  dropLabel,
  keepCommand,
  keepLabel,
  varlistText,
} from "./varlist";

import "./variables.css";

/** `--h-grid-row`. Read as a constant because the row height is a design token,
 *  not a measurement: a pane that measures its own rows every frame is a pane
 *  that reflows on every scroll. */
export const ROW_HEIGHT = 22;

/** Rows drawn above and below the viewport, so a wheel tick never shows a gap. */
export const OVERSCAN = 6;

/**
 * The three flags the selection rule reads. A `MouseEvent` and a `KeyboardEvent`
 * both satisfy it structurally, which is how one handler serves click and key.
 */
interface SelectionModifiers {
  readonly shiftKey: boolean;
  readonly metaKey: boolean;
  readonly ctrlKey: boolean;
}

export interface VariablesCounters {
  /** Row elements built. The windowing claim; never proportional to the frame. */
  rowsRendered: number;
  /** Filter evaluations. One per row per filter change, never per render. */
  filterPasses: number;
  /** Variables sent to the Command window by the paste column or a double click. */
  pastes: number;
  /** Commands this pane issued (`keep`/`drop`). Header clicks must add none. */
  commands: number;
}

const ZERO: VariablesCounters = { rowsRendered: 0, filterPasses: 0, pastes: 0, commands: 0 };
export const variablesCounters: VariablesCounters = { ...ZERO };
export function resetVariablesCounters(): void {
  Object.assign(variablesCounters, ZERO);
}

export interface VariablesPaneProps {
  /** Defaults to the shared store; the Data Editor sidebar and tests pass rows. */
  rows?: readonly VariableRow[];
  /** `Preferences`. Absent hides the item rather than showing a dead one. */
  onPreferences?: () => void;
  /**
   * The confirm for `keep`/`drop`. [GSM] 2: "You will be asked for
   * confirmation." Injected so a test can answer without a real dialog, and so
   * the host can raise a native one (06 §13.4).
   */
  confirm?: (message: string) => boolean;
}

/** Ascending is `▲`, descending `▼`, dataset order nothing at all. */
const SORT_GLYPH: Readonly<Record<"ascending" | "descending" | "none", string>> = {
  ascending: "▲",
  descending: "▼",
  none: "",
};

export function VariablesPane(props: VariablesPaneProps): JSX.Element {
  const [sort, setSort] = createSignal<SortState>(DATASET_ORDER);
  const [filter, setFilter] = createSignal<FilterSpec>({ query: "", mode: "any" });
  const [shown, setShown] = createSignal<ReadonlySet<VarColumnId>>(
    new Set(DEFAULT_VISIBLE_COLUMNS),
  );
  const [style, setStyle] = createSignal<VarlistStyle>("explicit");
  const [scrollTop, setScrollTop] = createSignal(0);
  const [viewportPx, setViewportPx] = createSignal(ROW_HEIGHT * 24);
  const [rowMenuAt, setRowMenuAt] = createSignal<{ x: number; y: number } | undefined>(undefined);
  const [headerMenuAt, setHeaderMenuAt] = createSignal<{ x: number; y: number } | undefined>(
    undefined,
  );
  const [magnifierAt, setMagnifierAt] = createSignal<{ x: number; y: number } | undefined>(
    undefined,
  );
  /** Roving tabindex: the list is one tab stop and the arrows move inside it. */
  const [focusRow, setFocusRow] = createSignal(0);
  let scroller: HTMLDivElement | undefined;

  const all = (): readonly VariableRow[] => props.rows ?? variables.rows;
  const columns = createMemo(() => visibleColumns(shown()));

  /**
   * Lowercased column text per row, computed once per (rows × columns) change.
   *
   * The filter runs on every keystroke; `toLowerCase()` on five columns of five
   * thousand rows per keystroke is 25 000 allocations a character. Precomputing
   * is §0a's "precompute, cache and index aggressively" applied to the one loop
   * in this pane that is genuinely O(rows).
   */
  const haystacks = createMemo(() => {
    const cols = columns();
    return all().map((row) => haystackOf(row, cols));
  });

  /** The rows on screen after filtering, still in dataset order. */
  const filtered = createMemo(() => {
    const spec = filter();
    const hay = haystacks();
    const rows = all();
    const out: VariableRow[] = [];
    for (let i = 0; i < rows.length; i++) {
      variablesCounters.filterPasses += 1;
      const row = rows[i];
      if (row === undefined) continue;
      if (!matchesFilter(spec, row.name, hay[i] ?? [row.name])) continue;
      out.push(row);
    }
    return out;
  });

  /** Display order: the filtered rows through the three-state sort. */
  const displayed = createMemo(() => {
    const rows = filtered();
    const state = sort();
    const column = state.column === undefined ? undefined : columnById_(state.column);
    const order = displayOrder(rows, state, column === undefined ? undefined : column.value);
    return order.map((i) => rows[i] as VariableRow);
  });

  const displayedNames = createMemo(() => displayed().map((r) => r.name));

  /** Dataset order — what a varlist range means. Never the display order. */
  const datasetNames = createMemo(() => all().map((r) => r.name));

  const selected = (): readonly string[] => variableSelection().names;
  const isSelected = (name: string): boolean => variableSelection().names.includes(name);

  // A `keep`/`drop` removes variables from under the selection; the store's rows
  // change and the selection must follow, or Properties offers `label variable`
  // on a name the engine has never heard of.
  createEffect(() => {
    pruneSelection(datasetNames());
  });

  const firstRow = (): number => Math.max(0, Math.floor(scrollTop() / ROW_HEIGHT) - OVERSCAN);
  const rowCount = (): number => Math.ceil(viewportPx() / ROW_HEIGHT) + OVERSCAN * 2;
  const window_ = createMemo(() => {
    const rows = displayed();
    const from = Math.min(firstRow(), Math.max(0, rows.length - 1));
    return { from, rows: rows.slice(from, from + rowCount()) };
  });

  /**
   * The one row in the tab order, clamped into the rendered window. A scroll can
   * carry the focused row off screen, and a list whose only tab stop is not in
   * the DOM is a list `Tab` skips entirely.
   */
  const tabRow = createMemo(() => {
    const w = window_();
    return Math.max(w.from, Math.min(w.from + Math.max(0, w.rows.length - 1), focusRow()));
  });

  /**
   * The viewport height in pixels, from a `ResizeObserver` rather than a layout
   * read per frame. jsdom has neither the observer nor a layout, so the initial
   * `viewportPx` above stands there — which is why the windowing tests assert a
   * bounded row count rather than an exact one.
   */
  const measure = (element: HTMLElement): void => {
    if (typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(() => {
      if (element.clientHeight > 0) setViewportPx(element.clientHeight);
    });
    observer.observe(element);
    onCleanup(() => observer.disconnect());
  };

  const onHeaderClick = (column: VarColumnId): void => {
    setSort((current) => {
      const next = nextSort(current, column);
      countSort();
      return next;
    });
  };

  const paste = (names: readonly string[]): void => {
    if (names.length === 0) return;
    variablesCounters.pastes += 1;
    insertVarlist(names);
  };

  const onRowClick = (row: VariableRow, event: SelectionModifiers): void => {
    if (event.shiftKey) extendSelection(displayedNames(), row.name);
    else if (event.metaKey || event.ctrlKey) toggleSelection(row.name);
    else selectOnly(row.name);
  };

  /** [GSM] 2: a double click inserts "at the insertion point", multiples space-separated. */
  const onRowDoubleClick = (row: VariableRow): void => {
    const names = isSelected(row.name) && selected().length > 1 ? selected() : [row.name];
    paste(names);
  };

  /**
   * Move the roving focus. The rows are windowed, so the row being moved to may
   * not be in the DOM yet: scroll it into the window first — O(1), it is one
   * `scrollTop` write and one slice — and then focus the element that renders.
   */
  const focusRowAt = (to: number): void => {
    const rows = displayed();
    if (rows.length === 0) return;
    const next = Math.max(0, Math.min(rows.length - 1, to));
    setFocusRow(next);
    if (scroller !== undefined) {
      const top = next * ROW_HEIGHT;
      const height = viewportPx();
      if (top < scrollTop()) scroller.scrollTop = top;
      else if (top + ROW_HEIGHT > scrollTop() + height)
        scroller.scrollTop = top + ROW_HEIGHT - height;
      // Solid re-renders the window synchronously from this write; the native
      // scroll event that follows would be a frame too late to focus from.
      setScrollTop(scroller.scrollTop);
    }
    const name = rows[next]?.name;
    scroller?.querySelector<HTMLElement>(`[data-variables-row="${name}"]`)?.focus();
  };

  /**
   * The keyboard equivalents of the three pointer gestures on a row, one key
   * each so nothing is reachable by mouse only (06 §17):
   *
   * * `Space` is the single click — select, with Shift and Mod meaning what
   *   they mean on the mouse.
   * * `Enter` is the double click — insert the varlist at the Command caret.
   * * `Mod+Enter` is the 14 px paste column — this one variable, no selection
   *   change. The `→` button keeps `tabIndex={-1}` because a tab stop per row
   *   would make Tab useless in a 5 000-variable frame; this is its equivalent.
   */
  const onRowKeyDown = (row: VariableRow, index: number, event: KeyboardEvent): void => {
    switch (event.key) {
      case "ArrowDown":
        event.preventDefault();
        focusRowAt(index + 1);
        break;
      case "ArrowUp":
        event.preventDefault();
        focusRowAt(index - 1);
        break;
      case "Home":
        event.preventDefault();
        focusRowAt(0);
        break;
      case "End":
        event.preventDefault();
        focusRowAt(displayed().length - 1);
        break;
      case " ":
        event.preventDefault();
        onRowClick(row, event);
        break;
      case "Enter":
        event.preventDefault();
        if (event.metaKey || event.ctrlKey) paste([row.name]);
        else onRowDoubleClick(row);
        break;
      default:
        break;
    }
  };

  const issue = (command: string): void => {
    variablesCounters.commands += 1;
    void submitCommand(command, "menu");
  };

  const confirm = (message: string): boolean =>
    props.confirm?.(message) ?? globalThis.confirm?.(message) ?? false;

  const onRowMenu = (id: string): void => {
    setRowMenuAt(undefined);
    const names = selected();
    switch (id) {
      case "keep": {
        const command = keepCommand(names, datasetNames(), style());
        if (confirm(confirmationText("keep", command))) issue(command);
        break;
      }
      case "drop": {
        const command = dropCommand(names, datasetNames(), style());
        if (confirm(confirmationText("drop", command))) issue(command);
        break;
      }
      case "copy":
        void navigator.clipboard?.writeText?.(varlistText(names, datasetNames(), style()));
        break;
      case "selectAll":
        selectAll(displayedNames());
        break;
      case "send":
        // "Send varlist to Command window" replaces; the paste column inserts.
        // Two different verbs in the manual, two different verbs here.
        sendToCommand(varlistText(names, datasetNames(), style()));
        break;
      case "compact":
        setStyle(style() === "compact" ? "explicit" : "compact");
        break;
      case "preferences":
        props.onPreferences?.();
        break;
      default:
        break;
    }
  };

  return (
    <section
      class="vars"
      data-pane="variables"
      onFocusIn={() => setKeyContext({ variablesFocus: true })}
      onFocusOut={() => setKeyContext({ variablesFocus: false })}
    >
      <PaneHeader title="Variables" />

      <div
        // This pane is a virtualised CSS grid and deliberately not a `<table>`:
        // see the header of `variables.css`. The rule is also unsatisfiable as
        // written — it fires on `<tr role="row">` too, i.e. on the very element
        // it recommends.
        // biome-ignore lint/a11y/useSemanticElements: a virtualised grid, not a `<table>`
        class="vars__grid"
        role="grid"
        aria-label="Variables"
        aria-rowcount={displayed().length + 1}
        aria-multiselectable="true"
        style={{
          "--vars-tracks": `14px ${columns()
            .map((c) => c.track)
            .join(" ")}`,
        }}
      >
        {/* The header is a row of `columnheader` cells, not a `<table>` head:
            each one holds a three-state control that has to be reachable from
            the keyboard, and the cell state (`aria-sort`) and the control are
            two different things. */}
        <div
          // This pane is a virtualised CSS grid and deliberately not a `<table>`:
          // see the header of `variables.css`. The rule is also unsatisfiable as
          // written — it fires on `<tr role="row">` too, i.e. on the very element
          // it recommends.
          // biome-ignore lint/a11y/useSemanticElements: a virtualised grid, not a `<table>`
          class="vars__head"
          role="row"
          aria-rowindex={1}
          tabIndex={-1}
          onContextMenu={(event) => {
            event.preventDefault();
            setHeaderMenuAt({ x: event.clientX, y: event.clientY });
          }}
        >
          <span
            class="vars__head-cell vars__head-cell--paste"
            role="columnheader"
            aria-label="Paste"
          />
          <For each={columns()}>
            {(column) => (
              /* The cell states the order, the button changes it. A `<button>`
                 carrying `role="columnheader"` was neither: the role stripped
                 the button semantics a screen-reader user needs to know the
                 header is pressable at all. */
              <div
                // This pane is a virtualised CSS grid and deliberately not a `<table>`:
                // see the header of `variables.css`. The rule is also unsatisfiable as
                // written — it fires on `<tr role="row">` too, i.e. on the very element
                // it recommends.
                // biome-ignore lint/a11y/useSemanticElements: a sortable header cell, not a `<th>`
                class="vars__head-cell"
                role="columnheader"
                aria-sort={ariaSort(sort(), column.id)}
                data-variables-header={column.id}
              >
                <button
                  type="button"
                  class="vars__head-button"
                  data-variables-sort={column.id}
                  onClick={() => onHeaderClick(column.id)}
                >
                  {column.header}
                  <span class="vars__sort" aria-hidden="true">
                    {SORT_GLYPH[ariaSort(sort(), column.id)]}
                  </span>
                </button>
              </div>
            )}
          </For>
        </div>

        {/* Three wrapper elements sit between the grid and its rows — the
            scroller, the spacer that gives the bar something to travel over and
            the translated window. `presentation` makes them transparent to
            assistive technology, so the grid owns the rows directly. */}
        <div
          class="vars__scroll"
          data-variables-rows
          role="presentation"
          ref={(element) => {
            scroller = element;
            measure(element);
          }}
          onScroll={(event) => setScrollTop(event.currentTarget.scrollTop)}
          onContextMenu={(event) => {
            event.preventDefault();
            setRowMenuAt({ x: event.clientX, y: event.clientY });
          }}
        >
          <div
            class="vars__spacer"
            role="presentation"
            style={{ height: `${displayed().length * ROW_HEIGHT}px` }}
          >
            <div
              class="vars__window"
              role="presentation"
              style={{ transform: `translateY(${window_().from * ROW_HEIGHT}px)` }}
            >
              <For each={window_().rows}>
                {(row, index) => {
                  variablesCounters.rowsRendered += 1;
                  return (
                    <div
                      // This pane is a virtualised CSS grid and deliberately not a `<table>`:
                      // see the header of `variables.css`. The rule is also unsatisfiable as
                      // written — it fires on `<tr role="row">` too, i.e. on the very element
                      // it recommends.
                      // biome-ignore lint/a11y/useSemanticElements: a virtualised grid row, not a `<tr>`
                      class="vars__row"
                      role="row"
                      // The header is row 1, so the data rows start at 2.
                      aria-rowindex={window_().from + index() + 2}
                      aria-selected={isSelected(row.name)}
                      tabIndex={window_().from + index() === tabRow() ? 0 : -1}
                      data-variables-row={row.name}
                      data-selected={isSelected(row.name) ? "" : undefined}
                      draggable
                      onKeyDown={(event) => onRowKeyDown(row, window_().from + index(), event)}
                      onDragStart={(event) => {
                        const names = isSelected(row.name) ? selected() : [row.name];
                        event.dataTransfer?.setData(
                          "text/plain",
                          varlistText(names, datasetNames(), style()),
                        );
                      }}
                      onClick={(event) => {
                        setFocusRow(window_().from + index());
                        onRowClick(row, event);
                      }}
                      onDblClick={() => onRowDoubleClick(row)}
                    >
                      {/* The 14 px paste column. One click, one variable, no
                          selection change — "loved and cheap" (06 §9.4). */}
                      <button
                        type="button"
                        class="vars__paste"
                        tabIndex={-1}
                        aria-label={`Paste ${row.name} into the Command window`}
                        data-variables-paste={row.name}
                        onClick={(event) => {
                          event.stopPropagation();
                          paste([row.name]);
                        }}
                      >
                        →
                      </button>
                      <For each={columns()}>
                        {(column) => (
                          <span class={`vars__cell vars__cell--${column.id}`}>
                            {column.value(row)}
                          </span>
                        )}
                      </For>
                    </div>
                  );
                }}
              </For>
            </div>
          </div>

          <Show when={displayed().length === 0}>
            <p class="vars__empty">
              {all().length === 0 ? "No data in memory." : "No variables match the filter."}
            </p>
          </Show>
        </div>
      </div>

      <div class="vars__filter">
        <button
          type="button"
          class="vars__magnifier"
          aria-label="Filter options"
          aria-haspopup="menu"
          data-variables-magnifier
          onClick={(event) => {
            const rect = event.currentTarget.getBoundingClientRect();
            setMagnifierAt({ x: rect.left, y: rect.bottom });
          }}
        >
          <Icon name="search" />
          <Icon name="chevron-down" />
        </button>
        <input
          class="vars__query"
          type="search"
          value={filter().query}
          placeholder="Filter variables"
          aria-label="Filter variables"
          data-variables-filter
          onInput={(event) => setFilter({ ...filter(), query: event.currentTarget.value })}
        />
      </div>

      <Popover
        open={rowMenuAt() !== undefined}
        anchor={rowMenuAt() ?? { x: 0, y: 0 }}
        onClose={() => setRowMenuAt(undefined)}
        label="Variable actions"
      >
        <Menu
          label="Variable actions"
          items={[
            { id: "keep", label: keepLabel(selected()), disabled: selected().length === 0 },
            { id: "drop", label: dropLabel(selected()), disabled: selected().length === 0 },
            { id: "s1", label: "", separator: true },
            { id: "copy", label: "Copy varlist", disabled: selected().length === 0 },
            { id: "selectAll", label: "Select all" },
            {
              id: "send",
              label: "Send varlist to Command window",
              disabled: selected().length === 0,
            },
            {
              id: "compact",
              label: `${style() === "compact" ? "✓ " : "   "}Output compact varlist`,
            },
            { id: "s2", label: "", separator: true },
            {
              id: "preferences",
              label: "Preferences",
              disabled: props.onPreferences === undefined,
            },
          ]}
          onSelect={onRowMenu}
        />
      </Popover>

      <Popover
        open={headerMenuAt() !== undefined}
        anchor={headerMenuAt() ?? { x: 0, y: 0 }}
        onClose={() => setHeaderMenuAt(undefined)}
        label="Columns"
      >
        <Menu
          label="Columns"
          items={VAR_COLUMNS.map((column) => ({
            id: column.id,
            label: `${shown().has(column.id) || column.required === true ? "✓ " : "   "}${column.header}`,
            disabled: column.required === true,
          }))}
          onSelect={(id) => {
            setHeaderMenuAt(undefined);
            const next = new Set(shown());
            if (next.has(id as VarColumnId)) next.delete(id as VarColumnId);
            else next.add(id as VarColumnId);
            setShown(next);
          }}
        />
      </Popover>

      <Popover
        open={magnifierAt() !== undefined}
        anchor={magnifierAt() ?? { x: 0, y: 0 }}
        onClose={() => setMagnifierAt(undefined)}
        label="Filter options"
      >
        <Menu
          label="Filter options"
          items={FILTER_MODES.map((m) => ({
            id: m.value,
            label: `${filter().mode === m.value ? "✓ " : "   "}${m.label}`,
          }))}
          onSelect={(id) => {
            setMagnifierAt(undefined);
            if (FILTER_MODES.some((m) => m.value === id)) {
              setFilter({ ...filter(), mode: id as FilterMode });
            }
          }}
        />
      </Popover>
    </section>
  );
}

/** Local lookup, so the memo above does not import `columnById`'s throw path. */
function columnById_(id: VarColumnId): VarColumn | undefined {
  return VAR_COLUMNS.find((c) => c.id === id);
}

/** Registers the pane with W12's dock. Returns the disposer. */
export function registerVariablesPane(props: VariablesPaneProps = {}): () => void {
  return registerPane(
    "variables",
    (host, register) => {
      register(render(() => <VariablesPane {...props} />, host));
    },
    "Variables",
  );
}

export { clearSelection, variableSelection };
