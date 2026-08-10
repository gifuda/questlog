/**
 * ExecutionPanel — displays execution state for an Epic/Step.
 *
 * Shows:
 * - TaskPackage summary (objective, hash, executor)
 * - Run timeline (attempts, status, timestamps)
 * - Git evidence (base SHA, candidate SHA, worktree, branch)
 * - DecisionRequest inbox (blocked questions needing human input)
 * - Action buttons (pause, cancel, retry)
 *
 * This component reads from the Runtime Store via useExecutions.
 * It NEVER bypasses server-side validation — all actions go through Tauri commands.
 */

import { useState, useMemo } from "react";
import { useTranslation } from "react-i18next";
import { useExecutions, useExecutionActions, type Execution, type ExecutionStatus } from "../hooks/useExecutions";

const STATUS_COLORS: Record<ExecutionStatus, string> = {
  queued: "bg-gray-100 text-gray-700",
  running: "bg-blue-100 text-blue-700 animate-pulse",
  candidate: "bg-yellow-100 text-yellow-700",
  passed: "bg-green-100 text-green-700",
  failed: "bg-red-100 text-red-700",
  cancelled: "bg-gray-100 text-gray-500",
  blocked: "bg-orange-100 text-orange-700",
  stale: "bg-gray-200 text-gray-500",
};

function formatTimestamp(ts: number): string {
  if (!ts) return "—";
  return new Date(ts * 1000).toLocaleTimeString();
}

function formatSha(sha: string | null): string {
  if (!sha) return "—";
  return sha.length > 12 ? `${sha.slice(0, 8)}…` : sha;
}

function ExecutionRow({ exec }: { exec: Execution }) {
  const { t } = useTranslation();
  const { cancel, retry, pause } = useExecutionActions();
  const [expanded, setExpanded] = useState(false);

  const statusKey = `execution.status.${exec.status}`;

  return (
    <div className="border rounded-lg p-3 mb-2 bg-white shadow-sm">
      <div className="flex items-center justify-between cursor-pointer" onClick={() => setExpanded(!expanded)}>
        <div className="flex items-center gap-2">
          <span className={`text-xs px-2 py-0.5 rounded-full font-medium ${STATUS_COLORS[exec.status]}`}>
            {t(statusKey, exec.status)}
          </span>
          <span className="text-xs text-gray-500">
            #{exec.attempts} · {formatTimestamp(exec.updated_at)}
          </span>
        </div>
        <span className="text-xs text-gray-400">{expanded ? "▼" : "▶"}</span>
      </div>

      {expanded && (
        <div className="mt-2 space-y-1 text-xs text-gray-600">
          <div>
            <span className="font-medium">{t("execution.baseSha")}:</span>{" "}
            <code className="bg-gray-100 px-1 rounded">{formatSha(exec.base_sha)}</code>
          </div>
          {exec.candidate_sha && (
            <div>
              <span className="font-medium">{t("execution.candidateSha")}:</span>{" "}
              <code className="bg-gray-100 px-1 rounded">{formatSha(exec.candidate_sha)}</code>
            </div>
          )}
          {exec.branch && (
            <div>
              <span className="font-medium">{t("execution.branch")}:</span> {exec.branch}
            </div>
          )}
          {exec.last_error && (
            <div className="text-red-600">
              <span className="font-medium">{t("execution.lastError")}:</span> {exec.last_error}
            </div>
          )}
          <div className="flex gap-2 mt-2">
            {exec.status === "running" && (
              <button
                onClick={() => pause(exec.id)}
                className="px-2 py-1 text-xs rounded bg-orange-100 text-orange-700 hover:bg-orange-200"
              >
                {t("execution.pause")}
              </button>
            )}
            {(exec.status === "failed" || exec.status === "cancelled" || exec.status === "stale") && (
              <button
                onClick={() => retry(exec.id)}
                className="px-2 py-1 text-xs rounded bg-blue-100 text-blue-700 hover:bg-blue-200"
              >
                {t("execution.retry")}
              </button>
            )}
            {exec.status !== "cancelled" && exec.status !== "passed" && (
              <button
                onClick={() => cancel(exec.id)}
                className="px-2 py-1 text-xs rounded bg-gray-100 text-gray-600 hover:bg-gray-200"
              >
                {t("execution.cancel")}
              </button>
            )}
          </div>
        </div>
      )}
    </div>
  );
}

export default function ExecutionPanel({ stepId }: { stepId?: string }) {
  const { t } = useTranslation();
  const { executions, loading } = useExecutions();

  const filtered = useMemo(() => {
    if (!stepId) return executions;
    return executions.filter((e) => e.step_id === stepId);
  }, [executions, stepId]);

  if (loading) {
    return <div className="text-xs text-gray-400 p-2">{t("execution.title")}…</div>;
  }

  if (filtered.length === 0) {
    return null; // Don't render if there are no executions — keeps the UI clean
  }

  return (
    <div className="mt-2 border-t pt-2">
      <h4 className="text-xs font-semibold text-gray-500 uppercase tracking-wide mb-1">
        {t("execution.runs")} ({filtered.length})
      </h4>
      {filtered.map((exec) => (
        <ExecutionRow key={exec.id} exec={exec} />
      ))}
    </div>
  );
}
