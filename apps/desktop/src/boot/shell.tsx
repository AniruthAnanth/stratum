/**
 * The main-window shell — 06 §8.2.
 *
 *     TopBar 38  ·  Dock (the layout's pane tree)  ·  StatusBar 22
 *
 * The shell owns three DOM rows and nothing else. Every pane inside the dock
 * belongs to another unit and arrives through `registerPane`; the shell never
 * imports one, which is what lets W13 through W21 land in any order.
 */

import { type JSX, Show, createEffect, createSignal, onCleanup, onMount } from "solid-js";
import { type DockAdapter, createDock } from "../dock/adapter";
import { installDetach } from "../dock/detach";
import { attachDock, detachDock, layoutNotice, layoutSpec } from "../state/layout";
import { type StateReadout, StatusBar, TopBar } from "../ui";
import { latestError } from "./errors";
import type { WindowIdentity } from "./role";

export interface ShellProps {
  identity: WindowIdentity;
  readout: StateReadout;
  platform: "macos" | "windows" | "linux";
}

export function Shell(props: ShellProps): JSX.Element {
  let dockContainer: HTMLDivElement | undefined;
  let dock: DockAdapter | undefined;
  const [revealed, setRevealed] = createSignal(false);

  onMount(() => {
    if (dockContainer === undefined) return;
    dock = createDock(dockContainer);
    attachDock(dock);

    // dockview measures itself from its container, and the container is a flex
    // child with no intrinsic size until the first layout pass. One explicit
    // `layout()` per resize is cheaper and more predictable than letting it
    // observe, and it is the only place the shell talks about pixels.
    const resize = (): void => {
      const rect = dockContainer?.getBoundingClientRect();
      if (rect !== undefined) dock?.layout(rect.width, rect.height);
    };
    resize();
    window.addEventListener("resize", resize);

    const disposeDetach = installDetach(dock, { project: props.identity.project });

    onCleanup(() => {
      window.removeEventListener("resize", resize);
      disposeDetach();
      detachDock();
      dock?.dispose();
      dock = undefined;
    });
  });

  // 06 §8.4: in Focus the top bar auto-hides and reveals on a pointer within
  // 6px of the top edge. Pointer proximity, not hover on the bar itself — a bar
  // that is off screen cannot be hovered.
  createEffect(() => {
    if (layoutSpec().chrome.topBar !== "auto-hide") {
      setRevealed(false);
      return;
    }
    const onMove = (event: PointerEvent): void => {
      setRevealed(event.clientY <= 6);
    };
    window.addEventListener("pointermove", onMove, { passive: true });
    onCleanup(() => window.removeEventListener("pointermove", onMove));
  });

  return (
    <>
      <TopBar
        mode={layoutSpec().chrome.topBar}
        revealed={revealed()}
        trafficLightInset={props.platform === "macos"}
        readout={props.readout}
      />

      <div class="shell__dock" ref={dockContainer} />

      <Show when={layoutSpec().chrome.statusBar}>
        <StatusBar cwd={props.identity.project} notice={layoutNotice() ?? latestError()?.message} />
      </Show>
    </>
  );
}
