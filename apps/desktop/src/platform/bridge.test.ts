/**
 * The platform boundary — ARCHITECTURE §8.3 and CONTRACTS §10.1.
 *
 * The first test here is the CI invariant itself, run from inside the unit that
 * has to satisfy it: ARCHITECTURE §8.3 greps the whole of `apps/desktop/src` for
 * the Tauri package scope and requires exactly one hit, `platform/bridge.ts`.
 * Asserting it here means a violation fails on the developer's machine rather
 * than in the pipeline, which is where it is cheap to fix.
 *
 * Note that this file never spells the scope out; see `SCOPE` below.
 */

import { readFileSync, readdirSync, statSync } from "node:fs";
import { dirname, join, relative, resolve, sep } from "node:path";
import { describe, expect, it } from "vitest";
import type { ResultId, SessionId } from "../ipc/hand";
import { appUrl, framePageUrl, graphUrl, rawResultUrl, resultTableUrl } from "./asset";
import {
  type HostPlatform,
  detachedBridge,
  rewriteAssetUrl,
  setAssetToken,
  setBridge,
} from "./bridge";

const srcRoot = ((): string => {
  let dir = process.cwd();
  while (!existsDir(join(dir, "apps/desktop/src"))) {
    const parent = dirname(dir);
    if (parent === dir) throw new Error("apps/desktop/src not found above the cwd");
    dir = parent;
  }
  return join(dir, "apps/desktop/src");
})();

function existsDir(path: string): boolean {
  try {
    return statSync(path).isDirectory();
  } catch {
    return false;
  }
}

function* walk(dir: string): Generator<string> {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const full = join(dir, entry.name);
    if (entry.isDirectory()) yield* walk(full);
    else if (/\.(ts|tsx)$/.test(entry.name)) yield full;
  }
}

/**
 * The package scope, assembled rather than written.
 *
 * ARCHITECTURE §8.3's check is a `rg` over `apps/desktop/src`, and it does not
 * care whether a match is an import or a string in a test. Spelling the scope
 * out here would make THIS file the second match and break the very invariant
 * it asserts — a genuinely funny way to fail CI, and one worth not shipping.
 */
const SCOPE = ["@tauri", "-apps"].join("");

describe("the Electron escape hatch (ARCHITECTURE §8.3)", () => {
  it("is imported from exactly one file", () => {
    const offenders = [...walk(srcRoot)]
      .filter((file) => readFileSync(file, "utf8").includes(SCOPE))
      .map((file) => relative(srcRoot, file).split(sep).join("/"))
      .sort();
    expect(offenders).toEqual(["platform/bridge.ts"]);
  });

  it("keeps every Tauri import dynamic", () => {
    // A static import pulls the Tauri runtime into the entry chunk, where it
    // throws on evaluation in a plain browser tab — which is where W13-W16
    // develop against W07's mock until W17's host exists.
    const source = readFileSync(resolve(srcRoot, "platform/bridge.ts"), "utf8");
    expect(source).not.toMatch(new RegExp(`^import .*from "${SCOPE}`, "m"));
    expect(source).toContain(`import("${SCOPE}/api/core")`);
  });
});

describe("rewriteAssetUrl (CONTRACTS §10.1, A21)", () => {
  const url = "stratum-asset://localhost/frame/7/default/page?state=17";

  it("leaves the scheme alone where the webview can register one", () => {
    for (const platform of ["macos", "linux"] as const) {
      expect(rewriteAssetUrl(url, platform)).toBe(url);
    }
  });

  it("maps to the http spelling on Windows, keeping the path identical", () => {
    // WebView2 cannot register a real custom scheme, so Tauri maps it. Pinning
    // the authority to `localhost` is what makes `/{kind}/{session}/…` the same
    // path on all three; the kind segment vanishing on two of them is the §27
    // defect this rewrite exists to prevent.
    const windows = rewriteAssetUrl(url, "windows");
    expect(windows).toBe("http://stratum-asset.localhost/frame/7/default/page?state=17");
    const path = (u: string): string => new URL(u).pathname;
    expect(path(windows)).toBe(path(url));
    expect(path(windows).split("/")[1]).toBe("frame");
  });

  it("passes through anything that is not an asset URL", () => {
    for (const platform of ["macos", "windows", "linux"] as HostPlatform[]) {
      expect(rewriteAssetUrl("https://example.invalid/x", platform)).toBe(
        "https://example.invalid/x",
      );
    }
  });
});

describe("the asset URL space", () => {
  const session = 7 as SessionId;
  const result = 41 as ResultId;

  it("builds every documented shape with `localhost` as the authority", () => {
    expect(rawResultUrl(session, result)).toBe("stratum-asset://localhost/result/7/41/raw");
    expect(resultTableUrl(session, result)).toBe("stratum-asset://localhost/result/7/41/table");
    expect(graphUrl(session, result, "svg")).toBe("stratum-asset://localhost/graph/7/41.svg");
    expect(graphUrl(session, result, "png")).toBe("stratum-asset://localhost/graph/7/41.png");
    expect(appUrl("/fonts/plex.woff2")).toBe("stratum-asset://localhost/app/fonts/plex.woff2");
  });

  it("encodes a frame name rather than splicing it into the path", () => {
    // The host rejects a segment containing `..`, `/`, `\` or NUL after one
    // decode. Encoding here means a frame legitimately named `a/b` survives and
    // a frame named `..` is refused by the host rather than by a coincidence of
    // string concatenation.
    const url = framePageUrl(session, "a/b", {
      state: 17,
      row0: 0,
      nrows: 40,
      cols: [0, 1, 2],
      render: "display",
      seq: 1,
    });
    expect(url).toContain("/frame/7/a%2Fb/page?");
    expect(new URL(url).pathname.split("/").filter(Boolean)).toHaveLength(4);
  });

  it("puts the page arguments in a fixed order, because the URL is the cache key", () => {
    const url = framePageUrl(session, "default", {
      state: 17,
      row0: 200,
      nrows: 40,
      cols: [0, 3],
      order: 9,
      render: "edit",
      seq: 12,
    });
    expect(url.split("?")[1]).toBe(
      "state=17&row0=200&nrows=40&cols=0%2C3&order=9&render=edit&seq=12",
    );
  });
});

describe("the detached bridge", () => {
  it("answers rather than throws, so the app runs with no host", async () => {
    const bridge = detachedBridge();
    expect(bridge.isHosted).toBe(false);
    expect(bridge.label()).toBe("main");
    await expect(bridge.subscribe(1 as SessionId, () => {})).resolves.toBeInstanceOf(Function);
    await expect(bridge.outerBounds()).resolves.toMatchObject({ w: 1280, h: 800 });
    await expect(bridge.openPaneWindow({ role: "pane", label: "p:pane:results" })).resolves.toBe(
      "p:pane:results",
    );
  });

  it("rejects `invoke` by name, because a silent success would be a lie", async () => {
    await expect(detachedBridge().invoke("layout_load")).rejects.toThrow(/layout_load/);
  });

  it("is installable as the singleton for a test", () => {
    const stub = detachedBridge({ platform: () => "windows" });
    setBridge(stub);
    setAssetToken("t");
    setBridge(undefined);
    expect(stub.platform()).toBe("windows");
  });
});
