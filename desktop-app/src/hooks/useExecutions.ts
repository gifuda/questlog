/**
 * useExecutions — React hook for fetching execution state from the Tauri backend.
 *
 * Subscribes to execution state changes and provides the data to UI components.
 * The execution state comes from the Runtime Store (not .epics.json).
 */

import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useRefreshFallback } from "./useRefreshFallback";

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
  // Budget semantics (S0-C) — retries_remaining is view-only (backend-filled)
  outcome?: string | null;
  budget_used?: number;
  retries_remaining?: number;
  scene_preserved?: boolean;
  scene_recovered_from?: string;
  package_display_no?: number;
}

/** Result of an execution action — S0-A: every click gets feedback. */
export interface ActionResult {
  ok: boolean;
  detail?: string;
  error?: string;
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

    return () => {
      unlisten.then((fn) => fn());
    };
  }, [refresh]);

  // Fallback refresh (poll + window focus) — S5: also covers remote mode
  // where no local watcher events arrive.
  useRefreshFallback(refresh);

  return { executions, loading, refresh };
}

/**
 * Execution actions — call Tauri commands to control execution state.
 * S0-A: failures are surfaced to the caller (never swallowed in a
 * console.warn) so every click renders a success or failure feedback.
 */
export function useExecutionActions() {
  const cancel = useCallback(async (id: string): Promise<ActionResult> => {
    try {
      await invoke("cancel_execution", { id });
      return { ok: true, detail: "cancel requested" };
    } catch (err) {
      return { ok: false, error: String(err) };
    }
  }, []);

  // Returns the new child execution_id (Evidence-aware Retry).
  const retry = useCallback(async (id: string): Promise<ActionResult & { childId?: string }> => {
    try {
      const childId = await invoke<string>("retry_execution", { id });
      return { ok: true, detail: `retry scheduled (${childId.slice(0, 12)}…)`, childId };
    } catch (err) {
      return { ok: false, error: String(err) };
    }
  }, []);

  const pause = useCallback(async (id: string): Promise<ActionResult> => {
    try {
      await invoke("pause_execution", { id });
      return { ok: true, detail: "pause requested" };
    } catch (err) {
      return { ok: false, error: String(err) };
    }
  }, []);

  return { cancel, retry, pause };
}

// ─── Runner runtime heartbeat ───

export interface RunnerRuntimeStatus {
  running: boolean;
  thread_alive: boolean;
  last_poll_at: number | null;
  poll_count: number;
  last_processed: number;
  poll_interval_secs: number;
  ready_packages: number;
  /** sealed but not enqueued — claimable once project + epic switches are ON */
  sealed_packages: number;
  active_executions: number;
}

/**
 * useRunnerStatus — poll the Runner heartbeat so the UI can show whether the
 * system is actually polling ("运转中 · Xs 前") vs switched on but not running
 * ("未运转") vs stale ("疑似停滞"). The Runner beats its heartbeat on EVERY
 * poll — even an empty one — so a fresh last_poll_at is proof of liveness even
 * when there is nothing to do.
 */
export function useRunnerStatus() {
  const [status, setStatus] = useState<RunnerRuntimeStatus | null>(null);
  const [now, setNow] = useState(Date.now());

  const refresh = useCallback(async () => {
    try {
      const data = await invoke<RunnerRuntimeStatus>("get_runner_runtime_status");
      setStatus(data);
    } catch {
      setStatus(null);
    }
  }, []);

  useEffect(() => {
    refresh();
    const interval = setInterval(() => {
      refresh();
      setNow(Date.now());
    }, 5000);
    return () => clearInterval(interval);
  }, [refresh]);

  const lastPollSecsAgo = status?.last_poll_at
    ? Math.max(0, Math.floor((now - status.last_poll_at * 1000) / 1000))
    : null;

  return { status, lastPollSecsAgo, refresh };
}

// ─── Per-project Runner control ───

export interface ProjectRunnerStatus {
  project_enabled: boolean;
  /** Master kill switch — when false, nothing runs even with project ON. */
  global_enabled: boolean;
}

// ─── Executor model selection ───

export interface ExecutorModelOption {
  id: string; // "provider/model"
  name: string;
  /** Thinking levels from the user's ZCode config (auto-follows config edits) */
  thinking_levels: string[];
  thinking_default: string | null;
}

export interface ExecutorProviderOption {
  name: string;
  display_name: string;
  models: ExecutorModelOption[];
}

export interface ExecutorOptions {
  providers: ExecutorProviderOption[];
  current_model: string | null;
  current_thinking: string | null;
}

/**
 * useExecutorOptions — which model the Runner's executor uses. Options come
 * from ~/.zcode/cli/config.json providers; the selection is stored in
 * runner-config.json and injected into each worktree as a project-level
 * .zcode/config.json (the CLI has no --model flag).
 */
export function useExecutorOptions() {
  const [options, setOptions] = useState<ExecutorOptions | null>(null);

  const refresh = useCallback(async () => {
    try {
      const data = await invoke<ExecutorOptions>("get_executor_options");
      setOptions(data);
    } catch {
      setOptions(null);
    }
  }, []);

  useEffect(() => {
    refresh();
  }, [refresh]);

  const select = useCallback(
    async (model: string | null) => {
      await invoke("set_executor_model", { model });
      await refresh();
    },
    [refresh],
  );

  const selectThinking = useCallback(
    async (thinking: string | null) => {
      await invoke("set_executor_thinking", { thinking });
      await refresh();
    },
    [refresh],
  );

  return { options, select, selectThinking, refresh };
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

  const toggleGlobal = useCallback(async () => {
    if (!status) return;
    try {
      setLoading(true);
      const next = !status.global_enabled;
      const globalEnabled = await invoke<boolean>("set_runner_global_enabled", { enabled: next });
      setStatus({ ...status, global_enabled: globalEnabled });
    } catch (err) {
      console.warn("Failed to toggle global runner:", err);
    } finally {
      setLoading(false);
    }
  }, [status]);

  return { status, loading, toggle, toggleGlobal, refresh };
}

/**
 * Get the auto_execute setting for a specific epic.
 * Returns null = NOT authorized (fail-closed: an explicit ▶ is required —
 * the project switch alone never authorizes a sealed package),
 * true/false = explicit setting.
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
  }, [refresh]);

  // S5: fallback refresh (poll + window focus) keeps the inbox fresh in
  // remote mode (no local decision-store watcher events).
  useRefreshFallback(refresh);

  const resolve = useCallback(async (requestId: string, resolution: string) => {
    try {
      await invoke("resolve_decision_request", { requestId, resolution });
      setError(null);
    } catch (err) {
      // The decision is recorded server-side, but the mapped action failed
      // (e.g. attempt budget exhausted) — surface it instead of a silent log.
      setError(String(err));
    }
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

/** What the Runner is working on right now (S7-followup banner). */
export interface CurrentExecution {
  execution_id: string;
  package_id: string;
  package_display_no: number | null;
  epic_title: string;
  step_title: string;
  status: string;
  attempt_no: number;
  scene_recovered_from: string | null;
  updated_at: number;
}

export function useCurrentExecution(pollMs = 5000) {
  const [current, setCurrent] = useState<CurrentExecution | null>(null);
  useEffect(() => {
    let alive = true;
    const tick = async () => {
      try {
        const data = await invoke<CurrentExecution | null>("get_current_execution");
        if (alive) setCurrent(data);
      } catch {
        /* banner is best-effort */
      }
    };
    tick();
    const id = setInterval(tick, pollMs);
    return () => {
      alive = false;
      clearInterval(id);
    };
  }, [pollMs]);
  return current;
}
