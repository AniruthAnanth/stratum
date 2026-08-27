/**
 * The Properties window — spec §7; 06 §9.5, and [GSM] 2 on this machine:
 *
 * > The Properties window allows you to view and edit variable and dataset
 * > properties. … If a single variable is selected in the Variables window, its
 * > properties are displayed. If there are multiple variables selected in the
 * > Variables window, the Properties window will display properties that are
 * > common across all selected variables.
 * >
 * > By default, the Properties window is locked, which prevents editing the
 * > properties. Clicking on the lock icon will unlock it and allow editing. …
 * > Clicking the arrow buttons next to the lock icon will select the previous or
 * > next variable shown in the Variables window.
 *
 * Every edit here goes through {@link commandFor} and then through the ordinary
 * `submitCommand`, so it appears in Results, in History with its `_rc`, and in
 * the log. 06 §9.5: "This is Stata's behaviour and it is a reproducibility
 * feature, not a legacy quirk — we keep it exactly." There is deliberately no
 * path from this pane to the engine that is not a command a user could have
 * typed.
 *
 * # What the Data section can honestly show
 *
 * 06 §9.5 lists Frame, Filename, Label, Notes, Variables, Observations, Size,
 * Memory and Sorted by. `stratum_proto::data::FrameInfo` carries `name`,
 * `n_obs`, `n_vars`, `sorted_by`, `changed` and `state` — so Frame, Variables,
 * Observations and Sorted by are answerable from the wire and the other five are
 * not. They are therefore **props**: a host that knows them (from `describe`)
 * fills them in and they are drawn; nothing fabricates a `—` for a field the
 * product cannot answer, for the same reason `columns.ts` ships no Notes column.
 * Escalated in W16's return.
 */

import { For, type JSX, Show, createMemo, createSignal } from "solid-js";
import { render } from "solid-js/web";
import { submitCommand } from "../../commandbar/submit";
import { registerPane } from "../../dock/panes";
import { type VariableRow, variables } from "../../state/vars";
import { Icon, PaneHeader } from "../../ui";
import { stepSelection, variableSelection } from "../variables/selection";
import { type EditRequest, type PropertyField, commandFor } from "./edits";

import "./properties.css";

export interface PropertiesCounters {
  /** Commands this pane issued. Every accepted edit is exactly one. */
  commands: number;
  /** Edits refused before becoming a command (locked, or not a name/format). */
  refused: number;
  /** `◀ ▶` presses that moved the primary selection. */
  steps: number;
}

const ZERO: PropertiesCounters = { commands: 0, refused: 0, steps: 0 };
export const propertiesCounters: PropertiesCounters = { ...ZERO };
export function resetPropertiesCounters(): void {
  Object.assign(propertiesCounters, ZERO);
}

/** The Data fields no wire type answers yet. Absent fields are not drawn. */
export interface DataFacts {
  filename?: string;
  label?: string;
  notes?: readonly string[];
  /** `describe`'s "Size" line, pre-formatted by whoever asked the engine. */
  size?: string;
  memory?: string;
  observations?: number;
  sortedBy?: readonly string[];
}

export interface PropertiesPaneProps {
  rows?: readonly VariableRow[];
  frame?: string;
  data?: DataFacts;
  /** The variable notes for the primary selection, when the host knows them. */
  notes?: (name: string) => readonly string[] | undefined;
  /** Starts locked, as Stata does. A detached inspector may start unlocked. */
  locked?: boolean;
}

/** One editable row of the Variables section. */
interface FieldSpec {
  readonly field: PropertyField;
  readonly label: string;
  readonly value: (row: VariableRow) => string;
  /** Type is derived from the storage type and is not editable. */
  readonly readOnly?: boolean;
}

const VARIABLE_FIELDS: readonly FieldSpec[] = [
  { field: "name", label: "Name", value: (row) => row.name },
  { field: "label", label: "Label", value: (row) => row.label ?? "" },
  { field: "format", label: "Format", value: (row) => row.format },
  { field: "valueLabel", label: "Value label", value: (row) => row.valueLabel ?? "" },
];

/**
 * The padlock, on `ui/icons.tsx`'s 14-unit grid and with its stroke conventions.
 *
 * Drawn here rather than added to `IconName` because that file is W12's (R0).
 * Not an emoji: a 🔒 is a different glyph on each of the three platforms, it
 * inherits none of the icon tokens, and §39 rules out a UI that reads as
 * assembled from whatever the font happened to have.
 */
function LockGlyph(props: { locked: boolean }): JSX.Element {
  return (
    <svg
      class="props__lock-glyph"
      viewBox="0 0 14 14"
      width="14"
      height="14"
      fill="none"
      stroke="currentColor"
      stroke-width="var(--icon-stroke)"
      stroke-linecap="square"
      stroke-linejoin="miter"
      aria-hidden="true"
    >
      <path d="M2.5 6.5 H11.5 V12.5 H2.5 Z" />
      {/* Unlocked lifts the shackle and shifts it right: the two states have to
          be distinguishable at 14 px without reading the colour. */}
      <path
        d={
          props.locked
            ? "M4.5 6.5 V4 A2.5 2.5 0 0 1 9.5 4 V6.5"
            : "M6.5 6.5 V4 A2.5 2.5 0 0 1 11.5 4"
        }
      />
    </svg>
  );
}

/** Multiple selection: a field is shown only where every row agrees ([GSM] 2). */
function commonValue(rows: readonly VariableRow[], spec: FieldSpec): string | undefined {
  const first = rows[0];
  if (first === undefined) return undefined;
  const value = spec.value(first);
  return rows.every((row) => spec.value(row) === value) ? value : undefined;
}

export function PropertiesPane(props: PropertiesPaneProps): JSX.Element {
  const [locked, setLocked] = createSignal(props.locked ?? true);
  const [draft, setDraft] = createSignal<Record<string, string>>({});
  const [refusal, setRefusal] = createSignal<string | undefined>(undefined);
  const [note, setNote] = createSignal("");

  const all = (): readonly VariableRow[] => props.rows ?? variables.rows;
  const selectedNames = (): readonly string[] => variableSelection().names;

  const selectedRows = createMemo(() => {
    const names = new Set(selectedNames());
    return all().filter((row) => names.has(row.name));
  });

  const primary = createMemo((): VariableRow | undefined => {
    const name = variableSelection().primary;
    return name === undefined ? undefined : all().find((row) => row.name === name);
  });

  /** The names in the order the Variables pane shows them; `◀ ▶` walk this. */
  const walkOrder = (): string[] => all().map((row) => row.name);

  const issue = (request: EditRequest): void => {
    if (locked()) {
      propertiesCounters.refused += 1;
      setRefusal("The Properties window is locked.");
      return;
    }
    const outcome = commandFor(request);
    if (!outcome.ok) {
      propertiesCounters.refused += 1;
      setRefusal(outcome.reason);
      return;
    }
    setRefusal(undefined);
    propertiesCounters.commands += 1;
    void submitCommand(outcome.command, "menu");
  };

  const commit = (spec: FieldSpec, value: string): void => {
    const row = primary();
    if (row === undefined) return;
    if (value === spec.value(row)) return;
    issue({ field: spec.field, variable: row.name, value });
  };

  const step = (delta: -1 | 1): void => {
    const moved = stepSelection(walkOrder(), delta);
    if (moved !== undefined) {
      propertiesCounters.steps += 1;
      setDraft({});
    }
  };

  const draftOf = (spec: FieldSpec): string => {
    const key = spec.field;
    const held = draft()[key];
    if (held !== undefined) return held;
    const rows = selectedRows();
    return (rows.length === 1 ? spec.value(rows[0] as VariableRow) : commonValue(rows, spec)) ?? "";
  };

  const mixed = (spec: FieldSpec): boolean =>
    selectedRows().length > 1 && commonValue(selectedRows(), spec) === undefined;

  const variableNotes = (): readonly string[] => {
    const name = variableSelection().primary;
    return (name === undefined ? undefined : props.notes?.(name)) ?? [];
  };

  return (
    <section class="props" data-pane="properties">
      <PaneHeader
        title="Properties"
        actions={
          <div class="props__actions">
            <button
              type="button"
              class="props__step"
              aria-label="Previous variable"
              data-properties-previous
              disabled={all().length === 0}
              onClick={() => step(-1)}
            >
              ◀
            </button>
            <button
              type="button"
              class="props__step"
              aria-label="Next variable"
              data-properties-next
              disabled={all().length === 0}
              onClick={() => step(1)}
            >
              ▶
            </button>
            {/* The padlock. Locked is the default and the whole point: the
                Properties window is a place you read, until you say otherwise. */}
            <button
              type="button"
              class="props__lock"
              aria-pressed={locked()}
              aria-label={locked() ? "Unlock properties for editing" : "Lock properties"}
              data-properties-lock
              data-locked={locked() ? "" : undefined}
              onClick={() => {
                setLocked(!locked());
                setRefusal(undefined);
              }}
            >
              <LockGlyph locked={locked()} />
            </button>
          </div>
        }
      />

      <div class="props__scroll">
        <h3 class="props__section">Variables</h3>
        <Show
          when={selectedRows().length > 0}
          fallback={<p class="props__empty">No variable is selected.</p>}
        >
          <dl class="props__list">
            <For each={VARIABLE_FIELDS}>
              {(spec) => (
                <>
                  <dt class="props__key">{spec.label}</dt>
                  <dd class="props__val">
                    <input
                      class="props__input"
                      type="text"
                      value={draftOf(spec)}
                      placeholder={mixed(spec) ? "(varies)" : ""}
                      disabled={locked() || selectedRows().length !== 1}
                      aria-label={spec.label}
                      data-properties-field={spec.field}
                      onInput={(event) =>
                        setDraft({ ...draft(), [spec.field]: event.currentTarget.value })
                      }
                      onChange={(event) => commit(spec, event.currentTarget.value)}
                      onKeyDown={(event) => {
                        if (event.key === "Enter") {
                          event.preventDefault();
                          commit(spec, event.currentTarget.value);
                        } else if (event.key === "Escape") {
                          setDraft({});
                        }
                      }}
                    />
                  </dd>
                </>
              )}
            </For>

            {/* Type is a property of the data, not of the metadata: it changes
                with `recast`, which is a data command and not a Properties edit. */}
            <dt class="props__key">Type</dt>
            <dd class="props__val props__val--static" data-properties-type>
              {selectedRows().length === 1 ? (primary()?.storage ?? "") : "(varies)"}
            </dd>

            <dt class="props__key">Notes</dt>
            <dd class="props__val">
              <ol class="props__notes">
                <For each={variableNotes()}>{(text) => <li>{text}</li>}</For>
              </ol>
              <input
                class="props__input"
                type="text"
                value={note()}
                placeholder="Add a note"
                disabled={locked() || variableSelection().primary === undefined}
                aria-label="Add a note"
                data-properties-note
                onInput={(event) => setNote(event.currentTarget.value)}
                onKeyDown={(event) => {
                  if (event.key !== "Enter") return;
                  event.preventDefault();
                  issue({
                    field: "note",
                    variable: variableSelection().primary,
                    value: note(),
                  });
                  setNote("");
                }}
              />
            </dd>
          </dl>
        </Show>

        <h3 class="props__section">Data</h3>
        <dl class="props__list">
          <dt class="props__key">Frame</dt>
          <dd class="props__val props__val--static" data-properties-frame>
            {props.frame ?? variables.frame}
          </dd>

          <Show when={props.data?.filename !== undefined}>
            <dt class="props__key">Filename</dt>
            <dd class="props__val props__val--static">{props.data?.filename}</dd>
          </Show>

          <Show when={props.data?.label !== undefined}>
            <dt class="props__key">Label</dt>
            <dd class="props__val">
              <input
                class="props__input"
                type="text"
                value={props.data?.label ?? ""}
                disabled={locked()}
                aria-label="Dataset label"
                data-properties-data-label
                onChange={(event) =>
                  issue({ field: "dataLabel", value: event.currentTarget.value })
                }
              />
            </dd>
          </Show>

          <dt class="props__key">Variables</dt>
          <dd class="props__val props__val--static" data-properties-nvars>
            {all().length}
          </dd>

          <Show when={props.data?.observations !== undefined}>
            <dt class="props__key">Observations</dt>
            <dd class="props__val props__val--static">
              {props.data?.observations?.toLocaleString("en-US")}
            </dd>
          </Show>

          <Show when={props.data?.size !== undefined}>
            <dt class="props__key">Size</dt>
            <dd class="props__val props__val--static">{props.data?.size}</dd>
          </Show>

          <Show when={props.data?.memory !== undefined}>
            <dt class="props__key">Memory</dt>
            <dd class="props__val props__val--static">{props.data?.memory}</dd>
          </Show>

          <Show when={props.data?.sortedBy !== undefined}>
            <dt class="props__key">Sorted by</dt>
            <dd class="props__val props__val--static" data-properties-sorted>
              {(props.data?.sortedBy ?? []).join(" ")}
            </dd>
          </Show>
        </dl>
      </div>

      <Show when={refusal() !== undefined}>
        {/* `<output>` rather than `<p role="status">`: the implicit role is the
            same one and the element is what a refusal actually is — the result
            of the edit the user just attempted. */}
        <output class="props__refusal" data-properties-refusal>
          <Icon name="warn" />
          {refusal()}
        </output>
      </Show>
    </section>
  );
}

/** Registers the pane with W12's dock. Returns the disposer. */
export function registerPropertiesPane(props: PropertiesPaneProps = {}): () => void {
  return registerPane(
    "properties",
    (host, register) => {
      register(render(() => <PropertiesPane {...props} />, host));
    },
    "Properties",
  );
}
