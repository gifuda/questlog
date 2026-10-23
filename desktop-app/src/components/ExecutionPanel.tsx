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
import { useTaskMaintenance } from "../hooks/useTaskMaintenance";
import { matchTaskMaintenanceLink } from "../taskMaintenanceLogic";
import { ExecutionTaskMaintenance } from "./TaskMaintenance";
import type { TaskMaintenance } from "../types/taskMaintenance";
import { useExecutions, useExecutionActions, useDecisionRequests, useCurrentExecution, type Execution, type ExecutionStatus } from "../hooks/useExecutions";
import Badge, { type BadgeTone } from "./Badge";
import ConfirmButton from "./ConfirmButton";

/** 执行状态 → Badge tone（原 STATUS_COLORS 语义色就近映射）。 */
const STATUS_TONE: Record<ExecutionStatus, { tone: BadgeTone; className?: string }> = {
  queued: { tone: "muted" },
  running: { tone: "accent", className: "animate-pulse" },
  candidate: { tone: "amber" },
  verifying: { tone: "info", className: "animate-pulse" },
  passed: { tone: "success" },
  failed: { tone: "danger" },
  cancelled: { tone: "muted" },
  cancel_requested: { tone: "amber", className: "animate-pulse" },
  pause_requested: { tone: "amber", className: "animate-pulse" },
  timed_out: { tone: "danger" },
  blocked: { tone: "amber" },
  stale: { tone: "muted" },
};

function formatTimestamp(ts: number): string {
  if (!ts) return "—";
  return new Date(ts * 1000).toLocaleTimeString();
}

function formatSha(sha: string | null): string {
  if (!sha) return "—";
  return sha.length > 12 ? `${sha.slice(0, 8)}…` : sha;
}

function ExecutionRow({ exec, depth = 0, maintenance }: { exec: Execution; depth?: number; maintenance: TaskMaintenance | null }) {
  const { t } = useTranslation();
  const { cancel, retry, pause } = useExecutionActions();
  const [expanded, setExpanded] = useState(false);
  // S0-A: per-row action feedback — every click renders success or failure.
  const [feedback, setFeedback] = useState<{ ok: boolean; text: string } | null>(null);
  // 动作防连点：pause/retry/cancel 在 await 期间全部禁用。
  const [acting, setActing] = useState(false);
  // cancel 确认窗（3s）期间同样锁死 pause/retry，避免与 cancel 并发。
  const [cancelPending, setCancelPending] = useState(false);
  const busy = acting || cancelPending;

  const runAction = async (action: () => Promise<{ ok: boolean; detail?: string; error?: string }>) => {
    if (acting) return;
    setActing(true);
    try {
      const result = await action();
      setFeedback(
        result.ok
          ? { ok: true, text: result.detail ?? "ok" }
          : { ok: false, text: result.error ?? "action failed" },
      );
    } finally {
      setActing(false);
    }
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
      className={`border border-line rounded-lg p-3 mb-2 bg-surface${isRetry ? " border-l-4 border-l-accent" : ""}`}
      style={{ marginLeft: depth > 0 ? depth * 16 : 0 }}
    >
      <div className="flex items-center justify-between cursor-pointer" onClick={() => setExpanded(!expanded)}>
        <div className="flex items-center gap-2 flex-wrap">
          <Badge
            tone={STATUS_TONE[exec.status].tone}
            className={`font-medium ${STATUS_TONE[exec.status].className ?? ""}`.trim()}
          >
            {t(statusKey, exec.status)}
          </Badge>
          {/* Attempt number — prominent lineage marker */}
          <span className="text-xs font-semibold text-muted tabular-nums">
            {isRetry ? (
              <>
                {t("execution.attemptNo", { no: exec.attempt_no })}{" "}
                <span className="font-normal text-faint">{t("execution.retryTag")}</span>
              </>
            ) : (
              t("execution.attemptNo", { no: exec.attempt_no })
            )}
          </span>
          {exec.executor && (
            <Badge tone="muted" className="border border-line">
              {exec.executor}
            </Badge>
          )}
          {exec.package_display_no !== undefined && (
            <Badge tone="accent" className="font-mono border border-accent/30">
              #{exec.package_display_no}
            </Badge>
          )}
          {exec.scene_recovered_from && (
            <Badge
              tone="amber"
              className="border border-amber/30"
              title={`${t("execution.recoveredFrom", "成果已恢复自")} ${exec.scene_recovered_from.slice(0, 16)}…`}
            >
              ⏫ {t("execution.sceneRecovered", "断点恢复")}
            </Badge>
          )}
          <span className="text-xs text-muted tabular-nums">
            #{exec.attempts} · {formatTimestamp(exec.updated_at)}
          </span>
          {/* Lineage badge — how this attempt links into the retry chain */}
          {parentAttemptId ? (
            <span className="text-[11px] text-muted tabular-nums" title={`parent: ${parentAttemptId}`}>
              {t("execution.retryLineage", { no: exec.attempt_no, parent: parentAttemptId.slice(0, 8) })}
            </span>
          ) : (
            <span className="text-[11px] text-muted tabular-nums">
              {t("execution.attemptShort", { no: exec.attempt_no })}
            </span>
          )}
        </div>
        <span className="text-xs text-faint">{expanded ? "▼" : "▶"}</span>
      </div>

      <ExecutionTaskMaintenance
        projection={maintenance}
        link={maintenance ? matchTaskMaintenanceLink(maintenance, exec.id, exec.attempt_no) : null}
        covered={Boolean(maintenance?.state === "observed" && maintenance.links.some((link) => link.execution_id === exec.id && link.attempt_no === exec.attempt_no))}
      />

      {expanded && (
        <div className="mt-2 space-y-1 text-xs text-muted">
          <div>
            <span className="font-medium">{t("execution.baseSha")}:</span>{" "}
            <code className="bg-chip px-1 rounded">{formatSha(exec.base_sha)}</code>
          </div>
          {exec.candidate_sha && (
            <div>
              <span className="font-medium">{t("execution.candidateSha")}:</span>{" "}
              <code className="bg-chip px-1 rounded">{formatSha(exec.candidate_sha)}</code>
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
            <div className="text-danger">
              <span className="font-medium">{t("execution.lastError")}:</span> {exec.last_error}
            </div>
          )}
          {retriesRemaining !== undefined && (
            <div className={budgetExhausted ? "text-amber" : "text-muted"}>
              <span className="font-medium">{t("execution.retryBudget", "Retry budget")}:</span>{" "}
              {budgetExhausted
                ? t("execution.budgetExhausted", "exhausted — revise the sealed package to retry")
                : t("execution.retriesLeft", "{{count}} auto-retries left", { count: retriesRemaining })}
            </div>
          )}
          {feedback && (
            <div className={`break-words ${feedback.ok ? "text-success" : "text-danger"}`}>
              {feedback.ok ? "✓ " : "✗ "}
              {feedback.text}
            </div>
          )}
          <div className="flex gap-2 mt-2">
            {exec.status === "running" && (
              <button
                disabled={busy}
                onClick={() => runAction(() => pause(exec.id))}
                className="px-2 py-1 text-xs rounded bg-amber-bg text-amber hover:bg-amber/20 disabled:opacity-40"
              >
                {t("execution.pause")}
              </button>
            )}
            {(exec.status === "failed" || exec.status === "cancelled" || exec.status === "stale" || exec.status === "blocked" || exec.status === "timed_out") && (
              <button
                disabled={busy || budgetExhausted}
                title={
                  budgetExhausted
                    ? t(
                        "execution.budgetExhaustedHint",
                        "Auto-retry budget exhausted — revise the sealed package (prepare → validate → seal) to continue",
                      )
                    : undefined
                }
                onClick={() => runAction(() => retry(exec.id))}
                className="px-2 py-1 text-xs rounded bg-accent-bg text-accent hover:bg-accent/20 disabled:opacity-40 disabled:cursor-not-allowed"
              >
                {t("execution.retry")}
              </button>
            )}
            {!(["cancelled", "passed", "blocked", "failed", "timed_out", "cancel_requested", "pause_requested"] as ExecutionStatus[]).includes(exec.status) && (
              <ConfirmButton
                label={t("execution.cancel")}
                onConfirm={() => runAction(() => cancel(exec.id))}
                onPendingChange={setCancelPending}
                className="px-2 py-1 text-xs rounded bg-elevated text-muted hover:bg-line-hover disabled:opacity-40"
              />
            )}
          </div>
        </div>
      )}
    </div>
  );
}

/** 决策动作标签走 i18n（drq.option.*）；未知 option 回退英文原文。 */

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
    <section className="px-4 py-2 border-b border-amber/30 bg-amber-bg">
      {/* 动作失败内联提示：不替换整个 inbox，其余待决项保持可见 */}
      {error && (
        <div className="mb-1.5 px-2 py-1 rounded text-[11px] text-danger bg-danger/10 border border-danger/30 break-words">
          {error}
        </div>
      )}
      <div className="text-[11px] font-semibold text-amber mb-1">
        🔔 {t("execution.attention", { count: requests.length })}
      </div>
      <div className="space-y-2">
        {requests.map((request) => (
          <div key={request.request_id} className="rounded border border-amber/20 p-2 text-[11px]">
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
                  className="px-2 py-0.5 rounded bg-amber-bg text-amber hover:bg-amber/25 disabled:opacity-40"
                >
                  {t(`drq.option.${option}`, option)}
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
                className="px-2 py-0.5 rounded bg-accent text-on-accent disabled:opacity-40"
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
  const { projection: maintenance } = useTaskMaintenance();

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
    return <div className="text-xs text-faint p-2">{t("execution.title")}…</div>;
  }

  if (filtered.length === 0) {
    return null; // Don't render if there are no executions — keeps the UI clean
  }

  return (
    <div className="mt-2 border-t pt-2">
      <h4 className="text-xs font-semibold text-muted uppercase tracking-wide mb-1">
        {t("execution.runs")} ({filtered.length})
      </h4>
      {chains.map((attempts) => (
        <div key={attempts[0].id}>
          {attempts.map((exec, depth) => (
            <ExecutionRow key={exec.id} exec={exec} depth={depth} maintenance={maintenance} />
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
    <div className="px-4 py-1 border-b border-accent/30 bg-accent-bg flex items-center gap-2 text-[11px]">
      <span className="animate-pulse">⚙</span>
      <span className="font-medium text-accent">{t("execution.runningNow", "运行中")}</span>
      {current.package_display_no !== null && (
        <span className="font-mono px-1 rounded bg-surface-dim text-accent">
          #{current.package_display_no}
        </span>
      )}
      <span className="truncate">{current.step_title}</span>
      <span className="text-faint">· {t(`execution.status.${current.status}`, current.status)}</span>
      <span className="text-faint">· {t("execution.attemptShort", { no: current.attempt_no })}</span>
      {current.scene_recovered_from && (
        <span className="px-1 rounded bg-amber-bg text-amber">
          ⏫ {t("execution.sceneRecovered", "断点恢复")}
        </span>
      )}
    </div>
  );
}
