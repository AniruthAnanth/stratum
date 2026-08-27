/**
 * The Project pane — 06 §8.2's left group in Modern, available in Classic.
 *
 * Classic does not dock it (06 §8.3 puts History there instead) and a twenty-
 * year Stata user will never miss it — Stata has no project explorer. It exists
 * because §8's Modern layout names one, and because `PaneId` lists `project`, so
 * something has to answer when the dock builds that panel.
 *
 * # Opening a data file is a `use`
 *
 * The same rule the Variables and Properties panes are built on ([GSM] 2: "Items
 * from the contextual menu issue standard Stata commands, so working by
 * right-clicking is just like working directly in the Command window"). A double
 * click on `auto.dta` issues `use "auto.dta", clear`; on a `.do` it opens the
 * editor, because opening a file for editing is not an act the log records.
 * Anything that changes the *state of the session* goes through a command, and
 * anything that changes only what is on screen does not.
 *
 * # Where the entries come from
 *
 * A prop, and this is an escalation rather than a shortcut. CONTRACTS §11 has no
 * directory-listing command: `workspace_load` answers a `WorkspaceState`,
 * `stratum-asset://` serves frames and graphs, and nothing enumerates a folder.
 * `complete.ts` hit the same wall for filename completion and made the same
 * choice — declining rather than inventing. With no entries this pane says so in
 * one sentence instead of drawing an empty tree that looks like an empty project.
 */

import { For, type JSX, Show, createMemo, createSignal } from "solid-js";
import { render } from "solid-js/web";
import { submitCommand } from "../../commandbar/submit";
import { registerPane } from "../../dock/panes";
import { PaneHeader } from "../../ui";

import "./project.css";

/** What the pane can be handed. Deliberately flat: a path carries its own tree. */
export interface ProjectEntry {
  /** Relative to the project root, `/`-separated on every platform. */
  readonly path: string;
  readonly kind: "do" | "data" | "log" | "graph" | "other";
  /** Directories are not entries; a path with `/` in it implies its folders. */
  readonly sizeBytes?: number;
}

export interface ProjectCounters {
  /** Commands this pane issued (`use`, `cd`). */
  commands: number;
  /** Files handed to the editor. Never a command — see the header. */
  opens: number;
  /** Rows built. Grouping must not be O(files) per render. */
  rowsRendered: number;
}

const ZERO: ProjectCounters = { commands: 0, opens: 0, rowsRendered: 0 };
export const projectCounters: ProjectCounters = { ...ZERO };
export function resetProjectCounters(): void {
  Object.assign(projectCounters, ZERO);
}

/** The `use` a double click on a data file issues. Quoted: paths have spaces. */
export function useCommand(path: string): string {
  return `use "${path}", clear`;
}

/** `cd` to the project root, quoted for the same reason. */
export function cdCommand(path: string): string {
  return `cd "${path}"`;
}

/** Group order. Do-files first: that is what a project is, in this product. */
const GROUPS: readonly { kind: ProjectEntry["kind"]; title: string }[] = [
  { kind: "do", title: "Do-files" },
  { kind: "data", title: "Data" },
  { kind: "log", title: "Logs" },
  { kind: "graph", title: "Graphs" },
  { kind: "other", title: "Other" },
];

export interface ProjectPaneProps {
  /** The project root, shown in the header and used by `cd`. */
  root?: string;
  entries?: readonly ProjectEntry[];
  /** Opening a `.do` — the editor's job, not the log's. */
  onOpenFile?: (path: string) => void;
}

export function ProjectPane(props: ProjectPaneProps): JSX.Element {
  const [collapsed, setCollapsed] = createSignal<ReadonlySet<string>>(new Set<string>());

  /**
   * Entries by kind, computed once per entry-list change.
   *
   * A memo rather than a filter inside the render: collapsing one group would
   * otherwise re-scan every file in the project, which is the interaction-path
   * O(n) work §0a rules out even when n is small enough to get away with.
   */
  const grouped = createMemo(() => {
    const out = new Map<ProjectEntry["kind"], ProjectEntry[]>();
    for (const entry of props.entries ?? []) {
      const list = out.get(entry.kind);
      if (list === undefined) out.set(entry.kind, [entry]);
      else list.push(entry);
    }
    for (const list of out.values()) list.sort((a, b) => (a.path < b.path ? -1 : 1));
    return out;
  });

  const issue = (command: string): void => {
    projectCounters.commands += 1;
    void submitCommand(command, "menu");
  };

  const activate = (entry: ProjectEntry): void => {
    if (entry.kind === "data") {
      issue(useCommand(entry.path));
      return;
    }
    projectCounters.opens += 1;
    props.onOpenFile?.(entry.path);
  };

  return (
    <section class="proj" data-pane="project">
      <PaneHeader
        title="Project"
        actions={
          <Show when={props.root !== undefined}>
            <button
              type="button"
              class="proj__cd"
              data-project-cd
              title={`cd "${props.root}"`}
              onClick={() => issue(cdCommand(props.root as string))}
            >
              cd
            </button>
          </Show>
        }
      />

      <Show when={props.root !== undefined}>
        <p class="proj__root" data-project-root>
          {props.root}
        </p>
      </Show>

      <div class="proj__scroll">
        <For each={GROUPS}>
          {(group) => (
            <Show when={(grouped().get(group.kind) ?? []).length > 0}>
              <section class="proj__group">
                <button
                  type="button"
                  class="proj__group-head"
                  aria-expanded={!collapsed().has(group.kind)}
                  data-project-group={group.kind}
                  onClick={() => {
                    const next = new Set(collapsed());
                    if (next.has(group.kind)) next.delete(group.kind);
                    else next.add(group.kind);
                    setCollapsed(next);
                  }}
                >
                  <span class="proj__twisty" aria-hidden="true">
                    {collapsed().has(group.kind) ? "▸" : "▾"}
                  </span>
                  {group.title}
                  <span class="proj__count">{(grouped().get(group.kind) ?? []).length}</span>
                </button>
                <Show when={!collapsed().has(group.kind)}>
                  <ul class="proj__list">
                    <For each={grouped().get(group.kind) ?? []}>
                      {(entry) => {
                        projectCounters.rowsRendered += 1;
                        return (
                          <li>
                            <button
                              type="button"
                              class="proj__entry"
                              data-project-entry={entry.path}
                              onDblClick={() => activate(entry)}
                              onKeyDown={(event) => {
                                if (event.key !== "Enter") return;
                                event.preventDefault();
                                activate(entry);
                              }}
                            >
                              {entry.path}
                            </button>
                          </li>
                        );
                      }}
                    </For>
                  </ul>
                </Show>
              </section>
            </Show>
          )}
        </For>

        <Show when={(props.entries ?? []).length === 0}>
          <p class="proj__empty">
            No project files are listed. Stratum has no directory-listing command yet (CONTRACTS
            §11), so this pane shows what the host hands it.
          </p>
        </Show>
      </div>
    </section>
  );
}

/** Registers the pane with W12's dock. Returns the disposer. */
export function registerProjectPane(props: ProjectPaneProps = {}): () => void {
  return registerPane(
    "project",
    (host, register) => {
      register(render(() => <ProjectPane {...props} />, host));
    },
    "Project",
  );
}
