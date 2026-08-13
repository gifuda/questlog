import { useEffect } from "react";

/**
 * Self-healing fallback for file-change refresh.
 *
 * The Tauri watcher emits `*-changed` events when the MCP writes the board
 * files, but the MCP writes atomically (tmp file + rename) and macOS fsevents
 * can coalesce or drop those rename events under rapid successive writes.
 * A light poll + window-focus refresh guarantees the UI catches up within
 * seconds without ever needing an app restart.
 */
export function useRefreshFallback(refresh: () => void, intervalMs = 10_000) {
  useEffect(() => {
    const onFocus = () => refresh();
    window.addEventListener("focus", onFocus);
    const interval = setInterval(refresh, intervalMs);
    return () => {
      window.removeEventListener("focus", onFocus);
      clearInterval(interval);
    };
  }, [refresh]);
}
