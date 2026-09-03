import { useMemo, useState, useEffect, Fragment } from "react";
import { useTranslation } from "react-i18next";
import { useEpics } from "../hooks/useEpics";
import { useProjectRunner, useRunnerStatus, getEpicAutoExecute, setEpicAutoExecute } from "../hooks/useExecutions";
import { epicStatusBadge, stageOrder, statusOptions, statusRank } from "../epicConstants";
import { formatDate } from "../utils";
import type { Epic, EpicStatus, StepStage } from "../types/epics";
import ConfirmButton from "./ConfirmButton";
import ExecutionPanel, { DecisionInbox, RunningBanner } from "./ExecutionPanel";
import ExecutorModelSelector from "./ExecutorModelSelector";

/** i18n keys for epic status labels (statuses stay lowercase in data). */
const epicStatusKey: Record<EpicStatus, string> = {
  draft: "epic.statusDraft",
  active: "epic.statusActive",
  verifying: "epic.statusVerifying",
  observing: "epic.statusObserving",
  done: "epic.statusDone",
  blocked: "epic.statusBlocked",
  paused: "epic.statusPaused",
};

/** i18n keys for step stage labels. */
const stepStageKey: Record<StepStage, string> = {
  pending: "epic.stagePending",
  designed: "epic.stageDesigned",
  coded: "epic.stageCoded",
  verified: "epic.stageVerified",
  observed: "epic.stageObserved",
};

/** 总表四列的栅格模板（表头与行共用，保证对齐）。 */
const GRID =
  "grid grid-cols-[minmax(240px,2.2fr)_110px_150px_minmax(200px,1.8fr)] items-center gap-3 px-4";

/** 在办步骤 = 处于中段（已设计/已实施/已验证）的步骤。 */
const IN_PROGRESS_STAGES: StepStage[] = ["designed", "coded", "verified"];

/** 排序：按方案自身状态权重（done 严格沉底，与状态徽章一致），同级按创建时间倒序。
 *  子方案跟随父方案渲染，不参与顶级排序；父方案 done 但子方案未完属数据不一致，
 *  徽章与位置保持一致比掩盖更重要。 */
function sortEpics(list: Epic[]): Epic[] {
  return [...list].sort((a, b) => {
    const r = statusRank(a.status) - statusRank(b.status);
    if (r !== 0) return r;
    return (b.created_at || "").localeCompare(a.created_at || "");
  });
}

interface EpicBoardProps {
  projectPath: string;
}

export default function EpicBoard({ projectPath }: EpicBoardProps) {
  const { t, i18n } = useTranslation();
  const { data, summary, epics, addEpic, updateEpic, deleteEpic, addStep, updateStep, deleteStep } = useEpics(projectPath);
  const { status: projectStatus, loading: projectLoading, toggle: toggleProjectRunner, toggleGlobal } = useProjectRunner(projectPath);
  const { status: runnerStatus, lastPollSecsAgo } = useRunnerStatus();
  const [showNewEpic, setShowNewEpic] = useState(false);
  const [showArchived, setShowArchived] = useState(false);
  const lang = i18n.resolvedLanguage ?? "zh";

  const sorted = sortEpics(epics);
  // 自动归档：done 的方案沉到归档折叠区，主列表只留在办方案。
  // 归档是展示层派生行为——用户把状态改回非 done 即自动回到主列表。
  const activeEpics = sorted.filter((e) => e.status !== "done");
  const archivedEpics = sorted.filter((e) => e.status === "done");

  return (
    <div className="flex flex-col h-full">
      {/* 摘要条 — 大号等宽数字 + Runner 心跳，3 秒扫读 */}
      {summary && summary.total > 0 && (
        <div className="px-4 py-3 border-b border-line bg-surface/40 flex items-center gap-6 flex-wrap">
          <Metric num={summary.total} label={t("epic.sumTotal")} />
          <Metric num={summary.active} label={t("epic.sumActive")} numClass="text-accent" />
          <Metric num={summary.observing} label={t("epic.sumObserving")} numClass="text-amber" />
          <Metric num={summary.blocked} label={t("epic.sumBlocked")} numClass="text-danger" />
          <Metric num={summary.done} label={t("epic.sumDone")} numClass="text-success" />
          <div className="ml-auto">
            <RunnerHeartbeat runnerStatus={runnerStatus} lastPollSecsAgo={lastPollSecsAgo} />
          </div>
        </div>
      )}

      {/* Runner 控制行 — 总闸 + 模型选择 + 项目开关 */}
      <div className="px-4 py-1.5 border-b border-line/50 flex items-center justify-end gap-2">
        <button
          onClick={toggleGlobal}
          disabled={projectLoading || !projectStatus}
          title={
            projectStatus?.global_enabled
              ? t("execution.globalOnTitle")
              : t("execution.globalOffTitle")
          }
          className={`px-2.5 py-1 rounded-full text-[11px] font-medium transition-colors shrink-0 disabled:opacity-40 ${
            projectStatus?.global_enabled
              ? "bg-success/15 text-success hover:bg-success/25"
              : "bg-danger/15 text-danger hover:bg-danger/25"
          }`}
        >
          ⏻ {projectStatus?.global_enabled ? t("execution.globalOn") : t("execution.globalOff")}
        </button>
        <ExecutorModelSelector />
        <button
          onClick={toggleProjectRunner}
          disabled={projectLoading}
          title={
            projectStatus?.project_enabled && !projectStatus?.global_enabled
              ? t("execution.projectBlockedByGlobal")
              : projectStatus?.project_enabled
                ? t("execution.projectRunnerEffective")
                : t("execution.projectRunnerOn")
          }
          className={`px-2.5 py-1 rounded-full text-[11px] font-medium transition-colors shrink-0 disabled:opacity-40 ${
            projectStatus?.project_enabled
              ? "bg-success/15 text-success hover:bg-success/25"
              : "bg-elevated text-faint hover:bg-line-hover"
          }`}
        >
          📂 {projectStatus?.project_enabled ? t("execution.projectOn") : t("execution.projectOff")}
        </button>
      </div>
      <RunningBanner />
      <DecisionInbox />

      {/* 总表 */}
      <div className="flex-1 overflow-auto">
        {sorted.length === 0 && !showNewEpic ? (
          <div className="flex flex-col items-center justify-center h-full text-faint gap-3">
            <span className="text-3xl">📊</span>
            <span className="text-sm">{t("epic.emptyTitle")}</span>
            <span className="text-xs">{t("epic.emptyDesc")}</span>
            <button
              className="text-[13px] px-3 py-1.5 bg-accent text-white rounded-lg hover:opacity-90"
              onClick={() => setShowNewEpic(true)}
            >
              {t("epic.newPlan")}
            </button>
          </div>
        ) : (
          <div className="min-w-[720px]">
            {/* 表头 */}
            <div className={`${GRID} h-9 border-b border-line font-mono text-[11px] tracking-widest uppercase text-faint sticky top-0 bg-page z-10`}>
              <span>{t("epic.colPlan")}</span>
              <span>{t("epic.colStatus")}</span>
              <span>{t("epic.colProgress")}</span>
              <span>{t("epic.colFocus")}</span>
            </div>

            {activeEpics.map((epic) => (
              <Fragment key={epic.id}>
                <EpicRow
                  epic={epic}
                  depth={0}
                  allEpics={epics}
                  lang={lang}
                  onUpdate={(id, patch) => updateEpic(id, patch)}
                  onDelete={(id) => deleteEpic(id)}
                  onAddStep={(id, title, desc, dependsOn) => addStep(id, title, desc, dependsOn)}
                  onUpdateStep={(stepId, patch) => updateStep(stepId, patch)}
                  onDeleteStep={(stepId) => deleteStep(stepId)}
                  onAddChild={(id, title, opts) => addEpic(title, { ...opts, parentId: id })}
                  projectPath={projectPath}
                  globalEnabled={projectStatus?.global_enabled}
                  projectEnabled={projectStatus?.project_enabled}
                />
                {sortEpics(epic.children ?? []).map((child) => (
                  <EpicRow
                    key={child.id}
                    epic={child}
                    depth={1}
                    allEpics={epics}
                    lang={lang}
                    onUpdate={(id, patch) => updateEpic(id, patch)}
                    onDelete={(id) => deleteEpic(id)}
                    onAddStep={(id, title, desc, dependsOn) => addStep(id, title, desc, dependsOn)}
                    onUpdateStep={(stepId, patch) => updateStep(stepId, patch)}
                    onDeleteStep={(stepId) => deleteStep(stepId)}
                    projectPath={projectPath}
                    globalEnabled={projectStatus?.global_enabled}
                    projectEnabled={projectStatus?.project_enabled}
                  />
                ))}
              </Fragment>
            ))}

            {/* 已归档区 — done 方案自动沉到这里，可展开查看/操作 */}
            {archivedEpics.length > 0 && (
              <>
                <button
                  className={`${GRID} w-full h-9 border-b border-line/60 text-left text-[11px] text-faint hover:text-muted hover:bg-surface/40 transition-colors`}
                  onClick={() => setShowArchived(!showArchived)}
                >
                  <span className="flex items-center gap-1.5">
                    <span className={`inline-block transition-transform ${showArchived ? "rotate-90" : ""}`}>▸</span>
                    🗂 {t("epic.archived")}（{archivedEpics.length}）
                  </span>
                  <span className="col-span-3 text-[10px] text-faint/70">{t("epic.archivedHint")}</span>
                </button>
                {showArchived &&
                  archivedEpics.map((epic) => (
                    <Fragment key={epic.id}>
                      <EpicRow
                        epic={epic}
                        depth={0}
                        allEpics={epics}
                        lang={lang}
                        onUpdate={(id, patch) => updateEpic(id, patch)}
                        onDelete={(id) => deleteEpic(id)}
                        onAddStep={(id, title, desc, dependsOn) => addStep(id, title, desc, dependsOn)}
                        onUpdateStep={(stepId, patch) => updateStep(stepId, patch)}
                        onDeleteStep={(stepId) => deleteStep(stepId)}
                        onAddChild={(id, title, opts) => addEpic(title, { ...opts, parentId: id })}
                        projectPath={projectPath}
                        globalEnabled={projectStatus?.global_enabled}
                        projectEnabled={projectStatus?.project_enabled}
                      />
                      {sortEpics(epic.children ?? []).map((child) => (
                        <EpicRow
                          key={child.id}
                          epic={child}
                          depth={1}
                          allEpics={epics}
                          lang={lang}
                          onUpdate={(id, patch) => updateEpic(id, patch)}
                          onDelete={(id) => deleteEpic(id)}
                          onAddStep={(id, title, desc, dependsOn) => addStep(id, title, desc, dependsOn)}
                          onUpdateStep={(stepId, patch) => updateStep(stepId, patch)}
                          onDeleteStep={(stepId) => deleteStep(stepId)}
                          projectPath={projectPath}
                          globalEnabled={projectStatus?.global_enabled}
                          projectEnabled={projectStatus?.project_enabled}
                        />
                      ))}
                    </Fragment>
                  ))}
              </>
            )}

            {/* 新建方案 */}
            {showNewEpic && (
              <div className="p-3">
                <NewEpicForm
                  existingTitles={epics.map((e) => ({ id: e.id, title: e.title }))}
                  onCancel={() => setShowNewEpic(false)}
                  onSubmit={async (title, opts) => {
                    await addEpic(title, opts);
                    setShowNewEpic(false);
                  }}
                />
              </div>
            )}
          </div>
        )}
      </div>

      {/* Bottom new-plan button */}
      {!showNewEpic && sorted.length > 0 && (
        <div className="px-4 py-2 border-t border-line">
          <button
            className="text-[13px] text-accent hover:underline"
            onClick={() => setShowNewEpic(true)}
          >
            {t("epic.newPlan")}
          </button>
        </div>
      )}
    </div>
  );
}

// ══════════════════════════════════════════════
// Metric — 摘要条的大号等宽数字
// ══════════════════════════════════════════════
function Metric({ num, label, numClass = "text-heading" }: { num: number; label: string; numClass?: string }) {
  return (
    <div className="flex items-baseline gap-1.5">
      <span className={`font-mono text-xl font-semibold leading-none tabular-nums ${numClass}`}>{num}</span>
      <span className="text-xs text-muted">{label}</span>
    </div>
  );
}

// ══════════════════════════════════════════════
// RunnerHeartbeat — Runner 运行状态指示灯
// ══════════════════════════════════════════════
function RunnerHeartbeat({ runnerStatus, lastPollSecsAgo }: {
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
// SignalCell — 焦点列：每行只显示最重要的一个信号，且必须与真实进度一致
// 优先级：堵点 > 在办步骤 > 在办子方案 > 可做步骤 > 收尾语义
// 收尾语义三分：已闭环(done) / 观察期中(observing) / 待收尾(全部完成但未标 done)
// ══════════════════════════════════════════════
function SignalCell({ epic, currentStep, nextStep }: {
  epic: Epic;
  currentStep: Epic["steps"][number] | undefined;
  nextStep: Epic["steps"][number] | undefined;
}) {
  const { t } = useTranslation();
  const children = epic.children ?? [];
  const nonDoneChildren = children.filter((c) => c.status !== "done");

  if (epic.status === "blocked") {
    return (
      <span className="text-danger">
        ⚠ {epic.depends_on_titles.length > 0
          ? t("epic.waitFor", { names: epic.depends_on_titles.join("、") })
          : t("epic.resolveDeps")}
      </span>
    );
  }
  if (epic.status === "paused") {
    return <span className="text-faint">{t("epic.statusPaused")}</span>;
  }
  // 在办：自己的中段步骤
  if (currentStep) {
    return (
      <>
        <span className="font-mono text-[11px] text-accent mr-1.5">{t(stepStageKey[currentStep.stage])}</span>
        <span className="text-body">{currentStep.title}</span>
      </>
    );
  }
  // 在办：还有未完成的子方案（父方案进度来自子方案均值）
  if (nonDoneChildren.length > 0) {
    const focusChild = [...nonDoneChildren].sort((a, b) => statusRank(a.status) - statusRank(b.status))[0];
    return (
      <>
        <span className="text-faint mr-1">→</span>
        <span className="text-body">{focusChild.title}</span>
        <span className="text-faint text-xs ml-1.5 whitespace-nowrap">
          {t("epic.closedCount", { done: children.length - nonDoneChildren.length, total: children.length })}
        </span>
      </>
    );
  }
  // 下一步：可做的自己的步骤
  if (nextStep) {
    return (
      <>
        <span className="text-faint mr-1">→</span>
        <span className="text-body">{nextStep.title}</span>
      </>
    );
  }
  // 收尾语义（自己的步骤全部 observed 或没有，子方案全部 done 或没有）
  if (epic.status === "done") {
    return <span className="text-faint">{t("epic.closed")}</span>;
  }
  if (epic.status === "observing") {
    return (
      <span className="text-amber break-words">
        ⏱ {epic.acceptance ? t("epic.observingWith", { acceptance: epic.acceptance }) : t("epic.observingPeriod")}
      </span>
    );
  }
  if (epic.progress >= 100 || (epic.steps.length > 0 && epic.steps.every((s) => s.stage === "observed"))) {
    return <span className="text-amber">{t("epic.pendingClose")}</span>;
  }
  if (epic.steps.length === 0 || epic.steps.every((s) => s.stage === "pending")) {
    return <span className="text-faint">{t("epic.notStarted")}</span>;
  }
  return <span className="text-faint">{t("epic.none")}</span>;
}

// ══════════════════════════════════════════════
// EpicRow — 总表中的一行（顶级方案或子方案），可展开看详情
// ══════════════════════════════════════════════
type EpicPatch = { title?: string; description?: string; status?: EpicStatus; priority?: string; dependsOn?: string[]; linksDoc?: string; linksBranch?: string; linksNote?: string; acceptance?: string; closeReason?: string };

interface EpicRowProps {
  epic: Epic;
  /** 0 = 顶级方案，1 = 子方案（缩进 + 树形标记） */
  depth: 0 | 1;
  allEpics: Epic[];
  lang: string;
  onUpdate: (id: string, patch: EpicPatch) => void;
  onDelete: (id: string) => void;
  onAddStep: (epicId: string, title: string, desc?: string, dependsOn?: string[]) => void;
  onUpdateStep: (stepId: string, patch: { title?: string; stage?: StepStage; description?: string }) => void;
  onDeleteStep: (stepId: string) => void;
  /** 创建子方案（仅顶级方案）。 */
  onAddChild?: (epicId: string, title: string, opts: { description?: string; priority?: string }) => void;
  /** 项目路径（用于方案级 auto_execute 控制，仅顶级方案）。 */
  projectPath?: string;
  /** S5: 上层开关状态——三态阻断原因推导（总闸急停 > 项目信任 > 方案授权）。 */
  globalEnabled?: boolean;
  projectEnabled?: boolean;
}

function EpicRow({ epic, depth, allEpics, lang, onUpdate, onDelete, onAddStep, onUpdateStep, onDeleteStep, onAddChild, projectPath, globalEnabled, projectEnabled }: EpicRowProps) {
  const { t } = useTranslation();
  const badge = epicStatusBadge[epic.status];
  const [expanded, setExpanded] = useState(false);
  const [showAddStep, setShowAddStep] = useState(false);
  const [showAddChild, setShowAddChild] = useState(false);
  const [editing, setEditing] = useState(false);
  const [autoExec, setAutoExec] = useState<boolean | null>(null);
  const isTopLevel = depth === 0;

  // Load auto_execute setting (top-level AND child epics — the Runner gates
  // on the package's own epic, so child epics need their own switch)
  useEffect(() => {
    if (projectPath) {
      getEpicAutoExecute(epic.id, projectPath).then(setAutoExec);
    }
  }, [epic.id, projectPath]);

  const toggleAutoExec = async (e: React.MouseEvent) => {
    e.stopPropagation();
    if (!projectPath) return;
    // Cycle: null(fail-closed, 未授权) → true → false → null
    const next = autoExec === null ? true : autoExec === true ? false : null;
    await setEpicAutoExecute(epic.id, projectPath, next);
    setAutoExec(next);
  };

  // S5: stable (order, created_at) ordering — ONE sorted array feeds every
  // consumer (list render, focus, next-step, dependency dropdown), so the
  // UI never follows JSON insertion order and always matches the backend.
  const steps = useMemo(
    () =>
      [...epic.steps].sort(
        (a, b) => a.order - b.order || a.created_at.localeCompare(b.created_at),
      ),
    [epic.steps],
  );

  const currentStep = steps.find((s) => IN_PROGRESS_STAGES.includes(s.stage));
  // 下一步排除在办步骤本身，避免两列显示同一条
  const nextStep = steps.find((s) => s !== currentStep && s.actionable && s.stage !== "observed");
  const isDone = epic.status === "done";
  const isBlocked = epic.status === "blocked";

  const barColor =
    isDone ? "bg-success" :
    isBlocked ? "bg-danger" :
    epic.status === "paused" ? "bg-faint" :
    epic.status === "observing" ? "bg-amber" :
    "bg-accent";

  return (
    <>
      {/* ── 行 ── */}
      <div
        className={`${GRID} py-2.5 border-b border-line/60 cursor-pointer hover:bg-surface/60 transition-colors ${
          isDone ? "opacity-50" : isBlocked ? "opacity-70" : ""
        } ${depth === 1 ? "bg-white/[.012]" : ""}`}
        onClick={() => setExpanded(!expanded)}
      >
        {/* 方案 */}
        <div className="flex items-center gap-2 min-w-0">
          {depth === 1 && <span className="text-faint shrink-0 w-4 text-center text-[10px]">└</span>}
          <span className={`text-faint shrink-0 w-4 text-center text-[10px] transition-transform ${expanded ? "rotate-90" : ""}`}>▶</span>
          <div className="min-w-0">
            <div className={`break-words leading-snug ${depth === 0 ? "text-sm font-semibold" : "text-[13px] font-medium"} ${isDone ? "text-faint line-through" : "text-heading"}`}>
              {epic.title}
            </div>
            <div className="text-xs text-faint font-mono truncate mt-0.5">
              {epic.child_count > 0
                ? t("epic.children", { count: epic.child_count })
                : t("epic.steps", { count: epic.step_count })}
              {" · "}
              {formatDate(epic.updated_at, lang)}
            </div>
          </div>
          <span className={`text-[11px] font-mono px-1.5 py-px rounded shrink-0 ${priorityBadge(epic.priority)}`}>
            {epic.priority}
          </span>
          {projectPath && (
            <button
              onClick={toggleAutoExec}
              title={
                autoExec === true
                  ? globalEnabled === false
                    ? t("execution.blockedByGlobalFirst")
                    : projectEnabled === false
                      ? t("execution.blockedByProjectFirst")
                      : t("execution.epicAutoOn")
                  : autoExec === false ? t("execution.epicAutoOff")
                  : t("execution.epicAutoInherit")
              }
              className={`text-[11px] px-1.5 py-0.5 rounded shrink-0 transition-colors ${
                autoExec === true
                  ? "bg-success/15 text-success hover:bg-success/25"
                  : autoExec === false
                    ? "bg-danger/15 text-danger hover:bg-danger/25"
                    : "bg-elevated text-faint hover:bg-line-hover"
              }`}
            >
              {autoExec === true ? "▶" : autoExec === false ? "⏸" : "⇆"}
            </button>
          )}
          {epic.close_reason && (
            <span
              className="text-[11px] px-1.5 py-px rounded shrink-0 bg-amber-bg text-amber"
              title={`${t("epic.earlyClose")}: ${epic.close_reason}`}
            >
              ⚠{t("epic.earlyClose")}
            </span>
          )}
        </div>

        {/* 状态 */}
        <div>
          <span className={`inline-flex items-center gap-1.5 text-[11px] px-2 py-0.5 rounded w-fit ${badge.bg} ${badge.text}`}>
            <span className={`w-1.5 h-1.5 rounded-full ${badge.dot}`} />
            {t(epicStatusKey[epic.status])}
          </span>
        </div>

        {/* 进度 */}
        <div className="flex items-center gap-2.5">
          <div className="flex-1 max-w-[76px] h-1 bg-line rounded-sm overflow-hidden">
            <div className={`h-full rounded-sm transition-all ${barColor}`} style={{ width: `${epic.progress}%` }} />
          </div>
          <span className="font-mono text-xs text-muted tabular-nums min-w-[36px]">{epic.progress}%</span>
        </div>

        {/* 焦点 — 每行最重要的一个信号：堵点 > 在办 > 下一步 > 待收尾 > 状态兜底。
            min-w-0 让栅格列可收缩，内容自然换行而非撑宽整行 */}
        <div className="text-[13px] leading-snug min-w-0 break-words">
          <SignalCell epic={epic} currentStep={currentStep} nextStep={nextStep} />
        </div>
      </div>

      {/* ── 展开的详情 ── */}
      {expanded && (
        <div className={`border-b border-line/60 bg-surface/30 px-4 py-3 space-y-3 ${depth === 1 ? "pl-12" : "pl-10"}`}>
          {editing ? (
            <EpicEditForm
              epic={epic}
              allEpics={allEpics}
              onSubmit={(patch) => { onUpdate(epic.id, patch); setEditing(false); }}
              onCancel={() => setEditing(false)}
            />
          ) : (
            <>
              {/* 详情区 — 有内容才渲染，标签小写等宽，内容分层 */}
              {(epic.description ||
                epic.depends_on_titles.length > 0 ||
                epic.blocks_titles.length > 0 ||
                epic.acceptance ||
                epic.close_reason ||
                epic.links.doc || epic.links.branch || epic.links.note) && (
                <section>
                  <div className="font-mono text-[10px] tracking-widest uppercase text-faint mb-1.5">{t("epic.secDetails")}</div>
                  <div className="space-y-1.5">
                    {epic.description && (
                      <p className="text-[13px] text-body leading-relaxed">{epic.description}</p>
                    )}
                    {epic.depends_on_titles.length > 0 && (
                      <div className="text-xs">
                        <span className="text-faint">{t("epic.depends")}</span>
                        <span className={isBlocked ? "text-danger" : "text-muted"}>{epic.depends_on_titles.join("、")}</span>
                      </div>
                    )}
                    {epic.blocks_titles.length > 0 && (
                      <div className="text-xs">
                        <span className="text-faint">{t("epic.blocking")}</span>
                        <span className="text-muted">{epic.blocks_titles.join("、")}</span>
                      </div>
                    )}
                    {epic.acceptance && (
                      <div className="text-xs text-amber leading-relaxed">
                        📌 {t("epic.acceptance")}{epic.acceptance}
                      </div>
                    )}
                    {epic.close_reason && (
                      <div className="text-xs text-amber/80 leading-relaxed">
                        ⚠ {t("epic.closeReason")}{epic.close_reason}
                      </div>
                    )}
                    {(epic.links.doc || epic.links.branch || epic.links.note) && (
                      <div className="flex flex-wrap gap-3 text-xs">
                        {epic.links.doc && <span className="text-accent">{t("epic.doc")}</span>}
                        {epic.links.branch && <span className="text-accent">🌿 {epic.links.branch}</span>}
                        {epic.links.note && <span className="text-muted">📝 {epic.links.note}</span>}
                      </div>
                    )}
                  </div>
                </section>
              )}

              {/* 步骤区 */}
              <section>
                <div className="font-mono text-[10px] tracking-widest uppercase text-faint mb-1">{t("epic.secSteps")}</div>
                <div className="flex flex-col gap-0.5">
                  {epic.steps.length === 0 && (
                    <div className="text-xs text-faint py-1">{t("epic.noSteps")}</div>
                  )}
                  {steps.map((step) => (
                    <StepRow
                      key={step.id}
                      step={step}
                      onUpdate={(patch) => onUpdateStep(step.id, patch)}
                      onDelete={() => onDeleteStep(step.id)}
                    />
                  ))}
                </div>
              </section>


              {/* Actions */}
              <div className="flex items-center gap-3 text-xs">
                {!showAddStep && (
                  <button className="text-accent hover:underline" onClick={(e) => { e.stopPropagation(); setShowAddStep(true); }}>
                    {t("epic.addStep")}
                  </button>
                )}
                {onAddChild && isTopLevel && !showAddChild && (
                  <button className="text-accent hover:underline" onClick={(e) => { e.stopPropagation(); setShowAddChild(true); }}>
                    {t("epic.addChild")}
                  </button>
                )}
                <button className="text-accent hover:underline" onClick={(e) => { e.stopPropagation(); copyEpicRef(epic, t); }}>
                  {t("epic.copyContinue")}
                </button>
                <button className="text-muted hover:text-heading" onClick={(e) => { e.stopPropagation(); setEditing(true); }}>
                  {t("epic.edit")}
                </button>
                <ConfirmButton
                  label={t("epic.deletePlan")}
                  confirmLabel={t("epic.confirmDelete")}
                  onConfirm={() => onDelete(epic.id)}
                  className="text-faint hover:text-danger"
                />
              </div>

              {/* 添加步骤表单 */}
              {showAddStep && (
                <StepAddForm
                  existingSteps={steps.map((s) => ({ id: s.id, title: s.title }))}
                  onCancel={() => setShowAddStep(false)}
                  onSubmit={(title, desc, dependsOn) => { onAddStep(epic.id, title, desc, dependsOn); setShowAddStep(false); }}
                />
              )}

              {/* 添加子方案表单 */}
              {showAddChild && onAddChild && (
                <div className="mt-2">
                  <NewEpicForm
                    existingTitles={allEpics.map((e) => ({ id: e.id, title: e.title }))}
                    compact
                    onCancel={() => setShowAddChild(false)}
                    onSubmit={async (title, opts) => {
                      await onAddChild(epic.id, title, opts);
                      setShowAddChild(false);
                    }}
                  />
                </div>
              )}
            </>
          )}
        </div>
      )}
    </>
  );
}

// ══════════════════════════════════════════════
// StepRow — 单个步骤，5 段胶囊轨道可点击推进阶段
// ══════════════════════════════════════════════
function StepRow({ step, onUpdate, onDelete }: {
  step: Epic["steps"][number];
  onUpdate: (patch: { title?: string; stage?: StepStage }) => void;
  onDelete: () => void;
}) {
  const { t } = useTranslation();
  const [hover, setHover] = useState(false);
  const [expanded, setExpanded] = useState(false);

  const hasPackage = !!step.package;
  const stageIdx = stageOrder.indexOf(step.stage);
  const isObserved = step.stage === "observed";

  return (
    <div
      className="rounded hover:bg-elevated/40 group"
      onMouseEnter={() => setHover(true)}
      onMouseLeave={() => setHover(false)}
    >
      <div
        className="flex items-center gap-2.5 py-1.5 px-2 cursor-pointer"
        onClick={() => hasPackage && setExpanded(!expanded)}
      >
        {/* 5 段阶段轨道：点击第 N 段 = 推进到第 N 阶段 */}
        <div className="flex items-center gap-[3px] shrink-0">
          {stageOrder.map((s, i) => (
            <button
              key={s}
              title={t(stepStageKey[s])}
              onClick={(e) => { e.stopPropagation(); onUpdate({ stage: s }); }}
              className={`w-3.5 h-1 rounded-sm transition-colors ${
                i <= stageIdx
                  ? isObserved ? "bg-success" : "bg-accent"
                  : "bg-line hover:bg-line-hover"
              }`}
            />
          ))}
        </div>
        <span className={`text-[13px] flex-1 truncate ${isObserved ? "text-faint line-through" : "text-body"}`}>
          {step.title}
        </span>
        {hasPackage && (
          <span className="text-[11px] font-mono text-success bg-success/10 px-1.5 py-px rounded shrink-0" title="Has sealed TaskPackage">
            📦
          </span>
        )}
        {step.actionable && (
          <span className="text-[11px] text-accent bg-accent-bg px-1.5 py-px rounded shrink-0">{t("epic.actionable")}</span>
        )}
        {step.depends_on_titles.length > 0 && (
          <span className="text-[11px] text-faint shrink-0 hidden md:inline" title={t("epic.waitFor", { names: step.depends_on_titles.join("、") })}>
            🔗{step.depends_on_titles.length}
          </span>
        )}
        <span className="text-[11px] font-mono text-faint w-14 text-right shrink-0">{t(stepStageKey[step.stage])}</span>
        {hover && (
          <ConfirmButton
            label="×"
            confirmLabel="✓"
            onConfirm={() => onDelete()}
            className="w-4 h-4 flex items-center justify-center text-[11px] text-faint hover:text-danger rounded-full"
          />
        )}
      </div>
      {/* Execution panel — shows package summary and run timeline */}
      {expanded && hasPackage && (
        <ExecutionPanel stepId={step.id} />
      )}
    </div>
  );
}

// ══════════════════════════════════════════════
// NewEpicForm — 新建方案
// ══════════════════════════════════════════════
function NewEpicForm({ existingTitles, onCancel, onSubmit, compact }: {
  existingTitles: { id: string; title: string }[];
  onCancel: () => void;
  onSubmit: (title: string, opts: { description?: string; priority?: string; dependsOn?: string[]; acceptance?: string }) => void;
  compact?: boolean;
}) {
  const { t } = useTranslation();
  const [title, setTitle] = useState("");
  const [description, setDescription] = useState("");
  const [priority, setPriority] = useState("P1");
  const [dependsOn, setDependsOn] = useState<string[]>([]);
  const [acceptance, setAcceptance] = useState("");

  return (
    <div className={`rounded-lg border border-accent/40 bg-accent-bg/30 ${compact ? "p-2" : "p-3"}`}>
      <input
        autoFocus
        className="w-full bg-transparent border-b border-line px-1 py-1 text-[13px] text-heading focus:outline-none focus:border-accent"
        placeholder={t("epic.titlePlaceholder")}
        value={title}
        onChange={(e) => setTitle(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter" && title.trim()) onSubmit(title.trim(), { description: description.trim() || undefined, priority, dependsOn: dependsOn.length ? dependsOn : undefined, acceptance: acceptance.trim() || undefined });
          if (e.key === "Escape") onCancel();
        }}
      />
      <input
        className="w-full bg-transparent px-1 py-1 mt-1 text-xs text-muted focus:outline-none"
        placeholder={t("epic.descPlaceholder")}
        value={description}
        onChange={(e) => setDescription(e.target.value)}
      />
      <input
        className="w-full bg-transparent px-1 py-1 mt-0.5 text-xs text-amber/80 focus:outline-none"
        placeholder={t("epic.acceptancePlaceholder")}
        value={acceptance}
        onChange={(e) => setAcceptance(e.target.value)}
      />
      <div className="flex items-center gap-2 mt-2">
        <select
          className="bg-elevated text-xs text-body rounded px-1.5 py-0.5 focus:outline-none"
          value={priority}
          onChange={(e) => setPriority(e.target.value)}
        >
          <option value="P0">P0</option>
          <option value="P1">P1</option>
          <option value="P2">P2</option>
          <option value="P3">P3</option>
        </select>
        {existingTitles.length > 0 && (
          <select
            multiple
            className="flex-1 bg-elevated text-xs text-body rounded px-1 py-0.5 focus:outline-none h-12"
            value={dependsOn}
            onChange={(e) => {
              const selected = Array.from(e.target.selectedOptions).map((o) => o.value);
              setDependsOn(selected);
            }}
            title={t("epic.cmdCtrlMultiSelect")}
          >
            {existingTitles.map((t) => (
              <option key={t.id} value={t.id}>{t.title}</option>
            ))}
          </select>
        )}
      </div>
      <div className="flex items-center gap-3 mt-2">
        <button
          className="text-xs px-2 py-1 bg-accent text-white rounded hover:opacity-90 disabled:opacity-40"
          disabled={!title.trim()}
          onClick={() => onSubmit(title.trim(), { description: description.trim() || undefined, priority, dependsOn: dependsOn.length ? dependsOn : undefined, acceptance: acceptance.trim() || undefined })}
        >
          {t("epic.create")}
        </button>
        <button className="text-xs text-faint hover:text-body" onClick={onCancel}>{t("epic.cancel")}</button>
        <span className="text-[11px] text-faint ml-auto">{t("epic.cmdMultiSelect")}</span>
      </div>
    </div>
  );
}

// ══════════════════════════════════════════════
// EpicEditForm — 编辑方案的详情
// ══════════════════════════════════════════════
function EpicEditForm({ epic, allEpics, onSubmit, onCancel }: {
  epic: Epic;
  allEpics: Epic[];
  onSubmit: (patch: { title?: string; description?: string; status?: EpicStatus; priority?: string; dependsOn?: string[]; linksDoc?: string; linksBranch?: string; linksNote?: string; acceptance?: string; closeReason?: string }) => void;
  onCancel: () => void;
}) {
  const { t } = useTranslation();
  const [title, setTitle] = useState(epic.title);
  const [description, setDescription] = useState(epic.description);
  const [status, setStatus] = useState<EpicStatus>(epic.status);
  const [priority, setPriority] = useState(epic.priority);
  const [dependsOn, setDependsOn] = useState<string[]>(epic.depends_on);
  const [linksDoc, setLinksDoc] = useState(epic.links.doc ?? "");
  const [linksBranch, setLinksBranch] = useState(epic.links.branch ?? "");
  const [linksNote, setLinksNote] = useState(epic.links.note ?? "");
  const [acceptance, setAcceptance] = useState(epic.acceptance ?? "");
  const [closeReason, setCloseReason] = useState(epic.close_reason ?? "");

  return (
    <div className="flex flex-col gap-2 py-1">
      <input
        autoFocus
        className="bg-transparent border-b border-line px-1 py-1 text-[13px] text-heading focus:outline-none focus:border-accent"
        value={title}
        onChange={(e) => setTitle(e.target.value)}
      />
      <textarea
        className="bg-transparent px-1 py-1 text-xs text-muted focus:outline-none resize-none"
        rows={2}
        placeholder={t("epic.epicDescPlaceholder")}
        value={description}
        onChange={(e) => setDescription(e.target.value)}
      />
      <div className="flex items-center gap-2">
        <label className="text-xs text-faint">{t("epic.status")}</label>
        <select
          className="bg-elevated text-xs text-body rounded px-1.5 py-0.5 focus:outline-none"
          value={status}
          onChange={(e) => setStatus(e.target.value as EpicStatus)}
        >
          {statusOptions.map((s) => (
            <option key={s} value={s}>{t(epicStatusKey[s])}</option>
          ))}
        </select>
        <label className="text-xs text-faint ml-2">{t("epic.priority")}</label>
        <select
          className="bg-elevated text-xs text-body rounded px-1.5 py-0.5 focus:outline-none"
          value={priority}
          onChange={(e) => setPriority(e.target.value)}
        >
          <option value="P0">P0</option>
          <option value="P1">P1</option>
          <option value="P2">P2</option>
          <option value="P3">P3</option>
        </select>
      </div>
      {/* Dependencies (multi-select) */}
      <div className="flex items-center gap-2">
        <label className="text-xs text-faint shrink-0">{t("epic.deps")}</label>
        <select
          multiple
          className="flex-1 bg-elevated text-xs text-body rounded px-1 py-0.5 focus:outline-none h-14"
          value={dependsOn}
          onChange={(e) => setDependsOn(Array.from(e.target.selectedOptions).map((o) => o.value))}
        >
          {allEpics.filter((e) => e.id !== epic.id).map((e) => (
            <option key={e.id} value={e.id}>{e.priority} {e.title}</option>
          ))}
        </select>
      </div>
      <div className="flex flex-col gap-1">
        <input className="bg-transparent border-b border-line px-1 py-0.5 text-xs text-muted focus:outline-none" placeholder={t("epic.docPlaceholder")} value={linksDoc} onChange={(e) => setLinksDoc(e.target.value)} />
        <input className="bg-transparent border-b border-line px-1 py-0.5 text-xs text-muted focus:outline-none" placeholder={t("epic.branchPlaceholder")} value={linksBranch} onChange={(e) => setLinksBranch(e.target.value)} />
        <input className="bg-transparent border-b border-line px-1 py-0.5 text-xs text-muted focus:outline-none" placeholder={t("epic.notePlaceholder")} value={linksNote} onChange={(e) => setLinksNote(e.target.value)} />
      </div>
      {/* 验收条件 + 收尾原因 */}
      <div className="flex flex-col gap-1">
        <textarea
          className="bg-transparent border-b border-line px-1 py-0.5 text-xs text-amber/90 focus:outline-none resize-none"
          rows={2}
          placeholder={t("epic.acceptancePlaceholder")}
          value={acceptance}
          onChange={(e) => setAcceptance(e.target.value)}
        />
        {status === "done" && (
          <input
            className="bg-transparent border-b border-line px-1 py-0.5 text-xs text-amber/80 focus:outline-none"
            placeholder={t("epic.closeReasonPlaceholder")}
            value={closeReason}
            onChange={(e) => setCloseReason(e.target.value)}
          />
        )}
      </div>
      <div className="flex items-center gap-3">
        <button
          className="text-xs px-2 py-1 bg-accent text-white rounded hover:opacity-90"
          onClick={() => onSubmit({
            title, description,
            status, priority,
            dependsOn,
            linksDoc, linksBranch, linksNote,
            acceptance, closeReason: closeReason,
          })}
        >
          {t("epic.save")}
        </button>
        <button className="text-xs text-faint hover:text-body" onClick={onCancel}>{t("epic.cancel")}</button>
      </div>
    </div>
  );
}

/** 优先级徽章配色（P0 红 / P1 蓝 / P2 黄 / P3 灰）。 */
function priorityBadge(priority: string): string {
  switch ((priority || "").toUpperCase()) {
    case "P0": return "bg-danger/15 text-danger";
    case "P1": return "bg-accent-bg text-accent";
    case "P2": return "bg-amber-bg text-amber";
    default: return "bg-elevated text-faint";
  }
}

// ══════════════════════════════════════════════
// copyEpicRef — copies the "continue plan" reference to the clipboard.
// Outputs the actionable-step set; pasting it into an AI session lets the
// assistant call list_epics to match the title and continue.
// Protocol text stays in sync with mcp-server/src/index.ts tool descriptions.
// ══════════════════════════════════════════════
type TFunc = (key: string, opts?: Record<string, unknown>) => string;

function copyEpicRef(epic: Epic, t: TFunc) {
  const actionable = [...epic.steps]
    .sort((a, b) => a.order - b.order || a.created_at.localeCompare(b.created_at))
    .filter((s) => s.actionable);
  const status = t(epicStatusKey[epic.status]);
  const lines = [
    t("protocol.continuePlan", { title: epic.title }),
    t("protocol.progress", { progress: epic.progress, status, steps: epic.step_count }),
  ];
  if (actionable.length > 0) {
    lines.push(t("protocol.actionable", {
      parallel: actionable.length > 1 ? t("epic.parallel") : "",
      names: actionable.map((s) => s.title).join("、"),
    }));
  } else {
    lines.push(t("protocol.actionableNone"));
  }
  lines.push(t("protocol.planId", { id: epic.id }));
  navigator.clipboard.writeText(lines.join("\n")).then(() => {}).catch(() => {});
}

// ══════════════════════════════════════════════
// StepAddForm
// ══════════════════════════════════════════════
function StepAddForm({ onCancel, onSubmit, existingSteps }: {
  onCancel: () => void;
  onSubmit: (title: string, desc: string | undefined, dependsOn: string[]) => void;
  existingSteps: { id: string; title: string }[];
}) {
  const { t } = useTranslation();
  const [title, setTitle] = useState("");
  const [desc, setDesc] = useState("");
  const [dependsOn, setDependsOn] = useState<string[]>([]);
  return (
    <div className="mt-1.5 flex flex-col gap-1.5">
      <div className="flex items-center gap-2">
        <input
          autoFocus
          className="flex-1 bg-elevated/50 rounded px-2 py-1 text-[13px] text-heading focus:outline-none focus:ring-1 focus:ring-accent"
          placeholder={t("epic.stepTitlePlaceholder")}
          value={title}
          onChange={(e) => setTitle(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && title.trim()) onSubmit(title.trim(), desc.trim() || undefined, dependsOn);
            if (e.key === "Escape") onCancel();
          }}
        />
        <button
          className="text-xs px-1.5 py-0.5 bg-accent text-white rounded disabled:opacity-40"
          disabled={!title.trim()}
          onClick={() => onSubmit(title.trim(), desc.trim() || undefined, dependsOn)}
        >
          {t("epic.addStep")}
        </button>
        <button className="text-xs text-faint hover:text-body" onClick={onCancel}>{t("epic.cancel")}</button>
      </div>
      {existingSteps.length > 0 && (
        <div className="flex items-center gap-1.5">
          <span className="text-[11px] text-faint shrink-0">{t("epic.depsOptional")}</span>
          <select
            multiple
            className="flex-1 bg-elevated/50 text-[11px] text-body rounded px-1 py-0.5 focus:outline-none h-10"
            value={dependsOn}
            onChange={(e) => setDependsOn(Array.from(e.target.selectedOptions).map((o) => o.value))}
            title={t("epic.stepDepsTitle")}
          >
            {existingSteps.map((s) => (
              <option key={s.id} value={s.id}>{s.title}</option>
            ))}
          </select>
        </div>
      )}
    </div>
  );
}
