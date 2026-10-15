import { useState, useEffect, Fragment } from "react";
import { useTranslation } from "react-i18next";
import { useEpics } from "../hooks/useEpics";
import { useProjectRunner, useRunnerStatus } from "../hooks/useExecutions";
import { DecisionInbox, RunningBanner } from "./ExecutionPanel";
import { GRID, sortEpics } from "./epic/helpers";
import { Metric } from "./epic/Metric";
import { RunnerHeartbeat } from "./epic/RunnerHeartbeat";
import { EpicRow } from "./epic/EpicRow";
import { NewEpicForm } from "./epic/forms";

interface EpicBoardProps {
  projectPath: string;
}

export default function EpicBoard({ projectPath }: EpicBoardProps) {
  const { t, i18n } = useTranslation();
  const { data, summary, epics, addEpic, updateEpic, deleteEpic, addStep, updateStep, deleteStep, loading, refresh: refreshEpics } = useEpics(projectPath);
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
        {loading ? (
          /* 首载期间不渲染空态，避免"暂无方案"闪现 */
          <div className="h-full" />
        ) : sorted.length === 0 && !showNewEpic ? (
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
                  onRefresh={refreshEpics}
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
                  onRefresh={refreshEpics}
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
                  onRefresh={refreshEpics}
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
                  onRefresh={refreshEpics}
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
