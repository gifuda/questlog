import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useTodos } from "../hooks/useStore";
import Column from "./Column";
import Card from "./Card";
import { updateItem } from "../hooks/useStore";

interface KanbanBoardProps {
  projectPath: string;
}

export default function KanbanBoard({ projectPath }: KanbanBoardProps) {
  const { t } = useTranslation();
  const { knowhow, todos, ideas, doneItems, tags, tagFilter, setTagFilter, refresh } = useTodos(projectPath);
  const [showDone, setShowDone] = useState(false);
  const [showNightly, setShowNightly] = useState(true);
  // 夜间完成·待确认：机器验收通过但用户尚未确认的结果——与手动完成分开，
  // 确认后才进入普通完成/归档区（review gate）。
  const nightlyItems = doneItems.filter((i) => i.auto_completed_at && !i.auto_ack_at);
  const regularDone = doneItems.filter((i) => !(i.auto_completed_at && !i.auto_ack_at));

  return (
    <div className="flex flex-col h-full">
      {/* Tag filter bar */}
      {tags.length > 0 && (
        <div className="flex items-center gap-1.5 px-3 py-2 border-b border-line overflow-x-auto">
          <span className="text-[10px] text-faint mr-1 shrink-0">{t("kanban.tagFilter")}</span>
          <button
            className={`text-[10px] px-2 py-0.5 rounded transition-colors shrink-0 ${
              !tagFilter ? "bg-accent text-white" : "text-muted hover:text-heading hover:bg-elevated"
            }`}
            onClick={() => setTagFilter(null)}
          >
            {t("kanban.all")}
          </button>
          {tags.map((tag) => (
            <button
              key={tag}
              className={`text-[10px] px-2 py-0.5 rounded transition-colors shrink-0 ${
                tagFilter === tag ? "bg-accent text-white" : "text-muted hover:text-heading hover:bg-elevated"
              }`}
              onClick={() => setTagFilter(tagFilter === tag ? null : tag)}
            >
              {tag}
            </button>
          ))}
        </div>
      )}

      {/* Three columns */}
      <div className="flex gap-4 flex-1 p-4 overflow-y-auto">
        <Column title={t("kanban.knowhowTitle")} icon="📘" type="knowhow" items={knowhow} projectPath={projectPath} onRefresh={refresh} accentColor="bg-blue-500" />
        <Column title={t("kanban.todoTitle")} icon="✅" type="todo" items={todos} projectPath={projectPath} onRefresh={refresh} accentColor="bg-green-500" />
        <Column title={t("kanban.ideaTitle")} icon="💡" type="idea" items={ideas} projectPath={projectPath} onRefresh={refresh} accentColor="bg-yellow-500" />
      </div>

      {/* Nightly-completed review gate */}
      {nightlyItems.length > 0 && (
        <div className="border-t border-line bg-blue-50/40">
          <button
            className="w-full flex items-center gap-2 px-4 py-2 text-[11px] text-blue-700 transition-colors"
            onClick={() => setShowNightly(!showNightly)}
          >
            <span className={`transition-transform ${showNightly ? "rotate-90" : ""}`}>▶</span>
            {t("kanban.nightlyReview")}
            <span className="bg-blue-100 px-1.5 py-0.5 rounded text-[10px]">{nightlyItems.length}</span>
          </button>
          {showNightly && (
            <div className="px-4 pb-3 flex flex-col gap-2 max-h-64 overflow-y-auto">
              {nightlyItems.map((item) => (
                <div key={item.id} className="flex items-start gap-2">
                  <div className="flex-1 min-w-0">
                    <Card item={item} projectPath={projectPath} onRefresh={refresh} dimmed />
                  </div>
                  <button
                    className="shrink-0 text-[10px] px-2.5 py-1.5 mt-1 rounded bg-blue-600 text-white hover:bg-blue-700 transition-colors"
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
            <span className="bg-chip px-1.5 py-0.5 rounded text-[10px]">{regularDone.length}</span>
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
