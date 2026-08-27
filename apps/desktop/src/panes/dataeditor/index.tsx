/**
 * The Data Editor pane — spec §26, 06 §9.7, 06 §15.3.
 *
 * Everything below the pane chrome is `DataGridController`; this file is the
 * Solid shell around it: the Browse/Edit switch, the Filter-observations panel,
 * the status bar, and the `Open in window` verb.
 *
 * **`Open in window` is a verb, never a reflex.** §18's rule for graphs is the
 * house rule — "Nothing spawns a window on its own" — and a Data Editor that
 * opened its own window on first data would break the same promise. Spec §26
 * asks for "Data Editor on one monitor, do-file on another"; this is the button
 * that does it.
 *
 * **What this pane does NOT build.** 06 §9.7 wants a sidebar with "the **same**
 * Variables and Properties components as the main window (one implementation,
 * two mounts)". `panes/variables/**` and `panes/properties/**` belong to another
 * unit (docs/ownership.toml) and R0 says one owner per file, so this pane takes
 * them as slots — `props.sidebar` — and builds only the third panel, Filter
 * observations, which is the Data Editor's own. Wiring is one prop for whoever
 * owns those panes.
 */

import { type JSX, Show, createEffect, createSignal, onCleanup, onMount } from "solid-js";
import { render } from "solid-js/web";
import { registerPane } from "../../dock/panes";
import {
  type GridColumn,
  type VariableLike,
  columnsFromVariables,
  resetGridCounters,
} from "../../grid/engine";
import type { SurfacePreference } from "../../grid/paint";
import type { DatasetStateId, SessionId } from "../../ipc/hand";
import { bridge } from "../../platform/bridge";
import { Button, Field, PaneHeader, Segmented } from "../../ui";
import { DataGridController, type GridMode, type GridStatus } from "./controller";
import { type SortKey, cycleSortKey, dropOrder, filterLabel, orderLabel, setOrder } from "./order";
import { DataStatusBar } from "./status";

import "./dataeditor.css";

const MODES = [
  { value: "browse" as const, label: "Browse" },
  { value: "edit" as const, label: "Edit" },
];

export interface DataEditorProps {
  session: SessionId;
  frame?: string;
  state?: DatasetStateId;
  /** From `variables_list`. Empty until it arrives; the grid renders its chrome. */
  variables?: readonly VariableLike[];
  obs?: number;
  surface?: SurfacePreference;
  /** Test seam, passed straight to the page source. */
  fetchAsset?: (url: string, init?: { signal?: AbortSignal }) => Promise<Response>;
  /**
   * Where `replace <var> = <val> in <n>` goes. The default submits it as
   * `RunIntent::CommandBar`, which is what puts it in the log and the ledger.
   */
  onSubmit?: (command: string) => void;
  /** Overridden by the shell so the dock loses the panel as the window gains it. */
  onDetach?: () => void;
  /** Slots for 06 §9.7's shared sidebar. See the file comment. */
  sidebar?: JSX.Element;
  project?: string;
}

export function DataEditorPane(props: DataEditorProps): JSX.Element {
  const [mode, setMode] = createSignal<GridMode>("browse");
  const [notice, setNotice] = createSignal<string | undefined>(undefined);
  const [filterText, setFilterText] = createSignal("");
  const [filterOpen, setFilterOpen] = createSignal(false);
  const [keys, setKeys] = createSignal<readonly SortKey[]>([]);
  const [status, setStatus] = createSignal<GridStatus>({
    vars: 0,
    order: "Dataset",
    obs: 0,
    length: 0,
    filter: "Off",
    mode: "browse",
    surface: "canvas",
    cellsPerSecond: 0,
    capped: false,
  });

  let host: HTMLDivElement | undefined;
  let controller: DataGridController | undefined;
  let order: number | undefined;

  const submit = (command: string): void => {
    if (props.onSubmit !== undefined) {
      props.onSubmit(command);
      return;
    }
    // `RunIntent::CommandBar` (CONTRACTS §7). One path for every command the
    // product runs, so an edit is echoed, logged and recorded exactly as if the
    // user had typed it — which, reproducibly speaking, they did.
    void bridge()
      .invoke("exec_submit", {
        session: props.session,
        intent: { intent: "command_bar", text: command },
        inlineMode: "compact",
      })
      .catch((error: unknown) => setNotice(String(error)));
  };

  /**
   * The one path to a view order: declare the intent, get an `OrderId` back.
   *
   * Never a client-side sort. `06` §15.3 requires sorting to happen in Rust, and
   * A13 is the amendment that made it possible to ask for it in 60 bytes.
   */
  const applyOrder = async (nextKeys: readonly SortKey[], filter: string): Promise<void> => {
    const grid = controller;
    if (grid === undefined) return;
    const state = props.state ?? (0 as DatasetStateId);
    const frame = props.frame ?? "default";
    const columnIndexOf = (key: SortKey): number =>
      (props.variables ?? []).findIndex((v, i) => (v.idx ?? i) === key.idx);

    if (nextKeys.length === 0 && filter === "") {
      if (order !== undefined) {
        await dropOrder(props.session, order).catch(() => undefined);
        order = undefined;
      }
      setKeys([]);
      grid.setOrder(undefined, props.obs ?? 0, "Dataset", "Off", undefined);
      return;
    }

    const outcome = await setOrder(
      props.session,
      frame,
      nextKeys,
      filter === "" ? undefined : filter,
      state,
    );
    if (!outcome.ok) {
      setNotice(outcome.reason);
      return;
    }
    setNotice(undefined);
    const previous = order;
    order = outcome.result.order;
    setKeys(nextKeys);
    const primary = nextKeys[0];
    grid.setOrder(
      outcome.result.order,
      outcome.result.nRows,
      orderLabel(nextKeys),
      filterLabel(filter === "" ? undefined : filter),
      primary === undefined ? undefined : { columnIndex: columnIndexOf(primary), dir: primary.dir },
    );
    // Free the superseded handle only after the new one is live, so a failed
    // sort leaves the user looking at the order they had.
    if (previous !== undefined) await dropOrder(props.session, previous).catch(() => undefined);
  };

  onMount(() => {
    const element = host;
    if (element === undefined) return;

    const grid = new DataGridController({
      session: props.session,
      frame: props.frame ?? "default",
      state: props.state ?? (0 as DatasetStateId),
      ...(props.surface === undefined ? {} : { surface: props.surface }),
      ...(props.fetchAsset === undefined ? {} : { fetchAsset: props.fetchAsset }),
      onEdit: submit,
      onStatus: setStatus,
      onNotice: setNotice,
      onStateAdvanced: (next) => grid.invalidate(next),
      onHeaderActivate: (index) => {
        const column = (props.variables ?? [])[index];
        if (column === undefined) return;
        void applyOrder(
          cycleSortKey(keys(), { idx: column.idx ?? index, name: column.name }),
          filterText().trim(),
        );
      },
    });
    controller = grid;
    element.appendChild(grid.element);

    const measure = (): void => {
      const rect = element.getBoundingClientRect();
      // jsdom reports every rect as zero; a nominal viewport keeps the engine's
      // window non-empty so the pane is testable without a layout engine.
      grid.layout(rect.width || 960, rect.height || 480);
    };
    measure();

    const observer =
      typeof ResizeObserver === "function" ? new ResizeObserver(() => measure()) : undefined;
    observer?.observe(element);

    onCleanup(() => {
      observer?.disconnect();
      grid.dispose();
      controller = undefined;
    });
  });

  // Columns and row count arrive after `variables_list` answers; the grid draws
  // its chrome before they do rather than showing a spinner over an empty pane.
  createEffect(() => {
    const vars = props.variables ?? [];
    const columns: GridColumn[] = columnsFromVariables(vars);
    controller?.setColumns(columns);
  });
  createEffect(() => {
    controller?.setRowCount(props.obs ?? 0);
  });
  createEffect(() => {
    controller?.setMode(mode());
  });
  // The dataset advanced under the grid (`state_changed`, routed into this
  // prop by the shell): every resident page describes a state that no longer
  // exists, so drop and refetch (CONTRACTS §8.1) rather than repaint stale
  // cells. `seen` starts as the state the controller was constructed with, so
  // the effect's first run — same value — costs nothing.
  let seen: DatasetStateId | undefined | "unset" = "unset";
  createEffect(() => {
    const next = props.state;
    if (seen === "unset") {
      seen = next;
      return;
    }
    if (next === undefined || next === seen) return;
    seen = next;
    controller?.invalidate(next);
  });

  return (
    <section class="dataeditor" data-pane="dataeditor">
      <PaneHeader
        title="Data"
        actions={
          <>
            <Segmented
              options={MODES}
              value={mode()}
              onChange={(next) => setMode(next)}
              label="Data editor mode"
            />
            <Button
              variant="quiet"
              icon="search"
              aria-expanded={filterOpen()}
              onClick={() => setFilterOpen((v) => !v)}
            >
              Filter
            </Button>
            <Button
              variant="quiet"
              icon="detach"
              title="Open the Data Editor in its own window (spec §26)"
              onClick={() =>
                props.onDetach === undefined
                  ? void bridge().openPaneWindow({
                      role: "pane",
                      paneId: "dataeditor",
                      // The label format `dock/detach.ts` uses, so a window opened
                      // from here and one opened by dragging the tab out are the
                      // same window rather than two.
                      label: `${props.project ?? "stratum"}:pane:dataeditor`,
                    })
                  : props.onDetach()
              }
            >
              Open in window
            </Button>
          </>
        }
      />

      <Show when={filterOpen()}>
        <form
          class="dataeditor__filter"
          onSubmit={(event) => {
            event.preventDefault();
            void applyOrder(keys(), filterText().trim());
          }}
        >
          <Field
            label="Filter observations"
            prompt="if"
            value={filterText()}
            placeholder="foreign == 1 & price > 5000"
            spellcheck={false}
            onInput={(event) => setFilterText(event.currentTarget.value)}
          />
          <Button type="submit" variant="accent">
            Apply filter
          </Button>
          <Button
            variant="quiet"
            onClick={() => {
              setFilterText("");
              void applyOrder(keys(), "");
            }}
          >
            Remove filter
          </Button>
        </form>
      </Show>

      <div class="dataeditor__body">
        <div class="dataeditor__grid" ref={host} />
        <Show when={props.sidebar !== undefined}>
          <aside class="dataeditor__sidebar">{props.sidebar}</aside>
        </Show>
      </div>

      <DataStatusBar status={status()} notice={notice()} />
    </section>
  );
}

/**
 * Registers the pane with the dock.
 *
 * Called from this module rather than from the shell, per `dock/panes.ts`:
 * "W13–W21 call this from their own modules." Solid is mounted imperatively
 * because `PaneMount` hands out a plain element, and the returned disposer is
 * the one `registerPane` registers.
 */
export function registerDataEditorPane(props: DataEditorProps): () => void {
  return registerPane("dataeditor", (host, register) => {
    resetGridCounters();
    register(render(() => <DataEditorPane {...props} />, host));
  });
}
