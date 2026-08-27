/**
 * Window role dispatch — 06 §13.1, §13.3.
 *
 * `pane.html` boots the same bundle as `index.html` with a tiny shell, and the
 * host tells it what it is through the query string:
 *
 *     pane.html?role=pane&paneId=results&label=proj:pane:results
 *
 * Parsing that in one place means the shell, the keyboard listener and the
 * detach machinery all agree about which window they are in — and it means a
 * malformed query produces the main window rather than a blank one.
 */

import { type PaneComponentId, isPaneComponentId } from "../dock/panes";
import type { WindowRole } from "../ipc/hand";

export interface WindowIdentity {
  role: WindowRole;
  label: string;
  /** Present when `role === "pane"`. */
  paneId?: PaneComponentId;
  /** `${project}` prefix of the label, used to name sibling windows. */
  project: string;
}

const ROLES: readonly WindowRole[] = ["main", "editor", "data", "graph", "pane", "viewer", "prefs"];

export function parseIdentity(search: string, documentRole?: string | null): WindowIdentity {
  const params = new URLSearchParams(search);
  const raw = params.get("role") ?? documentRole ?? "main";
  const role = (ROLES as readonly string[]).includes(raw) ? (raw as WindowRole) : "main";
  const label = params.get("label") ?? role;
  const project = label.includes(":") ? (label.split(":")[0] ?? "stratum") : "stratum";

  const paneParam = params.get("paneId");
  const paneId = paneParam !== null && isPaneComponentId(paneParam) ? paneParam : undefined;

  // A `pane` window with no valid pane id has nothing to show. Falling back to
  // the main shell is wrong (it would open a second dock); reporting it as
  // `viewer` would be a lie. It becomes a main-role window so the error surface
  // is visible rather than a blank pane.
  if (role === "pane" && paneId === undefined) {
    return { role: "main", label, project };
  }

  return paneId === undefined ? { role, label, project } : { role, label, paneId, project };
}

export function currentIdentity(): WindowIdentity {
  return parseIdentity(
    globalThis.location?.search ?? "",
    document.documentElement.dataset["role"] ?? null,
  );
}
