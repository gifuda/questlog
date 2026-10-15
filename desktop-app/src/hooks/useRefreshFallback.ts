import { useEffect } from "react";

/**
 * Self-healing fallback for file-change refresh.
 *
 * The Tauri watcher emits `*-changed` events when the MCP writes the board
 * files, but the MCP writes atomically (tmp file + rename) and macOS fsevents
 * can coalesce or drop those rename events under rapid successive writes.
 * A light poll + window-focus refresh guarantees the UI catches up within
 * seconds without ever needing an app restart.
 *
 * All default-interval subscribers share ONE module-level timer (S5): nine
 * independent intervals became a synchronized burst with mixed-age data
 * across panels; a single tick fans out to every subscriber at the same
 * moment. Custom intervals keep their own timer (only used by special
 * cases; today none).
 */

const TICK_MS = 10_000;
const subscribers = new Set<() => void>();
let sharedTimer: number | null = null;

function ensureSharedTimer() {
  if (sharedTimer === null) {
    sharedTimer = window.setInterval(() => {
      // Guard each subscriber: a synchronous throw in one refresh must not
      // starve the rest of the tick (current refresh fns are async and
      // reject via microtasks, this is future-proofing).
      for (const fn of subscribers) {
        try {
          fn();
        } catch {
          // ignore — next tick retries
        }
      }
    }, TICK_MS);
  }
}

function teardownIfIdle() {
  if (subscribers.size === 0 && sharedTimer !== null) {
    clearInterval(sharedTimer);
    sharedTimer = null;
  }
}

export function useRefreshFallback(refresh: () => void, intervalMs = TICK_MS) {
  useEffect(() => {
    const onFocus = () => refresh();
    window.addEventListener("focus", onFocus);
    if (intervalMs === TICK_MS) {
      subscribers.add(refresh);
      ensureSharedTimer();
    } else {
      var timer = setInterval(refresh, intervalMs);
    }
    return () => {
      window.removeEventListener("focus", onFocus);
      if (intervalMs === TICK_MS) {
        subscribers.delete(refresh);
        teardownIfIdle();
      } else {
        clearInterval(timer);
      }
    };
  }, [refresh, intervalMs]);
}
