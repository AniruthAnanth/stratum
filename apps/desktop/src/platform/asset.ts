/**
 * `stratum-asset://` URL construction — CONTRACTS §10.1.
 *
 * The authority is always the literal `localhost`, so the PATH is
 * `/{kind}/{session}/…` on all three platforms. Building these by hand at each
 * call site is how the kind segment silently disappears on two of them; there
 * is one builder and it is here.
 *
 * The scheme→http rewrite for WebView2 lives in `bridge.ts`, because it is the
 * only platform-conditional step and platform detection is the bridge's job.
 */

import type { ResultId, SessionId } from "../ipc/hand";

const ROOT = "stratum-asset://localhost";

/**
 * Percent-encodes a path segment. The host rejects any segment containing `..`,
 * `/`, `\` or NUL after a single decode (threat model, W03) — encoding here
 * means a frame legitimately named `a/b` survives, and a frame named `..` is
 * rejected by the host rather than by an accident of string concatenation.
 */
const seg = (s: string): string => encodeURIComponent(s);

export const rawResultUrl = (session: SessionId, result: ResultId): string =>
  `${ROOT}/result/${seg(String(session))}/${seg(String(result))}/raw`;

export const resultTableUrl = (session: SessionId, result: ResultId): string =>
  `${ROOT}/result/${seg(String(session))}/${seg(String(result))}/table`;

export const graphUrl = (session: SessionId, result: ResultId, format: "svg" | "png"): string =>
  `${ROOT}/graph/${seg(String(session))}/${seg(String(result))}.${format}`;

export const appUrl = (path: string): string =>
  `${ROOT}/app/${path.split("/").filter(Boolean).map(seg).join("/")}`;

/** The arguments of `PageRequest` (CONTRACTS §8.1), minus the ones in the path. */
export interface PageQuery {
  state: number;
  row0: number;
  nrows: number;
  cols: readonly number[];
  order?: number;
  render: "display" | "edit";
  seq: number;
}

/**
 * The one frame transport (A13). A `data_page` command was deleted from §11
 * because scrolling needs `AbortController` cancellation and HTTP caching, and
 * a Tauri command gives neither — so this URL is also the cache key, and the
 * parameter order below is fixed for that reason.
 */
export function framePageUrl(session: SessionId, frame: string, q: PageQuery): string {
  const params = new URLSearchParams();
  params.set("state", String(q.state));
  params.set("row0", String(q.row0));
  params.set("nrows", String(q.nrows));
  params.set("cols", q.cols.join(","));
  if (q.order !== undefined) params.set("order", String(q.order));
  params.set("render", q.render);
  params.set("seq", String(q.seq));
  return `${ROOT}/frame/${seg(String(session))}/${seg(frame)}/page?${params.toString()}`;
}
