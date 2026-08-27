/**
 * The Sections pane — `PaneId "sections"`, assigned to W13 by ARCHITECT
 * amendment A34 (it was listed in CONTRACTS §12 and owned by no unit).
 *
 * An outline of the `// %%` markers with the block count and status of each, and
 * the §3 operations. It is a VIEW of the segmenter's section index, not a second
 * copy of it: click-to-go-to reads offsets from the same `sections()` the editor
 * decorates from, so the pane and the gutter can never disagree about where a
 * section begins.
 *
 * Nothing here writes the document. `Rename` and `Move` are the gated W26
 * writers reached through `commands.ts`; the pane offers them and reports them
 * unavailable when W26 has not installed a writer, which is what the disabled
 * rows mean.
 */

import type { EditorView } from "@codemirror/view";
import { For, Show, createSignal, onCleanup } from "solid-js";
import type { JSX } from "solid-js";
import { render } from "solid-js/web";
import { registerPane } from "../../dock/panes";
import { blocksTouching } from "../../editor/blocks/blockField";
import { activeEditor } from "../../editor/commands";
import { anchorForBlock } from "../../editor/results/anchor";
import { displayStatus } from "../../editor/results/widget";
import { sectionTitle, sectionWriter, sections } from "../../editor/sections/markers";
import type { BlockStatusState } from "../../ipc/hand";
import { runCommand } from "../../keys/registry";
import { Button, Icon, StateGlyph } from "../../ui";

interface SectionRow {
  readonly id: number;
  readonly title: string;
  readonly from: number;
  readonly blocks: number;
  /** The worst status among the section's blocks — the outline's whole job. */
  readonly status: BlockStatusState;
}

/**
 * Read the outline out of the live editor.
 *
 * Recomputed on demand rather than subscribed to: a document with 40 sections
 * costs 40 rows, and a pane that re-rendered on every keystroke would put a
 * Solid render on the typing path for a list nobody is looking at while typing.
 */
function readSections(view: EditorView | null): SectionRow[] {
  if (view === null) return [];
  const state = view.state;
  return sections(state).map((section) => {
    const blocks = blocksTouching(state, section.from, section.to).filter((b) => b.executable);
    let status: BlockStatusState = "current";
    let any = false;
    for (const block of blocks) {
      const rec = anchorForBlock(state, block);
      if (rec === null) continue;
      any = true;
      const here = displayStatus(rec, block);
      if (RANK[here] < RANK[status]) status = here;
    }
    return {
      id: section.id,
      title: sectionTitle(state, section) || "(untitled)",
      from: section.from,
      blocks: blocks.length,
      status: any ? status : "never_run",
    };
  });
}

/** Worst-wins, same order as `STATUS_RANK`; lower is worse. */
const RANK: Readonly<Record<BlockStatusState, number>> = {
  never_run: 0,
  broken: 1,
  failed: 2,
  interrupted: 3,
  stale: 4,
  current_unverifiable: 5,
  current: 6,
  queued: 90,
  running: 91,
};

export function SectionsPane(): JSX.Element {
  const [rows, setRows] = createSignal<SectionRow[]>(readSections(activeEditor()));

  // Poll on focus/blur and on an explicit refresh rather than on every
  // transaction. The outline changes when markers are added or a block runs,
  // both of which are rare; a `updateListener` here would be a render per
  // keystroke for a pane that is often not even visible.
  const refresh = (): void => {
    setRows(readSections(activeEditor()));
  };
  const timer = setInterval(refresh, 500);
  onCleanup(() => clearInterval(timer));

  return (
    <nav class="pane-sections" aria-label="Sections">
      <Show
        when={rows().length > 0}
        fallback={
          <p class="pane-empty">
            No sections yet. A comment like <code>{"// %% Data loading"}</code> starts one, and it
            stays a valid Stata comment.
          </p>
        }
      >
        <ul class="pane-sectionList">
          <For each={rows()}>
            {(row) => (
              <li class="pane-sectionRow" data-status={row.status}>
                <button
                  type="button"
                  class="pane-sectionGo"
                  onClick={() => runCommand("section.goto", { id: row.id })}
                >
                  <StateGlyph state={row.status} />
                  <span class="pane-sectionTitle">{row.title}</span>
                  <span class="pane-sectionCount">{row.blocks}</span>
                </button>
                <span class="pane-sectionActions">
                  <Button
                    variant="quiet"
                    onClick={() => {
                      runCommand("section.goto", { id: row.id });
                      runCommand("section.run");
                    }}
                    aria-label={`Run section ${row.title}`}
                  >
                    <Icon name="run" title="Run section" />
                  </Button>
                  <Button
                    variant="quiet"
                    disabled={sectionWriter() === null}
                    onClick={() => {
                      runCommand("section.goto", { id: row.id });
                      runCommand("section.moveUp");
                    }}
                    aria-label={`Move section ${row.title} up`}
                  >
                    <Icon name="chevron-down" title="Move up" class="icon-flip" />
                  </Button>
                </span>
              </li>
            )}
          </For>
        </ul>
      </Show>
    </nav>
  );
}

/** Registers the pane with W12's dock. Returns the disposer. */
export function registerSectionsPane(): () => void {
  return registerPane("sections", (host, register) => {
    register(render(() => <SectionsPane />, host));
  });
}
