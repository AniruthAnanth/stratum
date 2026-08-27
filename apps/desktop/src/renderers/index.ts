/**
 * The renderer surface. One import point, so a pane never reaches past
 * [`ResultCard`] into a body renderer and accidentally draws a card without its
 * rail, its readout or its `Raw ▸`.
 */

export {
  ActionRow,
  actionLabel,
  orderedActions,
  rawOutputRepairs,
  resetRawOutputRepairs,
} from "./actions";
export { Card, announcement, cardState, middleEllipsis, type CardProps } from "./card";
export { assetUrl } from "./asset";
export { decimalPad, decimalPlaces, durationLabel, readout, type Readout } from "./readout";
export {
  HANDLED_KINDS,
  ResultCard,
  type ResultCardHandlers,
  type ResultCardProps,
} from "./registry";
export { RawView, type RawViewProps } from "./raw";
export { LogCard, styleClass, type LogCardProps } from "./log";
export { SummarizeCard, type SummarizeCardProps } from "./summarize";
export { EstimationCard, type EstimationCardProps } from "./estimation";
export {
  MAX_CELLS,
  TabulateCard,
  levelLabel,
  shownCells,
  type TabulateCardProps,
} from "./tabulate";
export { GraphCard, MAX_GRAPH_PT, type GraphCardProps } from "./graph";
export { ErrorCard, type ErrorCardProps } from "./error";
export {
  DataChangedCard,
  ScalarsCard,
  TableCard,
  deltaChips,
  type DataChangedCardProps,
  type ScalarsCardProps,
  type TableCardProps,
} from "./table";
export * from "./types";
