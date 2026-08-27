/**
 * Accelerator LABELS — CONTRACTS §11 `menu_accelerator`, 08 §5.4 and §12.
 *
 * The host owns the native menus, so the host owns the string a menu item
 * displays, and a tooltip that disagrees with the menu beside it is a bug the
 * user will find in the first minute. So the label comes from
 * `menu_accelerator({ action, preset })` — `MenuHost::accelerator(ActionId,
 * KeymapPreset)` on the Rust side, rendered by `Accelerator::display` — and
 * this module renders whatever it is handed.
 *
 * WHY there is no local renderer for the detached bridge (`pnpm dev` in a
 * browser tab, every vitest run): a modifier table here would be a SECOND
 * answer to a question the host already answers, and it would answer it worse,
 * because glyph choice, ordering and separators are platform knowledge the
 * webview does not have. A detached bridge has no host and therefore has no
 * answer, so the honest result is `null` — the same `null` the host itself
 * returns for "this action has no accelerator", which callers already render as
 * nothing. Ruled by W10 in
 * `crates/stratum-platform/tests/frontend_accelerator_literals.rs`, which is
 * also the CI assertion that keeps the table from growing back.
 */

import { bridge } from "../platform/bridge";
import type { KeymapPreset } from "./presets";

/**
 * The label for a command, asked of the host. `null` means "render nothing":
 * either the action has no accelerator, or there is no host to ask.
 */
export async function accelerator(command: string, preset: KeymapPreset): Promise<string | null> {
  // Short-circuited rather than left to the rejection below, so that "no host"
  // costs no IPC round-trip on a dev page that may ask for every row of the
  // command palette at once.
  if (!bridge().isHosted) return null;
  try {
    return await bridge().invoke<string | null>("menu_accelerator", { action: command, preset });
  } catch {
    // A host that is up but cannot answer must still not throw inside a menu or
    // palette render. Missing label, not a broken frame.
    return null;
  }
}
