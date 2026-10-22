import { useState, useCallback } from "react";
import { useTranslation } from "react-i18next";
import { useTodos } from "../hooks/useStore";
import { useTodoStatus } from "../hooks/useFleetHealth";
import Column from "./Column";
import Card from "./Card";
import { NightlyDigestLine } from "./FleetHealth";
import { updateItem } from "../hooks/useStore";

interface KanbanBoardProps {
  projectPath: string;
  /** 跳转到方案追踪页（卡片上的夜间任务链路入口）。 */
  onOpenEpics?: () => void;
}

export default function KanbanBoard({ projectPath, onOpenEpics }: KanbanBoardProps) {
  const { t } = useTranslation();
  const { knowhow, todos, ideas, doneItems, tags, tagFilter, setTagFilter, refresh, loading } = useTodos(projectPath);
  const { scheduled: nightlyScheduled, links: taskLinks, scheduleEnabled } = useTodoStatus(projectPath);
  const [showDone, setShowDone] = useState(false);
  const [showNightly, setShowNightly] = useState(true);
  // 夜间完成·待确认：机器验收通过但用户尚未确认的结果——与手动完成分开，
  // 确认后才进入普通完成/归档区（review gate）。
  const nightlyItems = doneItems.filter((i) => i.auto_completed_at && !i.auto_ack_at);
  const regularDone = doneItems.filter((i) => !(i.auto_completed_at && !i.auto_ack_at));

  // 最小 error 分支：useTodos 不暴露 error（hook 内部 refresh 失败为未处理
  // rejection），这里只捕获本组件发起的刷新（重试）。首载失败仍由 hook 吞掉
  // （loading 停留 true）——完整修复需 hook 暴露 error，见施工报告。
  const [loadError, setLoadError] = useState(false);
  const refreshBoard = useCallback(async () => {
    try {
      await refresh();
      setLoadError(false);
    } catch {
      setLoadError(true);
    }
  }, [refresh]);
  const boardEmpty =
    knowhow.length === 0 && todos.length === 0 && ideas.length === 0 && doneItems.length === 0;

  return (
    <div className="flex flex-col h-full">
      {/* Tag filter bar */}
      {tags.length > 0 && (
        <div className="flex items-center gap-1.5 px-3 py-2 border-b border-line overflow-x-auto">
          <span className="text-[11px] text-faint mr-1 shrink-0">{t("kanban.tagFilter")}</span>
          <button
            className={`text-[11px] px-2 py-0.5 rounded transition-colors shrink-0 ${
              !tagFilter ? "bg-accent text-on-accent" : "text-muted hover:text-heading hover:bg-elevated"
            }`}
            onClick={() => setTagFilter(null)}
          >
            {t("kanban.all")}
          </button>
          {tags.map((tag) => (
            <button
              key={tag}
              className={`text-[11px] px-2 py-0.5 rounded transition-colors shrink-0 ${
                tagFilter === tag ? "bg-accent text-on-accent" : "text-muted hover:text-heading hover:bg-elevated"
              }`}
              onClick={() => setTagFilter(tagFilter === tag ? null : tag)}
            >
              {tag}
            </button>
          ))}
        </div>
      )}

      {/* Three columns — 首载期间不渲染空态，避免"暂无"闪现 */}
      {loadError && boardEmpty ? (
        /* 刷新失败且无数据可展示：居中错误提示 + 重试（参照 InfraBoard 降级面板） */
        <div className="flex-1 flex flex-col items-center justify-center gap-3">
          <span className="text-3xl">📋</span>
          <span className="text-[13px] text-danger">{t("kanban.loadError")}</span>
          <button className="text-[13px] text-accent hover:underline" onClick={refreshBoard}>
            {t("kanban.retry")}
          </button>
        </div>
      ) : loading ? (
        <div className="flex-1" />
      ) : (
      <div className="flex gap-4 flex-1 p-4 overflow-y-auto">
        <Column title={t("kanban.knowhowTitle")} icon="📘" type="knowhow" items={knowhow} projectPath={projectPath} onRefresh={refresh} accentColor="bg-info" scheduleEnabled={scheduleEnabled} nightlyScheduled={nightlyScheduled} taskLinks={taskLinks} onOpenEpics={onOpenEpics} />
        <Column title={t("kanban.todoTitle")} icon="✅" type="todo" items={todos} projectPath={projectPath} onRefresh={refresh} accentColor="bg-success" scheduleEnabled={scheduleEnabled} nightlyScheduled={nightlyScheduled} taskLinks={taskLinks} onOpenEpics={onOpenEpics} />
        <Column title={t("kanban.ideaTitle")} icon="💡" type="idea" items={ideas} projectPath={projectPath} onRefresh={refresh} accentColor="bg-amber" scheduleEnabled={scheduleEnabled} nightlyScheduled={nightlyScheduled} taskLinks={taskLinks} onOpenEpics={onOpenEpics} />
      </div>
      )}

      {/* Nightly-completed review gate */}
      {nightlyItems.length > 0 && (
        <div className="border-t border-line bg-accent-bg">
          <button
            className="w-full flex items-center gap-2 px-4 py-2 text-[11px] text-accent transition-colors"
            onClick={() => setShowNightly(!showNightly)}
          >
            <span className={`transition-transform ${showNightly ? "rotate-90" : ""}`}>▶</span>
            {t("kanban.nightlyReview")}
            <span className="bg-chip px-1.5 py-0.5 rounded text-[11px] tabular-nums">{nightlyItems.length}</span>
            <span className="ml-auto" role="presentation" onClick={(e) => e.stopPropagation()}>
              <NightlyDigestLine />
            </span>
          </button>
          {showNightly && (
            <div className="px-4 pb-3 flex flex-col gap-2 max-h-64 overflow-y-auto">
              {nightlyItems.map((item) => (
                <div key={item.id} className="flex items-start gap-2">
                  <div className="flex-1 min-w-0">
                    <Card item={item} projectPath={projectPath} onRefresh={refresh} dimmed scheduleEnabled={scheduleEnabled} taskLink={taskLinks[item.id]} onOpenEpics={onOpenEpics} />
                  </div>
                  <button
                    className="shrink-0 text-[11px] px-2.5 py-1.5 mt-1 rounded bg-accent text-on-accent hover:opacity-90 transition-colors"
                    onClick={() => updateItem(projectPath, item.id, { auto_ack: true }).then(refresh)}
                    title={t("kanban.ackNightlyTitle")}
                  >
                    {t("kanban.ackNightly")}
                  </button>
                </div>
              ))}
            </div>
          )}
        </div>
      )}

      {/* Completed / Archived toggle */}
      {regularDone.length > 0 && (
        <div className="border-t border-line">
          <button
            className="w-full flex items-center gap-2 px-4 py-2 text-[11px] text-faint hover:text-body transition-colors"
            onClick={() => setShowDone(!showDone)}
          >
            <span className={`transition-transform ${showDone ? "rotate-90" : ""}`}>▶</span>
            {t("kanban.doneArchive")}
            <span className="bg-chip px-1.5 py-0.5 rounded text-[11px] tabular-nums">{regularDone.length}</span>
          </button>
          {showDone && (
            <div className="px-4 pb-3 flex flex-col gap-2 max-h-48 overflow-y-auto">
              {regularDone.map((item) => (
                <Card key={item.id} item={item} projectPath={projectPath} onRefresh={refresh} dimmed />
              ))}
            </div>
          )}
        </div>
      )}
    </div>
  );
}
