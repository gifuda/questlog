import { useState, useEffect } from "react";
import { useTranslation } from "react-i18next";
import { useEpics } from "../hooks/useEpics";
import { useProjectRunner, useRunnerStatus, getEpicAutoExecute, setEpicAutoExecute } from "../hooks/useExecutions";
import { epicStatusBadge, stageOrder, stageLabel, stageIcon, statusOptions, statusRank } from "../epicConstants";
import type { Epic, EpicStatus, StepStage } from "../types/epics";
import ConfirmButton from "./ConfirmButton";
import ExecutionPanel, { DecisionInbox } from "./ExecutionPanel";
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

interface EpicBoardProps {
  projectPath: string;
}

export default function EpicBoard({ projectPath }: EpicBoardProps) {
  const { t } = useTranslation();
  const { data, summary, epics, addEpic, updateEpic, deleteEpic, addStep, updateStep, deleteStep } = useEpics(projectPath);
  const { status: projectStatus, loading: projectLoading, toggle: toggleProjectRunner } = useProjectRunner(projectPath);
  const { status: runnerStatus, lastPollSecsAgo } = useRunnerStatus();
  const [showNewEpic, setShowNewEpic] = useState(false);

  // Backend already returns top-level epics newest-first; sort defensively.
  const sorted = [...epics].sort((a, b) => (b.created_at || "").localeCompare(a.created_at || ""));

  return (
    <div className="flex flex-col h-full">
      {/* Global summary — 3-second glance */}
      {summary && summary.total > 0 && (
        <div className="px-4 py-2.5 border-b border-line bg-surface/30">
          <div className="flex items-center gap-3 text-[11px] text-muted flex-wrap">
            <span className="font-bold text-heading">{t("epic.summaryTotal", { total: summary.total })}</span>
            <span className="text-blue-400">{t("epic.summaryActive", { count: summary.active })}</span>
            <span className="text-yellow-400">{t("epic.summaryObserving", { count: summary.observing })}</span>
            <span className="text-red-400">{t("epic.summaryBlocked", { count: summary.blocked })}</span>
            <span className="text-green-400">{t("epic.summaryDone", { count: summary.done })}</span>
          </div>
          {summary.focus && (
            <div className="mt-1 text-[11px]">
              <span className="text-faint">{t("epic.focus")}</span>
              <span className="text-heading font-medium">{summary.focus.title}</span>
              <span className="ml-2 text-blue-400">{summary.focus.progress}%</span>
            </div>
          )}
          {summary.next && (
            <div className="mt-0.5 text-[11px]">
              <span className="text-faint">{t("epic.next")}</span>
              <span className="text-body">{summary.next.title}</span>
              {summary.next.waiting_for.length > 0 && (
                <span className="ml-1.5 text-red-400">{t("epic.waiting", { names: summary.next.waiting_for.join("、") })}</span>
              )}
            </div>
          )}
        </div>
      )}

      {/* Runner toggle — per-project execution control + live heartbeat */}
      <div className="px-4 py-1 border-b border-line/50 flex items-center justify-end gap-1.5">
        {runnerStatus?.running ? (
          runnerStatus.thread_alive && (lastPollSecsAgo ?? 999) < 30 ? (
            runnerStatus.active_executions > 0 ? (
              <span
                className="flex items-center gap-1 text-[9px] font-medium text-blue-400 shrink-0"
                title={t("execution.runnerStatusHint")}
              >
                <span className="w-1.5 h-1.5 rounded-full bg-blue-400 animate-pulse" />
                {t("execution.runnerStatusActive", { count: runnerStatus.active_executions })}
              </span>
            ) : runnerStatus.ready_packages > 0 ? (
              <span
                className="flex items-center gap-1 text-[9px] font-medium text-orange-400 shrink-0"
                title={t("execution.runnerStatusHint")}
              >
                <span className="w-1.5 h-1.5 rounded-full bg-orange-400" />
                {t("execution.runnerStatusQueued", { count: runnerStatus.ready_packages })}
              </span>
            ) : (
              <span
                className="flex items-center gap-1 text-[9px] font-medium text-green-400 shrink-0"
                title={t("execution.runnerStatusHint")}
              >
                <span className="w-1.5 h-1.5 rounded-full bg-green-400 animate-pulse" />
                {t("execution.runnerStatusIdle", { secs: lastPollSecsAgo ?? "—" })}
              </span>
            )
          ) : (
            <span
              className="flex items-center gap-1 text-[9px] font-medium text-yellow-400 shrink-0"
              title={t("execution.runnerStatusHint")}
            >
              <span className="w-1.5 h-1.5 rounded-full bg-yellow-400" />
              {t("execution.runnerStatusStale", { secs: lastPollSecsAgo ?? "—" })}
            </span>
          )
        ) : (
          <span
            className="flex items-center gap-1 text-[9px] font-medium text-faint shrink-0"
            title={t("execution.runnerStatusHint")}
          >
            <span className="w-1.5 h-1.5 rounded-full bg-line-hover" />
            {t("execution.runnerStatusOff")}
          </span>
        )}
        <ExecutorModelSelector />
        <button
          onClick={toggleProjectRunner}
          disabled={projectLoading}
          title={
            projectStatus?.project_enabled
              ? t("execution.projectRunnerEffective")
              : t("execution.projectRunnerOn")
          }
          className={`px-2 py-0.5 rounded-full text-[9px] font-medium transition-colors shrink-0 disabled:opacity-40 ${
            projectStatus?.project_enabled
              ? "bg-green-500/20 text-green-400 hover:bg-green-500/30"
              : "bg-elevated text-faint hover:bg-line-hover"
          }`}
        >
          📂 {projectStatus?.project_enabled ? t("execution.projectOn") : t("execution.projectOff")}
        </button>
      </div>
      <DecisionInbox />

      {/* 路线图主视图 */}
      <div className="flex-1 overflow-y-auto p-3 flex flex-col gap-2.5">
        {sorted.length === 0 && !showNewEpic && (
          <div className="flex flex-col items-center justify-center h-full text-faint gap-3">
            <span className="text-3xl">📊</span>
            <span className="text-sm">{t("epic.emptyTitle")}</span>
            <span className="text-[11px]">{t("epic.emptyDesc")}</span>
            <button
              className="text-[11px] px-3 py-1.5 bg-accent text-white rounded-lg hover:opacity-90"
              onClick={() => setShowNewEpic(true)}
            >
              {t("epic.newPlan")}
            </button>
          </div>
        )}

        {sorted.map((epic) => (
          <EpicCard
            key={epic.id}
            epic={epic}
            allEpics={epics}
            onUpdate={(id, patch) => updateEpic(id, patch)}
            onDelete={(id) => deleteEpic(id)}
            onAddStep={(id, title, desc, dependsOn) => addStep(id, title, desc, dependsOn)}
            onUpdateStep={(stepId, patch) => updateStep(stepId, patch)}
            onDeleteStep={(stepId) => deleteStep(stepId)}
            onAddChild={(id, title, opts) => addEpic(title, { ...opts, parentId: id })}
            projectPath={projectPath}
          />
        ))}

        {/* 新建方案 */}
        {showNewEpic && (
          <NewEpicForm
            existingTitles={epics.map((e) => ({ id: e.id, title: e.title }))}
            onCancel={() => setShowNewEpic(false)}
            onSubmit={async (title, opts) => {
              await addEpic(title, opts);
              setShowNewEpic(false);
            }}
          />
        )}
      </div>

      {/* Bottom new-plan button */}
      {!showNewEpic && sorted.length > 0 && (
        <div className="px-4 py-2 border-t border-line">
          <button
            className="text-[11px] text-accent hover:underline"
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
// EpicCard — 单个方案卡，可展开看步骤
// ══════════════════════════════════════════════
type EpicPatch = { title?: string; description?: string; status?: EpicStatus; priority?: string; dependsOn?: string[]; linksDoc?: string; linksBranch?: string; linksNote?: string; acceptance?: string; closeReason?: string };

interface EpicCardProps {
  epic: Epic;
  allEpics: Epic[];
  onUpdate: (id: string, patch: EpicPatch) => void;
  onDelete: (id: string) => void;
  onAddStep: (epicId: string, title: string, desc?: string, dependsOn?: string[]) => void;
  onUpdateStep: (stepId: string, patch: { title?: string; stage?: StepStage; description?: string }) => void;
  onDeleteStep: (stepId: string) => void;
  /** 创建子方案（parent_id 指向 epicId）。仅顶层方案需要。 */
  onAddChild?: (epicId: string, title: string, opts: { description?: string; priority?: string }) => void;
  /** 层级缩进（顶层 0，子方案 1） */
  depth?: number;
  /** 项目路径（用于方案级 auto_execute 控制） */
  projectPath?: string;
}

function EpicCard({ epic, allEpics, onUpdate, onDelete, onAddStep, onUpdateStep, onDeleteStep, onAddChild, depth = 0, projectPath }: EpicCardProps) {
  const { t } = useTranslation();
  const badge = epicStatusBadge[epic.status];
  const [expanded, setExpanded] = useState(false);
  const [showAddStep, setShowAddStep] = useState(false);
  const [showAddChild, setShowAddChild] = useState(false);
  const [editing, setEditing] = useState(false);
  const [autoExec, setAutoExec] = useState<boolean | null>(null);
  const hasChildren = epic.children?.length > 0;
  const isTopLevel = depth === 0;

  // Load auto_execute setting for top-level epics
  useEffect(() => {
    if (isTopLevel && projectPath) {
      getEpicAutoExecute(epic.id, projectPath).then(setAutoExec);
    }
  }, [epic.id, projectPath, isTopLevel]);

  const toggleAutoExec = async (e: React.MouseEvent) => {
    e.stopPropagation();
    if (!projectPath) return;
    // Cycle: null(inherit) → true → false → null
    const next = autoExec === null ? true : autoExec === true ? false : null;
    await setEpicAutoExecute(epic.id, projectPath, next);
    setAutoExec(next);
  };

  return (
    <div className={`rounded-lg border ${epic.status === "active" ? "border-blue-500/40 bg-blue-500/5" : "border-line bg-surface/50"} transition-colors`}>
      {/* 卡片头部 */}
      <div className="px-3 py-2.5 cursor-pointer" onClick={() => setExpanded(!expanded)}>
        <div className="flex items-center gap-2">
          <span className={`w-2 h-2 rounded-full shrink-0 ${badge.dot}`} />
          <span className={`text-xs font-medium flex-1 truncate ${epic.status === "done" ? "line-through text-faint" : "text-heading"}`}>
            {epic.title}
          </span>
          <span className={`text-[9px] px-1 py-px rounded shrink-0 ${priorityBadge(epic.priority)}`}>
            {epic.priority}
          </span>
          {isTopLevel && projectPath && (
            <button
              onClick={toggleAutoExec}
              title={
                autoExec === true ? t("execution.epicAutoOn")
                  : autoExec === false ? t("execution.epicAutoOff")
                  : t("execution.epicAutoInherit")
              }
              className={`text-[9px] px-1.5 py-0.5 rounded shrink-0 transition-colors ${
                autoExec === true
                  ? "bg-green-500/20 text-green-400 hover:bg-green-500/30"
                  : autoExec === false
                    ? "bg-red-500/15 text-red-400 hover:bg-red-500/25"
                    : "bg-elevated text-faint hover:bg-line-hover"
              }`}
            >
              {autoExec === true ? "▶" : autoExec === false ? "⏸" : "⇆"}
            </button>
          )}
          {epic.close_reason && (
            <span
              className="text-[8px] px-1 py-px rounded shrink-0 bg-yellow-500/15 text-yellow-300"
              title={`${t("epic.earlyClose")}: ${epic.close_reason}`}
            >
              ⚠️{t("epic.earlyClose")}
            </span>
          )}
          <span className="text-[10px] text-muted shrink-0">{epic.progress}%</span>
          <span className={`text-[9px] px-1.5 py-0.5 rounded ${badge.bg} ${badge.text} shrink-0`}>
            {t(epicStatusKey[epic.status])}
          </span>
          <span className="text-[10px] text-faint shrink-0">{expanded ? "▾" : "▸"}</span>
        </div>

        {/* 进度条 */}
        <div className="mt-1.5 flex items-center gap-2">
          <div className="flex-1 h-1.5 bg-elevated rounded-full overflow-hidden">
            <div
              className={`h-full rounded-full transition-all ${
                epic.status === "done" ? "bg-green-500" :
                epic.status === "blocked" ? "bg-red-500" :
                epic.status === "paused" ? "bg-gray-500" :
                "bg-blue-500"
              }`}
              style={{ width: `${epic.progress}%` }}
            />
          </div>
          <span className="text-[9px] text-faint shrink-0">{t("epic.steps", { count: epic.step_count })}</span>
        </div>

        {/* Dependency hint (collapsed) */}
        {!expanded && epic.depends_on_titles.length > 0 && epic.status === "blocked" && (
          <div className="mt-1 text-[10px] text-red-400">
            {t("epic.blockedWait", { names: epic.depends_on_titles.join("、") })}
          </div>
        )}
      </div>

      {/* 展开内容 */}
      {expanded && (
        <div className="px-3 pb-3 border-t border-line/50 pt-2.5">
          {editing ? (
            <EpicEditForm
              epic={epic}
              allEpics={allEpics}
              onSubmit={(patch) => { onUpdate(epic.id, patch); setEditing(false); }}
              onCancel={() => setEditing(false)}
            />
          ) : (
            <>
              {/* 子方案（如果有）— 递归渲染，缩进一级 */}
              {(hasChildren || onAddChild) && (
                <div className={`mb-2 flex flex-col gap-1.5 ${depth === 0 ? "ml-0" : "ml-3 border-l-2 border-line pl-2"}`}>
                  <div className="flex items-center gap-2">
                    <span className="text-[10px] text-faint">{t("epic.children", { count: epic.children?.length ?? 0 })}</span>
                    {onAddChild && depth === 0 && !showAddChild && (
                      <button className="text-[10px] text-accent hover:underline" onClick={(e) => { e.stopPropagation(); setShowAddChild(true); }}>
                        {t("epic.addChild")}
                      </button>
                    )}
                  </div>
                  {epic.children?.map((child) => (
                    <EpicCard
                      key={child.id}
                      epic={child}
                      allEpics={allEpics}
                      onUpdate={onUpdate}
                      onDelete={onDelete}
                      onAddStep={onAddStep}
                      onUpdateStep={onUpdateStep}
                      onDeleteStep={onDeleteStep}
                      depth={depth + 1}
                    />
                  ))}
                  {showAddChild && onAddChild && (
                    <NewEpicForm
                      existingTitles={allEpics.map((e) => ({ id: e.id, title: e.title }))}
                      compact
                      onCancel={() => setShowAddChild(false)}
                      onSubmit={async (title, opts) => {
                        await onAddChild(epic.id, title, opts);
                        setShowAddChild(false);
                      }}
                    />
                  )}
                </div>
              )}

              {/* 描述 */}
              {epic.description && (
                <p className="text-[11px] text-muted mb-2 leading-relaxed">{epic.description}</p>
              )}

              {/* 依赖信息 */}
              {epic.depends_on_titles.length > 0 && (
                <div className="text-[10px] text-faint mb-1">
                  {t("epic.depends")} <span className={epic.status === "blocked" ? "text-red-400" : "text-muted"}>{epic.depends_on_titles.join("、")}</span>
                </div>
              )}
              {epic.blocks_titles.length > 0 && (
                <div className="text-[10px] text-faint mb-1">
                  {t("epic.blocking")} <span className="text-muted">{epic.blocks_titles.join("、")}</span>
                </div>
              )}

              {/* 验收条件 */}
              {epic.acceptance && (
                <div className="text-[10px] text-yellow-300/90 mb-1 leading-relaxed">
                  📌 {t("epic.acceptance")}{epic.acceptance}
                </div>
              )}
              {epic.close_reason && (
                <div className="text-[10px] text-yellow-400/80 mb-1 leading-relaxed">
                  ⚠️ {t("epic.closeReason")}{epic.close_reason}
                </div>
              )}

              {/* Links */}
              {(epic.links.doc || epic.links.branch || epic.links.note) && (
                <div className="flex flex-wrap gap-2 mb-2 text-[10px]">
                  {epic.links.doc && <span className="text-accent">{t("epic.doc")}</span>}
                  {epic.links.branch && <span className="text-accent">🌿 {epic.links.branch}</span>}
                  {epic.links.note && <span className="text-muted">📝 {epic.links.note}</span>}
                </div>
              )}

              {/* Step list */}
              <div className="flex flex-col gap-1 mb-2">
                {epic.steps.length === 0 && (
                  <div className="text-[10px] text-faint py-1">{t("epic.noSteps")}</div>
                )}
                {epic.steps.map((step) => (
                  <StepRow
                    key={step.id}
                    step={step}
                    onUpdate={(patch) => onUpdateStep(step.id, patch)}
                    onDelete={() => onDeleteStep(step.id)}
                  />
                ))}
              </div>

              {/* Actions */}
              <div className="flex items-center gap-3 text-[10px]">
                {!showAddStep && (
                  <button className="text-accent hover:underline" onClick={(e) => { e.stopPropagation(); setShowAddStep(true); }}>
                    {t("epic.addStep")}
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
                  existingSteps={epic.steps.map((s) => ({ id: s.id, title: s.title }))}
                  onCancel={() => setShowAddStep(false)}
                  onSubmit={(title, desc, dependsOn) => { onAddStep(epic.id, title, desc, dependsOn); setShowAddStep(false); }}
                />
              )}
            </>
          )}
        </div>
      )}
    </div>
  );
}

// ══════════════════════════════════════════════
// StepRow — 单个步骤，可拖动阶段滑块
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

  return (
    <div
      className="rounded hover:bg-elevated/40 group"
      onMouseEnter={() => setHover(true)}
      onMouseLeave={() => setHover(false)}
    >
      <div
        className="flex items-center gap-2 py-1 px-1.5 cursor-pointer"
        onClick={() => hasPackage && setExpanded(!expanded)}
      >
        <span className="text-xs shrink-0 w-4 text-center">{stageIcon[step.stage]}</span>
        <span className={`text-[11px] flex-1 truncate ${step.stage === "observed" ? "text-faint line-through" : "text-body"}`}>
          {step.title}
        </span>
        {hasPackage && (
          <span className="text-[8px] text-green-400 bg-green-500/10 px-1 py-px rounded shrink-0" title="Has sealed TaskPackage">
            📦
          </span>
        )}
        {step.actionable && (
          <span className="text-[8px] text-blue-400 bg-blue-500/10 px-1 py-px rounded shrink-0">{t("epic.actionable")}</span>
        )}
        {step.depends_on_titles.length > 0 && (
          <span className="text-[8px] text-faint shrink-0 hidden md:inline" title={t("epic.waitFor", { names: step.depends_on_titles.join("、") })}>
            🔗{step.depends_on_titles.length}
          </span>
        )}
        {/* Stage buttons */}
        <div className="flex items-center gap-0.5 shrink-0">
          {stageOrder.map((s) => (
            <button
              key={s}
              title={t(stepStageKey[s])}
              onClick={(e) => { e.stopPropagation(); onUpdate({ stage: s }); }}
              className={`w-3 h-3 rounded-sm transition-colors ${
                stageOrder.indexOf(step.stage) >= stageOrder.indexOf(s)
                  ? "bg-blue-500"
                  : "bg-elevated hover:bg-line-hover"
              }`}
            />
          ))}
        </div>
        <span className="text-[9px] text-faint w-8 text-right shrink-0">{t(stepStageKey[step.stage])}</span>
        {hover && (
          <ConfirmButton
            label="×"
            confirmLabel="✓"
            onConfirm={() => onDelete()}
            className="w-3.5 h-3.5 flex items-center justify-center text-[8px] text-faint hover:text-danger rounded-full"
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
        className="w-full bg-transparent border-b border-line px-1 py-1 text-xs text-heading focus:outline-none focus:border-accent"
        placeholder={t("epic.titlePlaceholder")}
        value={title}
        onChange={(e) => setTitle(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter" && title.trim()) onSubmit(title.trim(), { description: description.trim() || undefined, priority, dependsOn: dependsOn.length ? dependsOn : undefined, acceptance: acceptance.trim() || undefined });
          if (e.key === "Escape") onCancel();
        }}
      />
      <input
        className="w-full bg-transparent px-1 py-1 mt-1 text-[11px] text-muted focus:outline-none"
        placeholder={t("epic.descPlaceholder")}
        value={description}
        onChange={(e) => setDescription(e.target.value)}
      />
      <input
        className="w-full bg-transparent px-1 py-1 mt-0.5 text-[10px] text-yellow-300/80 focus:outline-none"
        placeholder={t("epic.acceptancePlaceholder")}
        value={acceptance}
        onChange={(e) => setAcceptance(e.target.value)}
      />
      <div className="flex items-center gap-2 mt-2">
        <select
          className="bg-elevated text-[11px] text-body rounded px-1.5 py-0.5 focus:outline-none"
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
            className="flex-1 bg-elevated text-[10px] text-body rounded px-1 py-0.5 focus:outline-none h-12"
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
          className="text-[11px] px-2 py-1 bg-accent text-white rounded hover:opacity-90 disabled:opacity-40"
          disabled={!title.trim()}
          onClick={() => onSubmit(title.trim(), { description: description.trim() || undefined, priority, dependsOn: dependsOn.length ? dependsOn : undefined, acceptance: acceptance.trim() || undefined })}
        >
          {t("epic.create")}
        </button>
        <button className="text-[11px] text-faint hover:text-body" onClick={onCancel}>{t("epic.cancel")}</button>
        <span className="text-[9px] text-faint ml-auto">{t("epic.cmdMultiSelect")}</span>
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
        className="bg-transparent border-b border-line px-1 py-1 text-xs text-heading focus:outline-none focus:border-accent"
        value={title}
        onChange={(e) => setTitle(e.target.value)}
      />
      <textarea
        className="bg-transparent px-1 py-1 text-[11px] text-muted focus:outline-none resize-none"
        rows={2}
        placeholder={t("epic.epicDescPlaceholder")}
        value={description}
        onChange={(e) => setDescription(e.target.value)}
      />
      <div className="flex items-center gap-2">
        <label className="text-[10px] text-faint">{t("epic.status")}</label>
        <select
          className="bg-elevated text-[11px] text-body rounded px-1.5 py-0.5 focus:outline-none"
          value={status}
          onChange={(e) => setStatus(e.target.value as EpicStatus)}
        >
          {statusOptions.map((s) => (
            <option key={s} value={s}>{t(epicStatusKey[s])}</option>
          ))}
        </select>
        <label className="text-[10px] text-faint ml-2">{t("epic.priority")}</label>
        <select
          className="bg-elevated text-[11px] text-body rounded px-1.5 py-0.5 focus:outline-none"
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
        <label className="text-[10px] text-faint shrink-0">{t("epic.deps")}</label>
        <select
          multiple
          className="flex-1 bg-elevated text-[10px] text-body rounded px-1 py-0.5 focus:outline-none h-14"
          value={dependsOn}
          onChange={(e) => setDependsOn(Array.from(e.target.selectedOptions).map((o) => o.value))}
        >
          {allEpics.filter((e) => e.id !== epic.id).map((e) => (
            <option key={e.id} value={e.id}>{e.priority} {e.title}</option>
          ))}
        </select>
      </div>
      <div className="flex flex-col gap-1">
        <input className="bg-transparent border-b border-line px-1 py-0.5 text-[10px] text-muted focus:outline-none" placeholder={t("epic.docPlaceholder")} value={linksDoc} onChange={(e) => setLinksDoc(e.target.value)} />
        <input className="bg-transparent border-b border-line px-1 py-0.5 text-[10px] text-muted focus:outline-none" placeholder={t("epic.branchPlaceholder")} value={linksBranch} onChange={(e) => setLinksBranch(e.target.value)} />
        <input className="bg-transparent border-b border-line px-1 py-0.5 text-[10px] text-muted focus:outline-none" placeholder={t("epic.notePlaceholder")} value={linksNote} onChange={(e) => setLinksNote(e.target.value)} />
      </div>
      {/* 验收条件 + 收尾原因 */}
      <div className="flex flex-col gap-1">
        <textarea
          className="bg-transparent border-b border-line px-1 py-0.5 text-[10px] text-yellow-300/90 focus:outline-none resize-none"
          rows={2}
          placeholder={t("epic.acceptancePlaceholder")}
          value={acceptance}
          onChange={(e) => setAcceptance(e.target.value)}
        />
        {status === "done" && (
          <input
            className="bg-transparent border-b border-line px-1 py-0.5 text-[10px] text-yellow-400/80 focus:outline-none"
            placeholder={t("epic.closeReasonPlaceholder")}
            value={closeReason}
            onChange={(e) => setCloseReason(e.target.value)}
          />
        )}
      </div>
      <div className="flex items-center gap-3">
        <button
          className="text-[11px] px-2 py-1 bg-accent text-white rounded hover:opacity-90"
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
        <button className="text-[11px] text-faint hover:text-body" onClick={onCancel}>{t("epic.cancel")}</button>
      </div>
    </div>
  );
}

/** 优先级徽章配色（P0 红 / P1 蓝 / P2 黄 / P3 灰）。 */
function priorityBadge(priority: string): string {
  switch ((priority || "").toUpperCase()) {
    case "P0": return "bg-red-500/15 text-red-300";
    case "P1": return "bg-blue-500/15 text-blue-300";
    case "P2": return "bg-yellow-500/15 text-yellow-300";
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
  const actionable = epic.steps.filter((s) => s.actionable);
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
          className="flex-1 bg-elevated/50 rounded px-2 py-1 text-[11px] text-heading focus:outline-none focus:ring-1 focus:ring-accent"
          placeholder={t("epic.stepTitlePlaceholder")}
          value={title}
          onChange={(e) => setTitle(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && title.trim()) onSubmit(title.trim(), desc.trim() || undefined, dependsOn);
            if (e.key === "Escape") onCancel();
          }}
        />
        <button
          className="text-[10px] px-1.5 py-0.5 bg-accent text-white rounded disabled:opacity-40"
          disabled={!title.trim()}
          onClick={() => onSubmit(title.trim(), desc.trim() || undefined, dependsOn)}
        >
          {t("epic.addStep")}
        </button>
        <button className="text-[10px] text-faint hover:text-body" onClick={onCancel}>{t("epic.cancel")}</button>
      </div>
      {existingSteps.length > 0 && (
        <div className="flex items-center gap-1.5">
          <span className="text-[9px] text-faint shrink-0">{t("epic.depsOptional")}</span>
          <select
            multiple
            className="flex-1 bg-elevated/50 text-[9px] text-body rounded px-1 py-0.5 focus:outline-none h-10"
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
