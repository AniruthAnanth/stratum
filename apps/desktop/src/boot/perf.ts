/**
 * The performance budgets of 06 §15.1, enforced rather than documented.
 *
 * "a dev-mode `PerformanceObserver` logs every long task". A budget that lives
 * only in a table is a budget nobody finds out they broke; this reports the
 * overrun with the name of the path that owns it, so the first person to notice
 * is the person who caused it rather than a user on a slower machine.
 *
 * Production builds install nothing: the observer itself costs main-thread time,
 * and §15's whole point is that the main thread is scarce.
 */

/** 06 §15.1, verbatim. Keys are the path names `mark()` is called with. */
export const BUDGETS_MS: Readonly<Record<string, number>> = {
  "keystroke.segment": 6,
  "run.glyph": 16,
  "log.firstPaint": 50,
  "card.mount": 8,
  "card.graphMount": 25,
  "data.window": 12,
  "log.jump": 16,
  "layout.switch": 120,
  "boot.interactive": 400,
};

type Reporter = (path: string, ms: number, budget: number) => void;

let report: Reporter = (path, ms, budget) => {
  console.warn(`[perf] ${path} took ${ms.toFixed(1)} ms, budget ${budget} ms`);
};

export function setPerfReporter(next: Reporter): void {
  report = next;
}

/** Times a synchronous span and reports it if it exceeds its budget. */
export function mark<T>(path: string, work: () => T): T {
  const budget = BUDGETS_MS[path];
  if (budget === undefined) return work();
  const started = performance.now();
  try {
    return work();
  } finally {
    const elapsed = performance.now() - started;
    if (elapsed > budget) report(path, elapsed, budget);
  }
}

let observer: PerformanceObserver | undefined;

/** Long-task observation. Dev builds only; a no-op where the entry type is absent. */
export function installLongTaskObserver(): () => void {
  if (observer !== undefined) return () => {};
  const supported = globalThis.PerformanceObserver?.supportedEntryTypes ?? [];
  if (!supported.includes("longtask")) return () => {};

  observer = new PerformanceObserver((list) => {
    for (const entry of list.getEntries()) {
      // 50 ms is the platform's own long-task threshold; anything it reports has
      // already blown every budget in the table above.
      report("longtask", entry.duration, 50);
    }
  });
  observer.observe({ entryTypes: ["longtask"] });
  return () => {
    observer?.disconnect();
    observer = undefined;
  };
}
