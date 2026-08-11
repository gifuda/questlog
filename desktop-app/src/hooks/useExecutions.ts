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
  | "verifying"
  | "passed"
  | "failed"
  | "cancelled"
  | "cancel_requested"
  | "pause_requested"
  | "timed_out"
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
  // Execution memory fields
  attempt_no: number;
  parent_attempt_id?: string;
  bundle_path?: string;
  executor?: string;
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

  // Returns the new child execution_id (Evidence-aware Retry).
  const retry = useCallback(async (id: string): Promise<string | null> => {
    try {
      return await invoke<string>("retry_execution", { id });
    } catch (err) {
      console.warn("Failed to retry execution:", err);
      return null;
    }
  }, []);

  const pause = useCallback(async (id: string) => {
    await invoke("pause_execution", { id });
  }, []);

  const cleanupStale = useCallback(async () => {
    await invoke("cleanup_stale_executions");
  }, []);

  return { cancel, retry, pause, cleanupStale };
}

/**
 * useRunner — manage the Runner enable/disable state at runtime.
 * Lets the user toggle auto-execution from the UI.
 */
export function useRunner() {
  const [enabled, setEnabled] = useState(false);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  // On mount, check the backend's current Runner state
  useEffect(() => {
    invoke<boolean>("is_runner_running")
      .then(setEnabled)
      .catch((err) => setError(String(err)))
      .finally(() => setLoading(false));
  }, []);

  const toggle = useCallback(async () => {
    const next = !enabled;
    try {
      setLoading(true);
      setError(null);
      const result = await invoke<boolean>("set_runner_enabled", { enabled: next });
      setEnabled(result);
    } catch (err) {
      setError(String(err));
    } finally {
      setLoading(false);
    }
  }, [enabled]);

  return { enabled, loading, error, toggle };
}

// ─── Per-project Runner control ───

export interface ProjectRunnerStatus {
  global_enabled: boolean;
  project_enabled: boolean;
  effective: boolean;
}

/**
 * useProjectRunner — manage Runner execution for a specific project.
 * The global toggle must also be ON for execution to happen.
 */
export function useProjectRunner(projectPath: string | null) {
  const [status, setStatus] = useState<ProjectRunnerStatus | null>(null);
  const [loading, setLoading] = useState(false);

  const refresh = useCallback(async () => {
    if (!projectPath) return;
    try {
      const data = await invoke<ProjectRunnerStatus>("get_runner_project_status", {
        projectPath,
      });
      setStatus(data);
    } catch {
      setStatus(null);
    }
  }, [projectPath]);

  useEffect(() => {
    refresh();
  }, [refresh]);

  const toggle = useCallback(async () => {
    if (!projectPath || !status) return;
    try {
      setLoading(true);
      const next = !status.project_enabled;
      const result = await invoke<ProjectRunnerStatus>("set_runner_project_enabled", {
        projectPath,
        enabled: next,
      });
      setStatus(result);
    } catch (err) {
      console.warn("Failed to toggle project runner:", err);
    } finally {
      setLoading(false);
    }
  }, [projectPath, status]);

  return { status, loading, toggle, refresh };
}

/**
 * Get the auto_execute setting for a specific epic.
 * Returns null = inherit project, true/false = explicit override.
 */
export async function getEpicAutoExecute(
  epicId: string,
  projectPath: string
): Promise<boolean | null> {
  try {
    return await invoke<boolean | null>("get_epic_auto_execute", { epicId, projectPath });
  } catch {
    return null;
  }
}

/**
 * Set the auto_execute setting for a specific epic.
 */
export async function setEpicAutoExecute(
  epicId: string,
  projectPath: string,
  autoExecute: boolean | null
): Promise<void> {
  await invoke("set_epic_auto_execute", { epicId, projectPath, autoExecute });
}

export interface DecisionRequest {
  request_id: string;
  epic_id: string;
  step_id: string;
  execution_id: string;
  kind: string;
  question: string;
  options: string[];
  evidence_refs: string[];
  blocking: boolean;
  package_revision: number;
  resolved: boolean;
  resolution?: string;
  created_at: string;
}

export function useDecisionRequests() {
  const [requests, setRequests] = useState<DecisionRequest[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      const data = await invoke<DecisionRequest[]>("get_decision_requests", {
        includeResolved: false,
      });
      setRequests(data);
      setError(null);
    } catch (err) {
      setError(String(err));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    refresh();
    const interval = setInterval(refresh, 5000);
    return () => clearInterval(interval);
  }, [refresh]);

  const resolve = useCallback(async (requestId: string, resolution: string) => {
    await invoke("resolve_decision_request", { requestId, resolution });
    await refresh();
  }, [refresh]);

  return { requests, loading, error, refresh, resolve };
}

// ─── AttemptBundle Trace Query hooks ───

export interface AttemptManifest {
  execution_id: string;
  attempt_no: number;
  parent_attempt_id?: string;
  package_id: string;
  plan_revision: number;
  executor: string;
  hypothesis?: string;
  base_sha: string;
  candidate_sha?: string;
  epic_id: string;
  step_id: string;
  task_package_ref: string;
  environment_snapshot_ref: string;
  raw_trace_ref: string;
  stdout_ref?: string;
  stderr_ref?: string;
  diff_ref?: string;
  candidate_ref?: string;
  verifier_ref?: string;
  decisions_ref?: string;
  outcome: string;
  integrity_hash?: string;
  created_at: number;
  sealed_at?: number;
}

export interface BundleFile {
  name: string;
  size: number;
}

export interface AttemptDetail {
  manifest: AttemptManifest;
  execution_id: string;
  files: BundleFile[];
}

/**
 * useAttemptBundles — fetch all attempt bundles for a given step.
 * Returns manifest-level summaries sorted by attempt_no ascending.
 */
export function useAttemptBundles(stepId: string | null) {
  const [bundles, setBundles] = useState<AttemptManifest[]>([]);
  const [loading, setLoading] = useState(false);

  const refresh = useCallback(async () => {
    if (!stepId) {
      setBundles([]);
      return;
    }
    try {
      setLoading(true);
      const data = await invoke<AttemptManifest[]>("get_attempt_bundles", { stepId });
      setBundles(data);
    } catch (err) {
      // Command may not be available in older app versions
      setBundles([]);
    } finally {
      setLoading(false);
    }
  }, [stepId]);

  useEffect(() => {
    refresh();
  }, [refresh]);

  return { bundles, loading, refresh };
}

/**
 * Fetch full detail for a single attempt bundle (manifest + file list).
 */
export async function getAttemptDetail(executionId: string): Promise<AttemptDetail | null> {
  try {
    return await invoke<AttemptDetail>("get_attempt_detail", { executionId });
  } catch {
    return null;
  }
}
