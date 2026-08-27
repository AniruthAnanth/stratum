/**
 * The History pane — spec §7, §11; 06 §9.3. Stata 18 renamed Review → History
 * and we use both names, as 06 does.
 *
 * The two gestures this pane exists for, from [GSM] 2 on this machine:
 *
 * > • Click once on a past command to copy it to the Command window, replacing
 * >   the contents of the Command window.
 * > • Double-click on a past command to resubmit it. Executing the command adds
 * >   the command to the bottom of the History window.
 *
 * Those are in twenty-year fingers and they are not negotiable. Everything else
 * here — the two columns, the red failures, the filter, the context menu — is
 * furniture around them.
 *
 * # Why a single click must not run
 *
 * A `bootstrap, reps(1000)` in the History list is forty seconds of compute. A
 * pane that ran it on a single click would be a pane people stop clicking in.
 * The separation is asserted directly in `history.test.tsx`: one click submits
 * nothing, and the counter proves it rather than the absence of a visible
 * effect.
 *
 * # Cut / Delete / Clear all
 *
 * 06 §9.3 asks for Stata's verbs "exactly", and three of them remove rows.
 * `state/history.ts` is W12's and has no removal API — `appendHistory` and a
 * test-seam reset are the whole surface — so removal here is a **per-window
 * suppression set**, which is correct in one window and wrong across two
 * (06 §13.1: the windows share one history). Said out loud rather than hidden:
 * it is escalated in W16's return, and the fix is one exported function in
 * `state/history.ts`, at which point `removed` below is deleted.
 */

import { For, type JSX, Show, createMemo, createSignal } from "solid-js";
import { render } from "solid-js/web";
import { sendToCommand } from "../../commandbar/handle";
import { sendHistoryToDoFile } from "../../commandbar/promote";
import { submitAll, submitCommand } from "../../commandbar/submit";
import { registerPane } from "../../dock/panes";
import { setKeyContext } from "../../keys/context";
import { type HistoryEntry, historyState } from "../../state/history";
import { Icon, Menu, PaneHeader, Popover } from "../../ui";
import { FILTER_MODES, type FilterMode, type FilterSpec, matchesFilter } from "./filter";

import "./history.css";

/**
 * The three flags the selection rule reads. A `MouseEvent` and a `KeyboardEvent`
 * both satisfy it structurally, which is how one click handler serves both.
 */
interface SelectionModifiers {
  readonly shiftKey: boolean;
  readonly metaKey: boolean;
  readonly ctrlKey: boolean;
}

export interface HistoryCounters {
  /** Commands resubmitted from this pane. A single click must never add one. */
  resubmits: number;
  /** Rows sent to the Command window by a single click. */
  loads: number;
  /** Filter evaluations. One per row per filter change, never per render. */
  filterPasses: number;
}

const ZERO: HistoryCounters = { resubmits: 0, loads: 0, filterPasses: 0 };
export const historyCounters: HistoryCounters = { ...ZERO };
export function resetHistoryCounters(): void {
  Object.assign(historyCounters, ZERO);
}

export interface HistoryPaneProps {
  /** Defaults to the shared store; a test or a preview passes its own. */
  entries?: readonly HistoryEntry[];
  /** `Save all…` / `Save selected…`. Absent disables those two items. */
  onSave?: (commands: readonly string[], scope: "all" | "selected") => void;
  /** Timestamp for §11's inserted comment. Injected so a golden can pin it. */
  now?: () => string;
}

const MENU_ITEMS = (enabled: {
  selection: boolean;
  save: boolean;
}): { id: string; label: string; disabled?: boolean; separator?: boolean }[] => [
  { id: "cut", label: "Cut", disabled: !enabled.selection },
  { id: "copy", label: "Copy", disabled: !enabled.selection },
  { id: "delete", label: "Delete", disabled: !enabled.selection },
  { id: "s1", label: "", separator: true },
  { id: "selectAll", label: "Select all" },
  { id: "clearAll", label: "Clear all" },
  { id: "s2", label: "", separator: true },
  { id: "do", label: "Do selected", disabled: !enabled.selection },
  { id: "send", label: "Send selected to Do-file Editor", disabled: !enabled.selection },
  { id: "s3", label: "", separator: true },
  { id: "saveAll", label: "Save all…", disabled: !enabled.save },
  { id: "saveSelected", label: "Save selected…", disabled: !enabled.save || !enabled.selection },
];

export function HistoryPane(props: HistoryPaneProps): JSX.Element {
  const [filter, setFilter] = createSignal<FilterSpec>({ query: "", mode: "any" });
  const [hideErrors, setHideErrors] = createSignal(false);
  const [selected, setSelected] = createSignal<ReadonlySet<number>>(new Set<number>());
  const [anchor, setAnchor] = createSignal<number | undefined>(undefined);
  const [removed, setRemoved] = createSignal<ReadonlySet<number>>(new Set<number>());
  const [menuAt, setMenuAt] = createSignal<{ x: number; y: number } | undefined>(undefined);
  /** Roving tabindex: one tab stop for the list, arrows move inside it. */
  const [focusRow, setFocusRow] = createSignal(0);
  const [magnifierAt, setMagnifierAt] = createSignal<{ x: number; y: number } | undefined>(
    undefined,
  );

  const all = (): readonly HistoryEntry[] => props.entries ?? historyState.entries;

  /**
   * The rows on screen, oldest first as Stata's Review pane is.
   *
   * A memo and not a render-time filter: the pane re-renders on selection and on
   * hover, and re-filtering 20 000 entries for a hover is exactly the O(rows)
   * interaction-path work §0a forbids.
   */
  const rows = createMemo(() => {
    const spec = filter();
    const hide = hideErrors();
    const gone = removed();
    const out: HistoryEntry[] = [];
    for (const entry of all()) {
      historyCounters.filterPasses += 1;
      if (gone.has(entry.seq)) continue;
      if (hide && entry.rc !== 0) continue;
      if (!matchesFilter(spec, entry.command, [entry.command, `_rc ${entry.rc}`])) continue;
      out.push(entry);
    }
    return out;
  });

  const selectedCommands = (): string[] =>
    rows()
      .filter((r) => selected().has(r.seq))
      .map((r) => r.command);

  /** Single click. Loads, never runs. Mod/Shift extend the selection. */
  const onRowClick = (entry: HistoryEntry, event: SelectionModifiers): void => {
    const list = rows();
    if (event.shiftKey && anchor() !== undefined) {
      const from = list.findIndex((r) => r.seq === anchor());
      const to = list.findIndex((r) => r.seq === entry.seq);
      if (from >= 0 && to >= 0) {
        const [lo, hi] = from <= to ? [from, to] : [to, from];
        setSelected(new Set(list.slice(lo, hi + 1).map((r) => r.seq)));
      }
    } else if (event.metaKey || event.ctrlKey) {
      const next = new Set(selected());
      if (next.has(entry.seq)) next.delete(entry.seq);
      else next.add(entry.seq);
      setSelected(next);
      setAnchor(entry.seq);
    } else {
      setSelected(new Set([entry.seq]));
      setAnchor(entry.seq);
    }
    historyCounters.loads += 1;
    sendToCommand(entry.command);
  };

  /** Double click. Resubmits, and the new row lands at the bottom. */
  const onRowDoubleClick = (entry: HistoryEntry): void => {
    historyCounters.resubmits += 1;
    void submitCommand(entry.command, "history");
  };

  /**
   * The keyboard equivalents of the two gestures above.
   *
   * `Enter` is the *single* click — load, never run. The reason a single click
   * must not run is the `bootstrap, reps(1000)` in the list, and that reason
   * does not change when the finger is on Enter rather than the mouse. The
   * double click's resubmit is `Mod+Enter`, which is deliberate friction.
   *
   * Focus moves by moving it, not by re-rendering: the rows are siblings in one
   * `<tbody>`, so the next row is `children[i]` and no lookup is O(rows).
   */
  const onRowKeyDown = (entry: HistoryEntry, index: number, event: KeyboardEvent): void => {
    const move = (to: number): void => {
      const list = rows();
      if (list.length === 0) return;
      const next = Math.max(0, Math.min(list.length - 1, to));
      setFocusRow(next);
      const sibling = (event.currentTarget as HTMLElement).parentElement?.children[next];
      if (sibling instanceof HTMLElement) sibling.focus();
      event.preventDefault();
    };
    switch (event.key) {
      case "ArrowDown":
        move(index + 1);
        break;
      case "ArrowUp":
        move(index - 1);
        break;
      case "Home":
        move(0);
        break;
      case "End":
        move(rows().length - 1);
        break;
      case " ":
        // Mod+click's verb: add this row to the selection without dropping it.
        event.preventDefault();
        onRowClick(entry, { shiftKey: false, metaKey: true, ctrlKey: false });
        break;
      case "Enter":
        event.preventDefault();
        if (event.metaKey || event.ctrlKey) onRowDoubleClick(entry);
        else onRowClick(entry, { shiftKey: event.shiftKey, metaKey: false, ctrlKey: false });
        break;
      default:
        break;
    }
  };

  const onMenuSelect = (id: string): void => {
    setMenuAt(undefined);
    const commands = selectedCommands();
    switch (id) {
      case "copy":
        void navigator.clipboard?.writeText?.(commands.join("\n"));
        break;
      case "cut":
        void navigator.clipboard?.writeText?.(commands.join("\n"));
        hideSelected();
        break;
      case "delete":
        hideSelected();
        break;
      case "selectAll":
        setSelected(new Set(rows().map((r) => r.seq)));
        break;
      case "clearAll":
        setRemoved(new Set(all().map((e) => e.seq)));
        setSelected(new Set<number>());
        break;
      case "do":
        // [GSM] 2: "Stata will attempt to run all the selected commands, even
        // those containing errors, and will not stop even if a command causes
        // an error." `submitAll` is written to that sentence.
        void submitAll(commands, "history");
        break;
      case "send":
        sendHistoryToDoFile(commands, props.now?.() ?? new Date().toISOString().slice(0, 16));
        break;
      case "saveAll":
        props.onSave?.(
          rows().map((r) => r.command),
          "all",
        );
        break;
      case "saveSelected":
        props.onSave?.(commands, "selected");
        break;
      default:
        break;
    }
  };

  const hideSelected = (): void => {
    const next = new Set(removed());
    for (const seq of selected()) next.add(seq);
    setRemoved(next);
    setSelected(new Set<number>());
  };

  return (
    <section
      class="hist"
      data-pane="history"
      onFocusIn={() => setKeyContext({ historyFocus: true })}
      onFocusOut={() => setKeyContext({ historyFocus: false })}
    >
      <PaneHeader title="History" />

      <div
        class="hist__scroll"
        data-history-rows
        onContextMenu={(event) => {
          event.preventDefault();
          setMenuAt({ x: event.clientX, y: event.clientY });
        }}
      >
        {/* `role="grid"` because the rows are selectable and arrow-navigable —
            `aria-selected` on a row is only defined inside one. The rule below
            asks for a `<table>`, which is what this already is; it fires on the
            element it recommends. */}
        {/* biome-ignore lint/a11y/useSemanticElements: a `<table>` already */}
        <table class="hist__table" role="grid" aria-multiselectable="true">
          <thead>
            <tr>
              <th scope="col" class="hist__th">
                Command
              </th>
              <th scope="col" class="hist__th hist__th--rc">
                _rc
              </th>
            </tr>
          </thead>
          <tbody>
            <For each={rows()}>
              {(entry, index) => (
                <tr
                  class="hist__row"
                  data-history-row={index()}
                  data-seq={entry.seq}
                  data-failed={entry.rc === 0 ? undefined : ""}
                  data-selected={selected().has(entry.seq) ? "" : undefined}
                  aria-selected={selected().has(entry.seq)}
                  tabindex={index() === Math.min(focusRow(), rows().length - 1) ? 0 : -1}
                  onClick={(event) => {
                    setFocusRow(index());
                    onRowClick(entry, event);
                  }}
                  onDblClick={() => onRowDoubleClick(entry)}
                  onKeyDown={(event) => onRowKeyDown(entry, index(), event)}
                >
                  <td class="hist__cmd">{entry.command}</td>
                  {/* Colour is never the only channel (06 §17): a failure also
                      prints its return code, which is the information anyway. */}
                  <td class="hist__rc">{entry.rc === 0 ? "" : entry.rc}</td>
                </tr>
              )}
            </For>
          </tbody>
        </table>

        <Show when={rows().length === 0}>
          <p class="hist__empty">
            {all().length === 0 ? "No commands yet." : "No commands match the filter."}
          </p>
        </Show>
      </div>

      <div class="hist__filter">
        <button
          type="button"
          class="hist__magnifier"
          aria-label="Filter options"
          aria-haspopup="menu"
          data-history-magnifier
          onClick={(event) => {
            const rect = event.currentTarget.getBoundingClientRect();
            setMagnifierAt({ x: rect.left, y: rect.bottom });
          }}
        >
          <Icon name="search" />
          <Icon name="chevron-down" />
        </button>
        <input
          class="hist__query"
          type="search"
          value={filter().query}
          placeholder="Filter"
          aria-label="Filter commands"
          data-history-filter
          onInput={(event) => setFilter({ ...filter(), query: event.currentTarget.value })}
        />
      </div>

      <Popover
        open={menuAt() !== undefined}
        anchor={menuAt() ?? { x: 0, y: 0 }}
        onClose={() => setMenuAt(undefined)}
        label="History actions"
      >
        <Menu
          label="History actions"
          items={MENU_ITEMS({
            selection: selected().size > 0,
            save: props.onSave !== undefined,
          })}
          onSelect={onMenuSelect}
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
          items={[
            ...FILTER_MODES.map((m) => ({
              id: m.value,
              label: `${filter().mode === m.value ? "✓ " : "   "}${m.label}`,
            })),
            { id: "sep", label: "", separator: true },
            { id: "hideErrors", label: `${hideErrors() ? "✓ " : "   "}Hide errors` },
          ]}
          onSelect={(id) => {
            setMagnifierAt(undefined);
            if (id === "hideErrors") setHideErrors(!hideErrors());
            else if (FILTER_MODES.some((m) => m.value === id)) {
              setFilter({ ...filter(), mode: id as FilterMode });
            }
          }}
        />
      </Popover>
    </section>
  );
}

/** Registers the pane with W12's dock. Returns the disposer. */
export function registerHistoryPane(props: HistoryPaneProps = {}): () => void {
  return registerPane(
    "history",
    (host, register) => {
      register(render(() => <HistoryPane {...props} />, host));
    },
    "History",
  );
}
