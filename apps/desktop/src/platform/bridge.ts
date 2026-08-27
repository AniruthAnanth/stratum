/**
 * The Electron escape hatch, and the only file in `apps/desktop/src` that may
 * import from `@tauri-apps/*` (ARCHITECTURE §8.3, enforced by CI:
 * `rg '@tauri-apps' apps/desktop/src -l` must print this path and nothing else).
 *
 * Everything above this file talks to `Bridge`, an interface with no Tauri
 * vocabulary in it. That is what makes "3 weeks, not a rewrite" a measurable
 * claim rather than a hope, and it is also what lets the whole frontend run
 * under vitest and in a plain browser tab: `detachedBridge()` is a complete
 * implementation that answers with the same shapes and no host.
 *
 * Every Tauri module is loaded with a dynamic `import()`. A static import would
 * pull the Tauri runtime into the entry chunk, where it throws on evaluation in
 * any context that is not a Tauri webview.
 */

import type { SessionId } from "../ipc/hand";

// ---------------------------------------------------------------------------
// The interface the rest of the app codes against
// ---------------------------------------------------------------------------

export type HostPlatform = "macos" | "windows" | "linux";

export interface WindowBounds {
  x: number;
  y: number;
  w: number;
  h: number;
  monitor?: string;
}

export interface OpenPaneWindowOptions {
  /** `pane.html`'s query string is the whole contract with the new window. */
  role: string;
  paneId?: string;
  label: string;
  bounds?: WindowBounds;
}

export interface Bridge {
  /** True in a Tauri webview; false under vitest, in a browser tab, in Storybook. */
  readonly isHosted: boolean;

  /** This webview's window label. `"main"` when there is no host. */
  label(): string;

  /**
   * 06 §2 rule 1: no synchronous IPC anywhere. Every command is a promise and
   * no caller may await one before painting feedback.
   */
  invoke<T>(command: string, args?: Record<string, unknown>): Promise<T>;

  /**
   * `session_subscribe`'s `Channel<Uint8Array>` (CONTRACTS §11). Host → webview
   * events are MessagePack frames, never JSON, so the callback sees bytes.
   * Returns an unsubscribe.
   */
  subscribe(session: SessionId, onFrame: (bytes: Uint8Array) => void): Promise<() => void>;

  /**
   * Rewrites a `stratum-asset://localhost/...` URL into whatever the platform's
   * webview can actually fetch. WebView2 cannot register a real custom scheme,
   * so Tauri maps it to `http://stratum-asset.localhost/...` on Windows; pinning
   * the authority to `localhost` (CONTRACTS §10.1, A21) is what makes the PATH
   * identical on all three, and this function handles the scheme half.
   */
  assetUrl(url: string): string;

  /** `fetch` for the asset scheme, with the `X-Stratum-Token` header attached. */
  fetchAsset(url: string, init?: { signal?: AbortSignal }): Promise<Response>;

  platform(): HostPlatform;

  /**
   * Host → webview *notification* events — the dev-only e2e request channel and
   * nothing else. Product events all travel over `subscribe`'s channel
   * (CONTRACTS §11); this exists because the e2e control surface (ADR-011)
   * reaches the webview by `emit`, which a channel cannot receive. Added by W17
   * with the host; returns an unsubscribe.
   */
  listen<T>(event: string, handler: (payload: T) => void): Promise<() => void>;

  openPaneWindow(options: OpenPaneWindowOptions): Promise<string>;
  closeWindow(label: string): Promise<void>;
  /** Screen-space bounds of this window, for the §13.3 cross-window drag hit-test. */
  outerBounds(): Promise<WindowBounds>;
  onCloseRequested(handler: () => void): Promise<() => void>;
}

// ---------------------------------------------------------------------------
// Token
// ---------------------------------------------------------------------------

let assetToken: string | undefined;

/**
 * Every `stratum-asset://` request carries `X-Stratum-Token`, checked against a
 * `OnceLock<[u8; 32]>` the host generates at startup (CONTRACTS §10.2). The host
 * hands the value to the webview; until it does, requests go out unauthenticated
 * and the handler rejects them, which is the correct failure — a frontend that
 * invented its own token would be a frontend that had disabled the check.
 */
export function setAssetToken(token: string): void {
  assetToken = token;
}

// ---------------------------------------------------------------------------
// Host detection and lazy module loading
// ---------------------------------------------------------------------------

interface TauriGlobals {
  __TAURI_INTERNALS__?: unknown;
}

function hosted(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in (window as TauriGlobals);
}

type CoreModule = typeof import("@tauri-apps/api/core");
type WindowModule = typeof import("@tauri-apps/api/window");
type EventModule = typeof import("@tauri-apps/api/event");

// Memoized per module, so a hundred `invoke` calls share one instantiation.
let corePromise: Promise<CoreModule> | undefined;
function core(): Promise<CoreModule> {
  corePromise ??= import("@tauri-apps/api/core");
  return corePromise;
}

let windowPromise: Promise<WindowModule> | undefined;
function win(): Promise<WindowModule> {
  windowPromise ??= import("@tauri-apps/api/window");
  return windowPromise;
}

let eventPromise: Promise<EventModule> | undefined;
function evt(): Promise<EventModule> {
  eventPromise ??= import("@tauri-apps/api/event");
  return eventPromise;
}

// ---------------------------------------------------------------------------
// Scheme rewriting — shared by both implementations, so dev and packaged agree
// ---------------------------------------------------------------------------

const ASSET_SCHEME = "stratum-asset://localhost/";
const ASSET_HTTP = "http://stratum-asset.localhost/";

/**
 * Pure, and exported for its own test: this is the function that has to be
 * right on a platform nobody is developing on. Both spellings appear in the CSP
 * (CONTRACTS §10.2) precisely because this rewrite exists.
 */
export function rewriteAssetUrl(url: string, platform: HostPlatform): string {
  if (!url.startsWith(ASSET_SCHEME)) return url;
  return platform === "windows" ? ASSET_HTTP + url.slice(ASSET_SCHEME.length) : url;
}

function sniffPlatform(): HostPlatform {
  const ua = typeof navigator === "undefined" ? "" : navigator.userAgent;
  if (/Windows/i.test(ua)) return "windows";
  if (/Mac OS X|Macintosh/i.test(ua)) return "macos";
  return "linux";
}

// ---------------------------------------------------------------------------
// The hosted implementation
// ---------------------------------------------------------------------------

function hostedBridge(): Bridge {
  const platform = sniffPlatform();

  return {
    isHosted: true,

    label(): string {
      // Read from the URL rather than awaiting `getCurrentWindow()`: the label
      // is needed synchronously during boot, before any promise can resolve.
      return new URLSearchParams(location.search).get("label") ?? "main";
    },

    async invoke<T>(command: string, args?: Record<string, unknown>): Promise<T> {
      const { invoke } = await core();
      return invoke<T>(command, args);
    },

    async subscribe(session: SessionId, onFrame: (bytes: Uint8Array) => void): Promise<() => void> {
      const { Channel, invoke } = await core();
      const channel = new Channel<Uint8Array>();
      channel.onmessage = onFrame;
      await invoke("session_subscribe", { session, channel });
      let closed = false;
      return () => {
        if (closed) return;
        closed = true;
        void invoke("session_close", { session });
      };
    },

    assetUrl(url: string): string {
      return rewriteAssetUrl(url, platform);
    },

    async fetchAsset(url: string, init?: { signal?: AbortSignal }): Promise<Response> {
      const headers: Record<string, string> = {};
      if (assetToken !== undefined) headers["X-Stratum-Token"] = assetToken;
      return fetch(rewriteAssetUrl(url, platform), { headers, signal: init?.signal });
    },

    platform(): HostPlatform {
      return platform;
    },

    async listen<T>(event: string, handler: (payload: T) => void): Promise<() => void> {
      const { listen } = await evt();
      return listen<T>(event, (e) => handler(e.payload));
    },

    async openPaneWindow(options: OpenPaneWindowOptions): Promise<string> {
      // The host owns window creation (CONTRACTS §11 `window_open_pane`) because
      // it also owns the per-window session binding the asset handler checks.
      // Creating a `WebviewWindow` from here would produce a window that cannot
      // read its own data.
      return this.invoke<{ label: string }>("window_open_pane", {
        role: options.role,
        paneId: options.paneId,
        label: options.label,
        bounds: options.bounds,
      }).then((r) => r.label);
    },

    async closeWindow(label: string): Promise<void> {
      await this.invoke<void>("window_close", { label });
    },

    async outerBounds(): Promise<WindowBounds> {
      const { getCurrentWindow } = await win();
      const w = getCurrentWindow();
      const [pos, size] = await Promise.all([w.outerPosition(), w.outerSize()]);
      return { x: pos.x, y: pos.y, w: size.width, h: size.height };
    },

    async onCloseRequested(handler: () => void): Promise<() => void> {
      const { getCurrentWindow } = await win();
      return getCurrentWindow().onCloseRequested(() => handler());
    },
  };
}

// ---------------------------------------------------------------------------
// The detached implementation
// ---------------------------------------------------------------------------

/**
 * A complete `Bridge` with no host behind it. Not a stub for tests only: this
 * is what runs in `pnpm dev` in a browser tab, and W13–W16 develop against it
 * plus W07's mock engine before W17's host exists.
 *
 * It answers rather than throws, because a frontend that crashes without a host
 * cannot be developed without one, and every unit after this one has to.
 */
export function detachedBridge(overrides: Partial<Bridge> = {}): Bridge {
  const platform = sniffPlatform();
  const base: Bridge = {
    isHosted: false,
    label: () => new URLSearchParams(globalThis.location?.search ?? "").get("label") ?? "main",
    invoke: <T>(command: string): Promise<T> =>
      Promise.reject(new Error(`no host: invoke(${command})`)),
    subscribe: () => Promise.resolve(() => {}),
    assetUrl: (url) => rewriteAssetUrl(url, platform),
    fetchAsset: (url, init) => fetch(rewriteAssetUrl(url, platform), { signal: init?.signal }),
    platform: () => platform,
    listen: () => Promise.resolve(() => {}),
    openPaneWindow: (o) => Promise.resolve(o.label),
    closeWindow: () => Promise.resolve(),
    outerBounds: () => Promise.resolve({ x: 0, y: 0, w: 1280, h: 800 }),
    onCloseRequested: () => Promise.resolve(() => {}),
  };
  return { ...base, ...overrides };
}

// ---------------------------------------------------------------------------
// The singleton
// ---------------------------------------------------------------------------

let current: Bridge | undefined;

export function bridge(): Bridge {
  current ??= hosted() ? hostedBridge() : detachedBridge();
  return current;
}

/** Test seam. Production code never calls this. */
export function setBridge(b: Bridge | undefined): void {
  current = b;
}
