import { useState } from "react";
import { useTranslation } from "react-i18next";
import type { TodoItem, ItemType } from "../types";
import { addItem } from "../hooks/useStore";
import type { TodoTaskLink } from "../hooks/useFleetHealth";
import Card from "./Card";

interface ColumnProps {
  title: string;
  icon: string;
  type: ItemType;
  items: TodoItem[];
  projectPath: string;
  onRefresh: () => void;
  accentColor: string;
  /** 夜间调度整体是否启用。 */
  scheduleEnabled?: boolean | null;
  /** 该项目是否在夜间调度名单内（仅 todo 卡片用到）。 */
  nightlyScheduled?: boolean | null;
  /** 待办 id → 夜间任务链路。 */
  taskLinks?: Record<string, TodoTaskLink>;
  onOpenEpics?: () => void;
}

export default function Column({ title, icon, type, items, projectPath, onRefresh, accentColor, scheduleEnabled, nightlyScheduled, taskLinks, onOpenEpics }: ColumnProps) {
  const { t } = useTranslation();
  const [adding, setAdding] = useState(false);
  const [titleInput, setTitleInput] = useState("");
  const [contentInput, setContentInput] = useState("");
  const [busy, setBusy] = useState(false);

  async function handleSubmit() {
    if (!titleInput.trim()) return;
    setBusy(true);
    try {
      await addItem(projectPath, type, titleInput.trim(), contentInput.trim() || undefined);
      setTitleInput("");
      setContentInput("");
      setAdding(false);
      onRefresh();
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="flex flex-col min-w-[220px] flex-1">
      <div className="flex items-center gap-2 mb-3 px-1">
        <span>{icon}</span>
        <h2 className="text-sm font-semibold text-heading">{title}</h2>
        <span className="text-xs text-faint bg-surface px-1.5 py-0.5 rounded tabular-nums">{items.length}</span>
        <div className={`h-0.5 flex-1 rounded ${accentColor} opacity-40`} />
        <button
          className="text-xs text-faint hover:text-heading hover:bg-elevated w-5 h-5 rounded flex items-center justify-center transition-colors"
          onClick={() => setAdding(!adding)}
          title={t("column.add")}
        >
          {adding ? "✕" : "+"}
        </button>
      </div>

      <div className="flex flex-col gap-2 flex-1 overflow-y-auto">
        {/* Inline add form */}
        {adding && (
          <div className="bg-surface rounded-lg p-3 border border-accent/40 space-y-2">
            <input
              className="bg-elevated text-heading text-sm w-full rounded px-2 py-1 outline-none placeholder:text-faint"
              placeholder={t("column.titlePlaceholder")}
              autoFocus
              value={titleInput}
              onChange={(e) => setTitleInput(e.target.value)}
              onKeyDown={(e) => { if (e.key === "Enter" && !e.shiftKey) handleSubmit(); }}
            />
            <textarea
              className="bg-elevated text-body text-xs w-full rounded px-2 py-1 outline-none resize-none placeholder:text-faint"
              placeholder={t("column.detailPlaceholder")}
              rows={2}
              value={contentInput}
              onChange={(e) => setContentInput(e.target.value)}
              onKeyDown={(e) => { if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) handleSubmit(); }}
            />
            <div className="flex justify-end gap-1">
              <button
                className="text-[11px] px-2 py-0.5 text-faint hover:text-heading rounded transition-colors"
                onClick={() => { setAdding(false); setTitleInput(""); setContentInput(""); }}
              >
                {t("column.cancel")}
              </button>
              <button
                className="text-[11px] px-2.5 py-0.5 bg-accent text-on-accent rounded hover:opacity-90 transition-colors disabled:opacity-40"
                onClick={handleSubmit}
                disabled={busy || !titleInput.trim()}
              >
                {t("column.add")}
              </button>
            </div>
          </div>
        )}

        {items.length === 0 && !adding && (
          <p className="text-xs text-faint text-center py-4">{t("column.empty")}</p>
        )}
        {items.map((item) => (
          <Card key={item.id} item={item} projectPath={projectPath} onRefresh={onRefresh} scheduleEnabled={scheduleEnabled} nightlyScheduled={nightlyScheduled} taskLink={taskLinks?.[item.id]} onOpenEpics={onOpenEpics} />
        ))}
      </div>
    </div>
  );
}
