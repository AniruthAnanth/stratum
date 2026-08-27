/**
 * The error boundary and the global error sink.
 *
 * A statistics tool that dies silently is worse than one that dies loudly: the
 * user is looking at numbers, and a pane that has stopped updating still shows
 * the last ones it had. So a crashed pane is REPLACED by a visible notice rather
 * than left displaying stale output, and an unhandled rejection reaches the
 * status bar instead of only the console.
 */

import { ErrorBoundary, type JSX, createSignal } from "solid-js";
import { setCommandErrorSink } from "../keys/registry";
import { setPerfReporter } from "./perf";

export interface AppError {
  source: string;
  message: string;
  at: number;
}

const [errors, setErrors] = createSignal<AppError[]>([]);

export const appErrors = errors;

/** Keeps the last 50; the point is the most recent one, not an audit log. */
export function reportError(source: string, error: unknown): void {
  const message = error instanceof Error ? error.message : String(error);
  setErrors((prev) => [...prev.slice(-49), { source, message, at: Date.now() }]);
}

export function clearErrors(): void {
  setErrors([]);
}

/** The most recent error, for the status bar's notice slot. */
export function latestError(): AppError | undefined {
  return errors().at(-1);
}

export function installErrorHandlers(target: Window = window): () => void {
  const onError = (event: ErrorEvent): void => {
    reportError("window", event.error ?? event.message);
  };
  const onRejection = (event: PromiseRejectionEvent): void => {
    reportError("promise", event.reason);
  };

  target.addEventListener("error", onError);
  target.addEventListener("unhandledrejection", onRejection);
  setCommandErrorSink((id, error) => {
    reportError(`command ${id}`, error);
  });
  setPerfReporter((path, ms, budget) => {
    // A budget overrun is not an error the user should see; it is a developer
    // signal. It goes to the console and to the dev-mode status readout only.
    if (import.meta.env.DEV) {
      console.warn(`[perf] ${path} ${ms.toFixed(1)} ms > ${budget} ms`);
    }
  });

  return () => {
    target.removeEventListener("error", onError);
    target.removeEventListener("unhandledrejection", onRejection);
  };
}

export interface PaneBoundaryProps {
  name: string;
  children: JSX.Element;
}

/**
 * One boundary per pane, not one per window. A crashed Variables pane must not
 * take the editor — and the user's unsaved text — with it.
 */
export function PaneBoundary(props: PaneBoundaryProps): JSX.Element {
  return (
    <ErrorBoundary
      fallback={(error: unknown, reset: () => void) => {
        reportError(props.name, error);
        return (
          <div class="pane-error" role="alert">
            <p class="t-body">{`${props.name} stopped.`}</p>
            <p class="t-small text-meta">
              {error instanceof Error ? error.message : String(error)}
            </p>
            <button type="button" class="btn btn--default" onClick={reset}>
              Reload pane
            </button>
          </div>
        );
      }}
    >
      {props.children}
    </ErrorBoundary>
  );
}
