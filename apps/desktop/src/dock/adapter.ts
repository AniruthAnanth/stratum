/**
 * The dockview adapter — 06 §1 ("we write the dockview adapter, ~150 LOC").
 *
 * dockview-core is framework-agnostic vanilla DOM, so the adapter is small on
 * purpose: it maps a component name to a persistent pane host (`panes.ts`),
 * draws our own tab instead of dockview's, and forwards `fromJSON`/`toJSON`.
 * Everything else — which panes exist, where they go, what a preset is — lives
 * in `LayoutSpec`, because dockview's blob is opaque by contract and we do not
 * want a second layout model that can disagree with it.
 */

import {
  type CreateComponentOptions,
  DockviewComponent,
  type DockviewTheme,
  type IContentRenderer,
  type ITabRenderer,
  type SerializedDockview,
} from "dockview-core";
import type { LayoutSpec } from "../ipc/hand";
import {
  type PaneComponentId,
  isPaneComponentId,
  paneHost,
  paneTitle,
  popScroll,
  stashScroll,
} from "./panes";

/**
 * dockview ships a generic-IDE look and we override essentially all of it
 * (06 §1). `gap: 0` and `dndOverlayMounting: "absolute"` are the two settings
 * that are structural rather than cosmetic: a gap would put the app background
 * between panes, and a relative overlay clips against the group it is mounted
 * in, which reads as a box drawn inside a box.
 */
const STRATUM_THEME: DockviewTheme = {
  name: "stratum",
  className: "dv-theme-stratum",
  gap: 0,
  dndOverlayMounting: "absolute",
  dndPanelOverlay: "group",
};

class PaneRenderer implements IContentRenderer {
  readonly element: HTMLElement;

  constructor(private readonly component: PaneComponentId) {
    this.element = paneHost(component);
  }

  init(): void {
    // The node has just been re-parented. Every engine we ship on zeroes
    // `scrollTop` when an element leaves the document, so the offset captured
    // on the way out is put back here — this is the whole of what a preset
    // switch has to restore by hand.
    popScroll(this.component);
  }

  dispose(): void {
    stashScroll(this.component);
    // Deliberately NOT removing the element or unmounting the pane: the node is
    // owned by `panes.ts` and outlives every panel that displays it.
  }
}

class UnknownPaneRenderer implements IContentRenderer {
  readonly element: HTMLElement;
  constructor(name: string) {
    this.element = document.createElement("div");
    this.element.className = "pane-host pane-host--unknown";
    this.element.textContent = `Unknown pane: ${name}`;
  }
  init(): void {}
}

/** Our tab. 28 px, hairline, no rounded shoulder, no close glyph until hover. */
class PaneTab implements ITabRenderer {
  readonly element: HTMLElement;
  private readonly label: HTMLElement;

  constructor() {
    this.element = document.createElement("div");
    this.element.className = "pane-tab";
    this.label = document.createElement("span");
    this.label.className = "pane-tab__label";
    this.element.appendChild(this.label);
  }

  init(params: { title: string }): void {
    this.label.textContent = params.title;
  }

  update(event: { params: Record<string, unknown> }): void {
    const title = event.params["title"];
    if (typeof title === "string") this.label.textContent = title;
  }
}

export interface DockAdapter {
  readonly element: HTMLElement;
  /** Replaces the whole dock from a spec's main window. */
  apply(spec: LayoutSpec): void;
  toJSON(): SerializedDockview;
  layout(width: number, height: number): void;
  panes(): PaneComponentId[];
  isOpen(pane: PaneComponentId): boolean;
  toggle(pane: PaneComponentId): void;
  focus(pane: PaneComponentId): void;
  onChange(handler: () => void): () => void;
  /** Fires when the user starts dragging a tab. `detach.ts` is the only consumer. */
  onPanelDragStart(handler: (panelId: string, event: DragEvent) => void): () => void;
  /** Removes a panel without touching its host element, for a detach. */
  close(pane: PaneComponentId): void;
  dispose(): void;
}

export function createDock(container: HTMLElement): DockAdapter {
  const dock = new DockviewComponent(container, {
    theme: STRATUM_THEME,
    createComponent: ({ name }: CreateComponentOptions): IContentRenderer =>
      isPaneComponentId(name) ? new PaneRenderer(name) : new UnknownPaneRenderer(name),
    createTabComponent: (): ITabRenderer => new PaneTab(),
    disableFloatingGroups: false,
    singleTabMode: "default",
  });

  const handlers = new Set<() => void>();
  const notify = (): void => {
    for (const h of handlers) h();
  };
  const subscription = dock.onDidLayoutChange(notify);

  const dragHandlers = new Set<(panelId: string, event: DragEvent) => void>();
  const dragSubscription = dock.onWillDragPanel((event) => {
    const native = event.nativeEvent;
    for (const h of dragHandlers) h(event.panel.id, native);
  });

  return {
    element: dock.element,

    apply(spec: LayoutSpec): void {
      const main = spec.windows[0];
      if (main === undefined) throw new Error("LayoutSpec has no main window");
      // `fromJSON` disposes every panel and builds new ones. The pane hosts do
      // not care: they are re-parented, not re-created. That is the property
      // the ≤120 ms preset-switch budget is bought with.
      dock.fromJSON(main.dock as SerializedDockview);
    },

    toJSON: () => dock.toJSON(),
    layout: (width, height) => dock.layout(width, height),

    panes: () =>
      dock.panels.map((p) => p.id).filter((id): id is PaneComponentId => isPaneComponentId(id)),

    isOpen: (pane) => dock.panels.some((p) => p.id === pane),

    toggle(pane: PaneComponentId): void {
      const existing = dock.getGroupPanel(pane);
      if (existing !== undefined) {
        existing.api.close();
        return;
      }
      dock.api.addPanel({ id: pane, component: pane, title: paneTitle(pane) });
    },

    focus(pane: PaneComponentId): void {
      const panel = dock.getGroupPanel(pane);
      if (panel === undefined) {
        dock.api.addPanel({ id: pane, component: pane, title: paneTitle(pane) });
        return;
      }
      panel.api.setActive();
    },

    onChange(handler: () => void): () => void {
      handlers.add(handler);
      return () => {
        handlers.delete(handler);
      };
    },

    onPanelDragStart(handler): () => void {
      dragHandlers.add(handler);
      return () => {
        dragHandlers.delete(handler);
      };
    },

    close(pane: PaneComponentId): void {
      dock.getGroupPanel(pane)?.api.close();
    },

    dispose(): void {
      dragSubscription.dispose();
      subscription.dispose();
      handlers.clear();
      dock.dispose();
    },
  };
}
