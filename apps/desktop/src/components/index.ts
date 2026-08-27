/**
 * The execution-state components — W15's import surface.
 *
 * Same idiom as `ui/index.ts`: one entry point, so a pane never reaches into a
 * component file and never gets a component without its stylesheet. These are
 * the *connected* half of the execution-state UI — they read `state/exec.ts` —
 * where `ui/**` (W12) is the presentational half that works from plain props.
 */

export {
  CleanChip,
  CleanRunButton,
  CleanScope,
  type CleanChipProps,
  type CleanRunButtonProps,
} from "./CleanChip";
export {
  PlanNotice,
  StaleBanner,
  brokenFix,
  depKeyLabel,
  describeStatus,
  staleBecause,
  type PlanNoticeProps,
  type StaleBannerProps,
  type StateAction,
  type StatusDescription,
} from "./StaleBanner";
export {
  RUN_VERBS,
  RunQueue,
  RunVerbs,
  type RunQueueProps,
  type RunVerbsProps,
} from "./RunQueue";
export {
  RunModeLabel,
  StaleCountButton,
  StateReadout,
  datasetText,
  execText,
  type StaleCountButtonProps,
  type StateReadoutProps,
} from "./StateReadout";
