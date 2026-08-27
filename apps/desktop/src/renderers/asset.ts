/**
 * `stratum-asset://` for an opaque [`AssetRefView`] path — CONTRACTS §10.1.
 *
 * `platform/asset.ts` (W12) builds these URLs from `(session, result)`, which is
 * the right shape for every call site that HAS a session. A renderer does not:
 * `ResultEnvelope` carries no `SessionId`, and an `AssetRef.path` is already the
 * whole path the host will match. So this is the `AssetRef` case of the same
 * scheme, encoded segment-by-segment with the same rule, and it exists here
 * rather than in W12's file because that file is W12's.
 */

const ROOT = "stratum-asset://localhost";

/**
 * The host percent-decodes each segment exactly once and then rejects any that
 * contains `..`, `/`, `\` or NUL (§10.2, W03's threat model). Encoding per
 * segment is what lets a graph legitimately named `by/region` survive while a
 * path element of `..` still reaches the host as `..` and is refused there —
 * the check belongs on the host, and this must not launder input past it.
 */
export function assetUrl(path: string): string {
  const segments = path.split("/").filter((s) => s.length > 0);
  return `${ROOT}/${segments.map(encodeURIComponent).join("/")}`;
}
