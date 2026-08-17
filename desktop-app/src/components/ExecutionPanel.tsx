/**
 * ExecutionPanel — displays execution state for an Epic/Step.
 *
 * Shows:
 * - TaskPackage summary (objective, hash, executor)
 * - Run timeline (attempts, status, timestamps, retry lineage via parent_attempt_id)
 * - Git evidence (base SHA, candidate SHA, worktree, branch)
 * - DecisionRequest inbox (blocked questions needing human input)
 * - Action buttons (pause, cancel, retry)
 *
 * This component reads from the Runtime Store via useExecutions.
 * It NEVER bypasses server-side validation — all actions go through Tauri commands.
 */

import { useState, useMemo } from "react";
import { useTranslation } from "react-i18next";
import { useExecutions, useExecutionActions, useDecisionRequests, useCurrentExecution, type Execution, type ExecutionStatus } from "../hooks/useExecutions";

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

function ExecutionRow({ exec, depth = 0 }: { exec: Execution; depth?: number }) {
  const { t } = useTranslation();
  const { cancel, retry, pause } = useExecutionActions();
  const [expanded, setExpanded] = useState(false);
  // S0-A: per-row action feedback — every click renders success or failure.
  const [feedback, setFeedback] = useState<{ ok: boolean; text: string } | null>(null);

  const runAction = async (action: () => Promise<{ ok: boolean; detail?: string; error?: string }>) => {
    const result = await action();
    setFeedback(
      result.ok
        ? { ok: true, text: result.detail ?? "ok" }
        : { ok: false, text: result.error ?? "action failed" },
    );
  };

  const statusKey = `execution.status.${exec.status}`;
  // Retry attempts carry a parent_attempt_id pointing at the previous attempt in the chain.
  const parentAttemptId = exec.parent_attempt_id;
  const isRetry = Boolean(parentAttemptId);
  // S0-C: budget view — undefined on legacy rows / when the package is gone.
  const retriesRemaining = exec.retries_remaining;
  const budgetExhausted = retriesRemaining !== undefined && retriesRemaining <= 0;

  return (
    <div
      className={`border rounded-lg p-3 mb-2 bg-white shadow-sm${isRetry ? " border-l-4 border-l-accent" : ""}`}
      style={{ marginLeft: depth > 0 ? depth * 16 : 0 }}
    >
      <div className="flex items-center justify-between cursor-pointer" onClick={() => setExpanded(!expanded)}>
        <div className="flex items-center gap-2 flex-wrap">
          <span className={`text-xs px-2 py-0.5 rounded-full font-medium ${STATUS_COLORS[exec.status]}`}>
            {t(statusKey, exec.status)}
          </span>
          {/* Attempt number — prominent lineage marker */}
          <span className="text-xs font-semibold text-muted">
            {isRetry ? (
              <>
                Attempt {exec.attempt_no} <span className="font-normal text-faint">(retry)</span>
              </>
            ) : (
              <>Attempt {exec.attempt_no}</>
            )}
          </span>
          {exec.executor && (
            <span className="text-[10px] px-1.5 py-0.5 rounded bg-gray-100 text-gray-600 border border-gray-200">
              {exec.executor}
            </span>
          )}
          {exec.package_display_no !== undefined && (
            <span className="text-[10px] font-mono px-1.5 py-0.5 rounded bg-blue-100 text-blue-700 border border-blue-200">
              #{exec.package_display_no}
            </span>
          )}
          {exec.scene_recovered_from && (
            <span
              title={`${t("execution.recoveredFrom", "成果已恢复自")} ${exec.scene_recovered_from.slice(0, 16)}…`}
              className="text-[10px] px-1.5 py-0.5 rounded bg-amber-100 text-amber-700 border border-amber-200"
            >
              ⏫ {t("execution.sceneRecovered", "断点恢复")}
            </span>
          )}
          <span className="text-xs text-gray-500">
            #{exec.attempts} · {formatTimestamp(exec.updated_at)}
          </span>
          {/* Lineage badge — how this attempt links into the retry chain */}
          {parentAttemptId ? (
            <span className="text-[10px] text-muted" title={`parent: ${parentAttemptId}`}>
              ↳ retry #{exec.attempt_no} from {parentAttemptId.slice(0, 8)}
            </span>
          ) : (
            <span className="text-[10px] text-muted">attempt #{exec.attempt_no}</span>
          )}
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
          {exec.executor && (
            <div>
              <span className="font-medium">{t("execution.executor")}:</span> {exec.executor}
            </div>
          )}
          {exec.last_error && (
            <div className="text-red-600">
              <span className="font-medium">{t("execution.lastError")}:</span> {exec.last_error}
            </div>
          )}
          {retriesRemaining !== undefined && (
            <div className={budgetExhausted ? "text-orange-600" : "text-gray-500"}>
              <span className="font-medium">{t("execution.retryBudget", "Retry budget")}:</span>{" "}
              {budgetExhausted
                ? t("execution.budgetExhausted", "exhausted — revise the sealed package to retry")
                : t("execution.retriesLeft", "{{count}} auto-retries left", { count: retriesRemaining })}
            </div>
          )}
          {feedback && (
            <div className={`break-words ${feedback.ok ? "text-green-600" : "text-red-600"}`}>
              {feedback.ok ? "✓ " : "✗ "}
              {feedback.text}
            </div>
          )}
          <div className="flex gap-2 mt-2">
            {exec.status === "running" && (
              <button
                onClick={() => runAction(() => pause(exec.id))}
                className="px-2 py-1 text-xs rounded bg-orange-100 text-orange-700 hover:bg-orange-200"
              >
                {t("execution.pause")}
              </button>
            )}
            {(exec.status === "failed" || exec.status === "cancelled" || exec.status === "stale" || exec.status === "blocked" || exec.status === "timed_out") && (
              <button
                disabled={budgetExhausted}
                title={
                  budgetExhausted
                    ? t(
                        "execution.budgetExhaustedHint",
                        "Auto-retry budget exhausted — revise the sealed package (prepare → validate → seal) to continue",
                      )
                    : undefined
                }
                onClick={() => runAction(() => retry(exec.id))}
                className="px-2 py-1 text-xs rounded bg-blue-100 text-blue-700 hover:bg-blue-200 disabled:opacity-40 disabled:cursor-not-allowed"
              >
                {t("execution.retry")}
              </button>
            )}
            {!(["cancelled", "passed", "blocked", "failed", "timed_out", "cancel_requested", "pause_requested"] as ExecutionStatus[]).includes(exec.status) && (
              <button
                onClick={() => runAction(() => cancel(exec.id))}
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

/** Chinese labels for the fixed decision actions (original English stays
 *  available on hover and is what actually gets resolved). */
const OPTION_LABELS: Record<string, string> = {
  retry: "重试",
  resume: "续跑",
  cancel: "取消",
  revise_scope: "修订范围并继续",
  revise_package: "修订封存包",
  transfer_manual: "转人工",
  manual: "转人工",
  reject: "拒绝",
};

/** Localized question by kind + option signature; the English original is
 *  preserved in the tooltip so nothing is lost. */
function localizedQuestion(
  t: (k: string, opts?: Record<string, unknown>) => string,
  kind: string,
  options: string[],
): string {
  const has = (o: string) => options.includes(o);
  if (kind === "retry_exhausted") return t("drq.retryExhausted");
  if (kind === "git_drift") return t("drq.gitDrift");
  if (kind === "verifier_failed") return t("drq.verifierFailed");
  if (kind === "approval") return t("drq.approval");
  if (kind === "adjudication") return t("drq.adjudication");
  if (kind === "blocked" && has("revise_scope")) return t("drq.scopePath");
  if (kind === "blocked" && has("revise_package")) return t("drq.scopeForbiddenOrCount");
  return "";
}

export function DecisionInbox() {
  const { t } = useTranslation();
  const { requests, loading, error, resolve } = useDecisionRequests();
  const [custom, setCustom] = useState<Record<string, string>>({});
  const [submitting, setSubmitting] = useState<string | null>(null);

  if (loading) return null;
  if (requests.length === 0 && !error) return null;

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
      {/* 动作失败内联提示：不替换整个 inbox，其余待决项保持可见 */}
      {error && (
        <div className="mb-1.5 px-2 py-1 rounded text-[11px] text-red-300 bg-red-500/10 border border-red-500/30 break-words">
          {error}
        </div>
      )}
      <div className="text-[11px] font-semibold text-orange-300 mb-1">
        🔔 {t("execution.attention", { count: requests.length })}
      </div>
      <div className="space-y-2">
        {requests.map((request) => (
          <div key={request.request_id} className="rounded border border-orange-500/20 p-2 text-[10px]">
            <div className="text-body font-medium" title={request.question}>
              {localizedQuestion(t, request.kind, request.options) || request.question}
            </div>
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
                  title={option}
                  onClick={() => answer(request.request_id, option)}
                  className="px-2 py-0.5 rounded bg-orange-500/15 text-orange-300 hover:bg-orange-500/25 disabled:opacity-40"
                >
                  {OPTION_LABELS[option] ?? option}
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

  // Group executions into retry chains: all attempts of one package form a chain,
  // ordered root-first by following parent_attempt_id links.
  const chains = useMemo(() => {
    const byPackage = new Map<string, Execution[]>();
    for (const exec of filtered) {
      const list = byPackage.get(exec.package_id) ?? [];
      list.push(exec);
      byPackage.set(exec.package_id, list);
    }

    const result: Execution[][] = [];
    for (const execs of byPackage.values()) {
      const byId = new Map(execs.map((e) => [e.id, e]));
      const childrenOf = new Map<string, Execution[]>();
      const roots: Execution[] = [];

      for (const exec of execs) {
        const parent = exec.parent_attempt_id ? byId.get(exec.parent_attempt_id) : undefined;
        if (parent) {
          const siblings = childrenOf.get(parent.id) ?? [];
          siblings.push(exec);
          childrenOf.set(parent.id, siblings);
        } else {
          roots.push(exec);
        }
      }

      const byChain = (a: Execution, b: Execution) =>
        a.attempt_no - b.attempt_no || a.created_at - b.created_at;
      for (const siblings of childrenOf.values()) siblings.sort(byChain);
      roots.sort(byChain);

      const attempts: Execution[] = [];
      const visit = (exec: Execution) => {
        attempts.push(exec);
        for (const child of childrenOf.get(exec.id) ?? []) visit(child);
      };
      for (const root of roots) visit(root);

      result.push(attempts);
    }
    return result;
  }, [filtered]);

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
      {chains.map((attempts) => (
        <div key={attempts[0].id}>
          {attempts.map((exec, depth) => (
            <ExecutionRow key={exec.id} exec={exec} depth={depth} />
          ))}
        </div>
      ))}
    </div>
  );
}

/** Top banner: what the Runner is executing right now. */
export function RunningBanner() {
  const { t } = useTranslation();
  const current = useCurrentExecution();
  if (!current) return null;
  return (
    <div className="px-4 py-1 border-b border-blue-500/30 bg-blue-500/5 flex items-center gap-2 text-[11px]">
      <span className="animate-pulse">⚙</span>
      <span className="font-medium text-blue-400">{t("execution.runningNow", "运行中")}</span>
      {current.package_display_no !== null && (
        <span className="font-mono px-1 rounded bg-blue-500/15 text-blue-300">
          #{current.package_display_no}
        </span>
      )}
      <span className="truncate">{current.step_title}</span>
      <span className="text-faint">· {t(`execution.status.${current.status}`, current.status)}</span>
      <span className="text-faint">· attempt {current.attempt_no}</span>
      {current.scene_recovered_from && (
        <span className="px-1 rounded bg-amber-500/15 text-amber-300">
          ⏫ {t("execution.sceneRecovered", "断点恢复")}
        </span>
      )}
    </div>
  );
}
