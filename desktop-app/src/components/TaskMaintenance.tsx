import { useTranslation } from "react-i18next";
import type { TaskMaintenance, TaskMaintenanceLink } from "../types/taskMaintenance";
import { taskMaintenanceStatusLabel } from "../taskMaintenanceLogic";

function timeLabel(value: string): string {
  return new Date(value).toLocaleString("zh-CN", { hour12: false });
}

function observed(projection: TaskMaintenance | null): projection is Extract<TaskMaintenance, { state: "observed" }> {
  return projection?.state === "observed";
}

export function InfraTaskMaintenanceCard({ projection, loading }: { projection: TaskMaintenance | null; loading: boolean }) {
  const { t } = useTranslation();
  return (
    <section className="rounded-lg border border-line bg-surface-dim p-3" aria-label="任务与系统维护">
      <div className="flex items-center gap-2 mb-1.5">
        <h2 className="text-[13px] font-medium text-heading">{t("infra.maintenanceTitle")}</h2>
        <span className="text-[11px] text-faint">{t("infra.maintenanceReadonly")}</span>
        <span className="ml-auto text-[11px] text-faint tabular-nums">
          {observed(projection) ? t("infra.maintenanceCaptured", { time: timeLabel(projection.observed_at) }) : t("infra.maintenanceUnknownCaptured")}
        </span>
      </div>
      {loading && !projection ? <div className="text-[11px] text-faint">{t("infra.maintenanceLoading")}</div>
        : !observed(projection) ? <div className="text-[11px] text-amber">{t("infra.maintenanceUnknown")}</div>
          : <>
            <div className="flex flex-wrap gap-x-4 gap-y-1 text-[11px] text-body tabular-nums">
              <span>{t("infra.maintenanceExecutions", { count: projection.summary.execution_count })}</span>
              <span>{t("infra.maintenanceLinks", { count: projection.summary.linked_count })}</span>
              <span>{t("infra.maintenanceCandidates", { count: projection.summary.candidate_count })}</span>
              <span>{t("infra.maintenanceCoverage", { shown: projection.coverage.projected_links, total: projection.coverage.total_links })}</span>
            </div>
            <div className="text-[11px] text-faint mt-1">
              {t("infra.maintenanceSource", { time: timeLabel(projection.source_as_of) })}
            </div>
          </>}
    </section>
  );
}

export function MorningTaskMaintenance({
  projection,
  loading,
  onGoto,
}: {
  projection: TaskMaintenance | null;
  loading: boolean;
  onGoto: () => void;
}) {
  const { t } = useTranslation();
  const hasCandidates = observed(projection) && projection.summary.candidate_count > 0;
  return (
    <section className="rounded-lg border border-line bg-surface px-3.5 py-2.5">
      <div className="flex items-center gap-2">
        <h2 className="text-[13px] font-semibold text-heading">{t("morning.maintenanceTitle")}</h2>
        <span className="text-[11px] text-faint">{t("morning.maintenanceReadonly")}</span>
        <button
          type="button"
          disabled={!hasCandidates}
          onClick={onGoto}
          className="ml-auto text-[11px] text-accent hover:underline disabled:text-faint disabled:no-underline"
        >{t("morning.maintenanceGoto")}</button>
      </div>
      {loading && !projection ? <p className="mt-1 text-[11px] text-faint">{t("morning.maintenanceLoading")}</p>
        : !observed(projection) ? <p className="mt-1 text-[11px] text-amber">{t("morning.maintenanceUnknown")}</p>
          : <div className="mt-1 text-[11px] text-body">
            {hasCandidates
              ? t("morning.maintenanceCandidates", { count: projection.summary.candidate_count })
              : t("morning.maintenanceNoCandidates")}
            {projection.summary.linked_count > 0 && ` ${t("morning.maintenanceLinked", { count: projection.summary.linked_count })}`}
            {projection.coverage.truncated_links > 0 && ` ${t("morning.maintenanceCoverage", { shown: projection.coverage.projected_links, total: projection.coverage.total_links })}`}
            <span className="text-faint"> {t("morning.maintenanceClosed")}</span>
          </div>}
    </section>
  );
}

export function ExecutionTaskMaintenance({
  projection,
  link,
  covered,
}: {
  projection: TaskMaintenance | null;
  link: TaskMaintenanceLink | null;
  covered: boolean;
}) {
  const { t } = useTranslation();
  if (!projection || projection.state !== "observed") {
    return <div className="mt-1 text-[11px] text-amber">{t("execution.maintenanceUnknown")}</div>;
  }
  if (!link || !covered) {
    return <div className="mt-1 text-[11px] text-faint">{t("execution.maintenanceNotCovered")}</div>;
  }
  return (
    <div className="mt-1 space-y-0.5 text-[11px] text-muted">
      <div>{t("execution.maintenanceStatus", { status: taskMaintenanceStatusLabel(link.status) })}{link.case_id ? ` · ${t("execution.maintenanceCase", { id: link.case_id })}` : ""}</div>
      {link.decision_request_id && <div>{t("execution.maintenanceDecision")}: <code>{link.decision_request_id}</code></div>}
      {link.recovery_candidate && <div className="text-amber">{t("execution.maintenanceCandidate")}</div>}
    </div>
  );
}
