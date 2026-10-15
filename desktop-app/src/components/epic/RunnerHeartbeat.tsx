import { useTranslation } from "react-i18next";
import { useRunnerStatus } from "../../hooks/useExecutions";

// RunnerHeartbeat — Runner 运行状态指示灯
// ══════════════════════════════════════════════
export function RunnerHeartbeat({ runnerStatus, lastPollSecsAgo }: {
  runnerStatus: ReturnType<typeof useRunnerStatus>["status"];
  lastPollSecsAgo: number | null;
}) {
  const { t } = useTranslation();
  const base = "flex items-center gap-1.5 text-[11px] font-mono font-medium shrink-0";
  const hint = t("execution.runnerStatusHint");

  if (!runnerStatus?.running) {
    return (
      <span className={`${base} text-faint`} title={hint}>
        <span className="w-1.5 h-1.5 rounded-full bg-line-hover" />
        {t("execution.runnerStatusOff")}
      </span>
    );
  }
  // In-flight executions are the STRONGEST liveness signal — check them
  // BEFORE the heartbeat staleness gate. The Runner thread executes packages
  // synchronously, so the heartbeat stops beating for the whole execution
  // (potentially an hour); a stale heartbeat with active executions is
  // healthy work, not a stall.
  if (runnerStatus.active_executions > 0) {
    return (
      <span className={`${base} text-accent`} title={hint}>
        <span className="w-1.5 h-1.5 rounded-full bg-accent animate-pulse" />
        {t("execution.runnerStatusActive", { count: runnerStatus.active_executions })}
      </span>
    );
  }
  if (!runnerStatus.thread_alive || (lastPollSecsAgo ?? 999) >= 30) {
    return (
      <span className={`${base} text-amber`} title={hint}>
        <span className="w-1.5 h-1.5 rounded-full bg-amber" />
        {t("execution.runnerStatusStale", { secs: lastPollSecsAgo ?? "—" })}
      </span>
    );
  }
  if (runnerStatus.ready_packages > 0) {
    return (
      <span className={`${base} text-amber`} title={hint}>
        <span className="w-1.5 h-1.5 rounded-full bg-amber" />
        {t("execution.runnerStatusQueued", { count: runnerStatus.ready_packages })}
      </span>
    );
  }
  if (runnerStatus.sealed_packages > 0) {
    return (
      <span className={`${base} text-amber/70`} title={t("execution.runnerStatusSealedHint")}>
        <span className="w-1.5 h-1.5 rounded-full bg-amber/70" />
        {t("execution.runnerStatusSealed", { count: runnerStatus.sealed_packages })}
      </span>
    );
  }
  return (
    <span className={`${base} text-success`} title={hint}>
      <span className="w-1.5 h-1.5 rounded-full bg-success animate-pulse" />
      {t("execution.runnerStatusIdle", { secs: lastPollSecsAgo ?? "—" })}
    </span>
  );
}

// ══════════════════════════════════════════════
