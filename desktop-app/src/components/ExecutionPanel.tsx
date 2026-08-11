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
import { useExecutions, useExecutionActions, useDecisionRequests, type Execution, type ExecutionStatus } from "../hooks/useExecutions";

const STATUS_COLORS: Record<ExecutionStatus, string> = {
  queued: "bg-gray-100 text-gray-700",
  running: "bg-blue-100 text-blue-700 animate-pulse",
  candidate: "bg-yellow-100 text-yellow-700",
  verifying: "bg-purple-100 text-purple-700 animate-pulse",
  passed: "bg-green-100 text-green-700",
  failed: "bg-red-100 text-red-700",
  cancelled: "bg-gray-100 text-gray-500",
  cancel_requested: "bg-orange-100 text-orange-700 animate-pulse",
  pause_requested: "bg-orange-100 text-orange-700 animate-pulse",
  timed_out: "bg-red-100 text-red-700",
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
            {(exec.status === "failed" || exec.status === "cancelled" || exec.status === "stale" || exec.status === "blocked" || exec.status === "timed_out") && (
              <button
                onClick={() => retry(exec.id)}
                className="px-2 py-1 text-xs rounded bg-blue-100 text-blue-700 hover:bg-blue-200"
              >
                {t("execution.retry")}
              </button>
            )}
            {!(["cancelled", "passed", "blocked", "failed", "timed_out", "cancel_requested", "pause_requested"] as ExecutionStatus[]).includes(exec.status) && (
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

export function DecisionInbox() {
  const { t } = useTranslation();
  const { requests, loading, error, resolve } = useDecisionRequests();
  const [custom, setCustom] = useState<Record<string, string>>({});
  const [submitting, setSubmitting] = useState<string | null>(null);

  if (loading) return null;
  if (error) {
    return <div className="px-4 py-2 text-[10px] text-red-400 border-b border-line">{error}</div>;
  }
  if (requests.length === 0) return null;

  const answer = async (requestId: string, resolution: string) => {
    if (!resolution.trim()) return;
    setSubmitting(requestId);
    try {
      await resolve(requestId, resolution.trim());
    } finally {
      setSubmitting(null);
    }
  };

  return (
    <section className="px-4 py-2 border-b border-orange-500/30 bg-orange-500/5">
      <div className="text-[11px] font-semibold text-orange-300 mb-1">
        🔔 {t("execution.attention", { count: requests.length })}
      </div>
      <div className="space-y-2">
        {requests.map((request) => (
          <div key={request.request_id} className="rounded border border-orange-500/20 p-2 text-[10px]">
            <div className="text-body font-medium">{request.question}</div>
            <div className="text-faint mt-0.5">
              {request.kind} · rev {request.package_revision} · {request.execution_id.slice(0, 12)}
            </div>
            {request.evidence_refs.length > 0 && (
              <div className="text-faint truncate mt-0.5" title={request.evidence_refs.join("\n")}>
                {t("execution.evidence")}: {request.evidence_refs[0]}
              </div>
            )}
            <div className="flex flex-wrap gap-1 mt-1.5">
              {request.options.map((option) => (
                <button
                  key={option}
                  disabled={submitting === request.request_id}
                  onClick={() => answer(request.request_id, option)}
                  className="px-2 py-0.5 rounded bg-orange-500/15 text-orange-300 hover:bg-orange-500/25 disabled:opacity-40"
                >
                  {option}
                </button>
              ))}
              <input
                value={custom[request.request_id] ?? ""}
                onChange={(event) => setCustom((current) => ({ ...current, [request.request_id]: event.target.value }))}
                placeholder={t("execution.customDecision")}
                className="min-w-36 flex-1 bg-elevated rounded px-1.5 py-0.5 text-body outline-none"
              />
              <button
                disabled={submitting === request.request_id || !(custom[request.request_id] ?? "").trim()}
                onClick={() => answer(request.request_id, custom[request.request_id] ?? "")}
                className="px-2 py-0.5 rounded bg-accent text-white disabled:opacity-40"
              >
                {t("execution.submitDecision")}
              </button>
            </div>
            <div className="text-faint mt-1">{t("execution.decisionRecordedHint")}</div>
          </div>
        ))}
      </div>
    </section>
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
