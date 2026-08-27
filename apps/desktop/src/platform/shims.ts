/**
 * Capability shims for environments that are missing something the shell needs.
 *
 * Two consumers, one file: jsdom under vitest (no `ResizeObserver`, no
 * `matchMedia`, no `scheduler.postTask`), and any webview that predates one of
 * these. Each shim installs only when the real thing is absent, so a browser
 * that has it keeps its own implementation.
 *
 * This is `vite.config.ts`'s `test.setupFiles`. It is idempotent and free of
 * test-framework imports, so it is equally importable from production boot.
 */

/**
 * A `ResizeObserver` that never fires.
 *
 * dockview measures with `getBoundingClientRect` on construction and then
 * relies on the observer for later changes; jsdom reports every rect as zero
 * either way. Firing synthetic entries would invent sizes the layout never had
 * and make a test pass for a reason production does not share, so the shim
 * stays inert and the dock tests drive `layout(w, h)` explicitly.
 */
class InertResizeObserver implements ResizeObserver {
  private readonly targets = new Set<Element>();
  // No constructor: the callback a caller passes is accepted and dropped, which
  // is what "never fires" means.
  observe(target: Element): void {
    this.targets.add(target);
  }
  unobserve(target: Element): void {
    this.targets.delete(target);
  }
  disconnect(): void {
    this.targets.clear();
  }
}

function installResizeObserver(g: typeof globalThis): void {
  if ("ResizeObserver" in g) return;
  (g as { ResizeObserver?: unknown }).ResizeObserver = InertResizeObserver;
}

function installMatchMedia(g: typeof globalThis): void {
  if (typeof g.window === "undefined" || typeof g.window.matchMedia === "function") return;
  g.window.matchMedia = (query: string): MediaQueryList =>
    ({
      matches: false,
      media: query,
      onchange: null,
      addEventListener: () => {},
      removeEventListener: () => {},
      addListener: () => {},
      removeListener: () => {},
      dispatchEvent: () => false,
    }) as unknown as MediaQueryList;
}

/**
 * `scheduler.postTask` slices a >256 KB payload into 8 ms chunks (06 §15.1).
 * Where it is missing the fallback is a macrotask, not a microtask: a microtask
 * would run before paint and defeat the entire point of slicing.
 */
function installPostTask(g: typeof globalThis): void {
  const scoped = g as { scheduler?: { postTask?: unknown } };
  if (scoped.scheduler?.postTask !== undefined) return;
  scoped.scheduler = {
    postTask: <T>(callback: () => T): Promise<T> =>
      new Promise<T>((resolve, reject) => {
        setTimeout(() => {
          try {
            resolve(callback());
          } catch (error) {
            reject(error);
          }
        }, 0);
      }),
  };
}

export function installShims(g: typeof globalThis = globalThis): void {
  installResizeObserver(g);
  installMatchMedia(g);
  installPostTask(g);
}

installShims();
