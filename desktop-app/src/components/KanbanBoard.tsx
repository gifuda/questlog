import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useTodos } from "../hooks/useStore";
import Column from "./Column";
import Card from "./Card";

interface KanbanBoardProps {
  projectPath: string;
}

export default function KanbanBoard({ projectPath }: KanbanBoardProps) {
  const { t } = useTranslation();
  const { knowhow, todos, ideas, doneItems, tags, tagFilter, setTagFilter, refresh } = useTodos(projectPath);
  const [showDone, setShowDone] = useState(false);

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

      {/* Completed / Archived toggle */}
      {doneItems.length > 0 && (
        <div className="border-t border-line">
          <button
            className="w-full flex items-center gap-2 px-4 py-2 text-[11px] text-faint hover:text-body transition-colors"
            onClick={() => setShowDone(!showDone)}
          >
            <span className={`transition-transform ${showDone ? "rotate-90" : ""}`}>▶</span>
            {t("kanban.doneArchive")}
            <span className="bg-chip px-1.5 py-0.5 rounded text-[10px]">{doneItems.length}</span>
          </button>
          {showDone && (
            <div className="px-4 pb-3 flex flex-col gap-2 max-h-48 overflow-y-auto">
              {doneItems.map((item) => (
                <Card key={item.id} item={item} projectPath={projectPath} onRefresh={refresh} dimmed />
              ))}
            </div>
          )}
        </div>
      )}
    </div>
  );
}
