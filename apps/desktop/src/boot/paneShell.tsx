/**
 * The detached-pane shell — 06 §13.3 step 2: "`pane.html` boots the same bundle
 * with a tiny shell (no dock, no top bar), so the code is shared and the chunk
 * graph is warm."
 *
 * Tiny is the specification, not an economy. A detached Results pane that grew a
 * top bar and a dock would be a second main window, and then "which window owns
 * the document" (§13.2) stops having an answer.
 */

import { type JSX, onCleanup, onMount } from "solid-js";
import { type PaneComponentId, paneHost, paneTitle } from "../dock/panes";
import { PaneHeader } from "../ui";
import { PaneBoundary } from "./errors";

export interface PaneShellProps {
  paneId: PaneComponentId;
}

export function PaneShell(props: PaneShellProps): JSX.Element {
  let mount: HTMLDivElement | undefined;

  onMount(() => {
    // The same `paneHost` the dock uses. A pane that has been detached and
    // re-docked twice is still the same element with the same scroll position
    // and the same CodeMirror state.
    const host = paneHost(props.paneId);
    mount?.appendChild(host);
    onCleanup(() => {
      // Detach, do not destroy: this window is closing, but the pane may be on
      // its way back into the main window's dock.
      host.remove();
    });
  });

  return (
    <PaneBoundary name={paneTitle(props.paneId)}>
      <PaneHeader title={paneTitle(props.paneId)} />
      <div class="shell__pane" ref={mount} />
    </PaneBoundary>
  );
}
