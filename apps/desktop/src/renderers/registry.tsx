/**
 * Payload → renderer, and the one composition every card goes through.
 *
 * [`ResultCard`] is the only public way to draw a result. That is deliberate:
 * the acceptance bullets "every card has `Raw ▸` in the same position, always"
 * and "card anatomy is identical across renderers" are properties of a shell, and
 * a shell only holds if there is no second way in. A renderer in this directory
 * exports a BODY — `SummarizeCard`, `EstimationCard`, … — and never a card.
 *
 * One block → one card (06 §4.7). A block emitting a table *and* a graph
 * produces one card with two stacked sections separated by a hairline, which is
 * why the body maps over `payloads` rather than picking the first one.
 */

import { For, type JSX, Show, createMemo, createSignal } from "solid-js";
import { Card } from "./card";
import { ErrorCard } from "./error";
import { EstimationCard } from "./estimation";
import { GraphCard } from "./graph";
import { LogCard } from "./log";
import { RawView } from "./raw";
import { SummarizeCard } from "./summarize";
import { DataChangedCard, ScalarsCard, TableCard } from "./table";
import { TabulateCard } from "./tabulate";
import type {
  CardActionView,
  CardUiState,
  PayloadKind,
  ResultEnvelopeView,
  ResultPayloadView,
  ScalarsPayloadView,
} from "./types";

/** Host hooks. A renderer reports; it never runs anything itself (spec §13). */
export interface ResultCardHandlers {
  onAction?: (action: CardActionView) => void;
  onMenu?: () => void;
  onSelectVar?: (name: string) => void;
  onOpenViewer?: () => void;
  onApplySuggestion?: (index: number) => void;
  onHelp?: (rc: number) => void;
}

export interface ResultCardProps extends ResultCardHandlers {
  envelope: ResultEnvelopeView;
  ui?: CardUiState;
}

export function ResultCard(props: ResultCardProps): JSX.Element {
  const [rawOpen, setRawOpen] = createSignal(false);

  /**
   * `EstimationPayload` has no display strings for its `e()` scalars, so the
   * model strip reads a sibling `Scalars` payload when the engine sends one.
   * See the escalation at the head of `estimation/index.tsx`.
   */
  const siblingScalars = createMemo((): ScalarsPayloadView | undefined =>
    props.envelope.payloads.find((p): p is ScalarsPayloadView => p.kind === "scalars"),
  );

  /** An `Estimation` payload consumes the sibling `Scalars`; do not draw it twice. */
  const bodies = createMemo((): readonly ResultPayloadView[] => {
    const hasEstimation = props.envelope.payloads.some((p) => p.kind === "estimation");
    return hasEstimation
      ? props.envelope.payloads.filter((p) => p.kind !== "scalars")
      : props.envelope.payloads;
  });

  const handle = (action: CardActionView): void => {
    if (action.action === "raw_output") setRawOpen((v) => !v);
    props.onAction?.(action);
  };

  return (
    <Card
      envelope={props.envelope}
      ui={props.ui}
      onAction={handle}
      onMenu={props.onMenu}
      expanded={rawOpen() ? "raw_output" : undefined}
    >
      <For each={bodies()}>
        {(payload) => (
          <section class="card__section" data-payload={payload.kind}>
            <PayloadBody
              payload={payload}
              envelope={props.envelope}
              scalars={siblingScalars()}
              handlers={props}
            />
          </section>
        )}
      </For>
      <Show when={rawOpen()}>
        <RawView raw={props.envelope.raw} />
      </Show>
    </Card>
  );
}

function PayloadBody(props: {
  payload: ResultPayloadView;
  envelope: ResultEnvelopeView;
  scalars: ScalarsPayloadView | undefined;
  handlers: ResultCardHandlers;
}): JSX.Element {
  // Exhaustive over `PayloadKind`. `assertNever` below makes a variant added to
  // `ResultPayload` in Rust a TypeScript error here rather than a blank card.
  const p = props.payload;
  switch (p.kind) {
    case "log":
      return <LogCard payload={p} />;
    case "summarize":
      return <SummarizeCard payload={p} onSelectVar={props.handlers.onSelectVar} />;
    case "tabulate":
      return <TabulateCard payload={p} onOpenViewer={props.handlers.onOpenViewer} />;
    case "estimation":
      return <EstimationCard payload={p} scalars={props.scalars} />;
    case "graph":
      return <GraphCard payload={p} />;
    case "table":
      return <TableCard payload={p} />;
    case "scalars":
      return <ScalarsCard payload={p} />;
    case "data_changed":
      return <DataChangedCard payload={p} />;
    case "error":
      return (
        <ErrorCard
          payload={p}
          onApply={props.handlers.onApplySuggestion}
          onHelp={props.handlers.onHelp}
        />
      );
    case "unknown":
      // §5.2: "Renders through the raw renderer. No apology, no empty state."
      return <RawView raw={props.envelope.raw} inline />;
    default:
      return assertNever(p);
  }
}

function assertNever(value: never): never {
  throw new TypeError(`unhandled ResultPayload: ${JSON.stringify(value)}`);
}

/** The kinds this dispatch handles, for the enumeration test. */
export const HANDLED_KINDS: readonly PayloadKind[] = [
  "log",
  "summarize",
  "tabulate",
  "estimation",
  "graph",
  "table",
  "scalars",
  "data_changed",
  "error",
  "unknown",
];
