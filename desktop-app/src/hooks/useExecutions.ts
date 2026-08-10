/**
 * useExecutions — React hook for fetching execution state from the Tauri backend.
 *
 * Subscribes to execution state changes and provides the data to UI components.
 * The execution state comes from the Runtime Store (not .epics.json).
 */

import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

export interface ExecutionLease {
  execution_id: string;
  acquired_at: number;
  last_heartbeat: number;
  pid: number;
  runner_id: string;
}

export type ExecutionStatus =
  | "queued"
  | "running"
  | "candidate"
  | "passed"
  | "failed"
  | "cancelled"
  | "blocked"
  | "stale";

export interface Execution {
  id: string;
  package_id: string;
  epic_id: string;
  step_id: string;
  project_path: string;
  status: ExecutionStatus;
  lease: ExecutionLease | null;
  cli_session: string | null;
  worktree_path: string | null;
  branch: string | null;
  base_sha: string;
  candidate_sha: string | null;
  attempts: number;
  last_error: string | null;
  created_at: number;
  updated_at: number;
}

export function useExecutions() {
  const [executions, setExecutions] = useState<Execution[]>([]);
  const [loading, setLoading] = useState(true);

  const refresh = useCallback(async () => {
    try {
      const data = await invoke<Execution[]>("get_executions");
      setExecutions(data);
    } catch (err) {
      // If the command isn't available (older app version), silently ignore
      console.warn("Failed to fetch executions:", err);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    refresh();

    // Listen for execution state changes
    // The watcher emits this when executions.json changes
    const unlisten = listen("executions-changed", () => {
      refresh();
    });

    // Refresh every 5s while mounted (for heartbeat updates)
    const interval = setInterval(refresh, 5000);

    return () => {
      unlisten.then((fn) => fn());
      clearInterval(interval);
    };
  }, [refresh]);

  return { executions, loading, refresh };
}

/**
 * Execution actions — call Tauri commands to control execution state.
 */
export function useExecutionActions() {
  const cancel = useCallback(async (id: string) => {
    await invoke("cancel_execution", { id });
  }, []);

  const retry = useCallback(async (id: string) => {
    await invoke("retry_execution", { id });
  }, []);

  const pause = useCallback(async (id: string) => {
    await invoke("pause_execution", { id });
  }, []);

  const cleanupStale = useCallback(async () => {
    await invoke("cleanup_stale_executions");
  }, []);

  return { cancel, retry, pause, cleanupStale };
}
