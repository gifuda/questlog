import { useState, useEffect, useRef } from "react";
import { useTranslation } from "react-i18next";
import type { TodoItem } from "../types";
import { updateItem } from "../hooks/useStore";
import { stripMd, formatDate } from "../utils";

interface CardProps {
  item: TodoItem;
  projectPath: string;
  onRefresh: () => void;
  dimmed?: boolean;
}

const priorityColors: Record<string, string> = {
  high: "bg-red-500",
  medium: "bg-yellow-500",
  low: "bg-gray-500",
};

/** Type label keys shared with the locales; values come from t(). */
const typeLabelKey: Record<string, string> = {
  knowhow: "card.itemTypeKnowhow",
  todo: "card.itemTypeTodo",
  idea: "card.itemTypeIdea",
};

const statusLabelKey: Record<string, string> = {
  completed: "card.statusCompleted",
  converted: "card.statusConverted",
  archived: "card.statusArchived",
};

export default function Card({ item, projectPath, onRefresh, dimmed }: CardProps) {
  const { t, i18n } = useTranslation();
  const [expanded, setExpanded] = useState(false);
  const [editing, setEditing] = useState(false);
  const [editTitle, setEditTitle] = useState(item.title);
  const [editContent, setEditContent] = useState(item.content);
  const [copied, setCopied] = useState(false);
  const [showMove, setShowMove] = useState(false);
  const moveRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!showMove) return;
    function close(e: MouseEvent) {
      if (moveRef.current && !moveRef.current.contains(e.target as Node)) {
        setShowMove(false);
      }
    }
    window.addEventListener("mousedown", close);
    return () => window.removeEventListener("mousedown", close);
  }, [showMove]);

  function handleCopyRef() {
    // Protocol text — stays in sync with the MCP server's tool descriptions
    // (mcp-server/src/index.ts), which teach the AI to recognize both prefixes.
    const typeLabel = t(typeLabelKey[item.type] ?? "card.itemTypeTodo");
    const text = t("protocol.handle", { type: typeLabel, title: item.title });
    navigator.clipboard.writeText(text).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    });
    // Mark as processing so the board shows this item is being worked on
    if (item.type === "todo" && !item.processing) {
      updateItem(projectPath, item.id, { processing: true }).then(onRefresh);
    }
  }

  async function handleStatus(status: string) {
    await updateItem(projectPath, item.id, { status: status as TodoItem["status"] });
    onRefresh();
  }

  async function handleMoveType(newType: string) {
    if (newType === item.type) {
      setShowMove(false);
      return;
    }
    // Reset status to active when changing type
    await updateItem(projectPath, item.id, {
      type: newType,
      status: "active" as TodoItem["status"],
    });
    setShowMove(false);
    onRefresh();
  }

  async function handleSave() {
    await updateItem(projectPath, item.id, { title: editTitle, content: editContent });
    setEditing(false);
    onRefresh();
  }

  const moveOptions = [
    { key: "knowhow", label: `📘 ${t("card.itemTypeKnowhow")}` },
    { key: "todo", label: `✅ ${t("card.itemTypeTodo")}` },
    { key: "idea", label: `💡 ${t("card.itemTypeIdea")}` },
  ].filter((o) => o.key !== item.type);

  return (
    <div className={`bg-surface rounded-lg p-3 border border-line hover:border-line-hover transition-colors ${dimmed ? "opacity-60" : ""}`}>
      {/* Header */}
      <div className="flex items-start justify-between gap-2">
        <div className="flex-1 min-w-0">
          {editing ? (
            <input
              className="bg-elevated text-heading text-sm w-full rounded px-2 py-1 outline-none"
              value={editTitle}
              onChange={(e) => setEditTitle(e.target.value)}
            />
          ) : (
            <h3
              className="text-sm font-medium text-heading cursor-pointer truncate"
              title={item.title}
              onClick={() => setExpanded(!expanded)}
            >
              {dimmed && statusLabelKey[item.status] && (
                <span className="text-faint mr-1">[{t(statusLabelKey[item.status])}]</span>
              )}
              {!dimmed && item.type === "todo" && item.processing && (
                <span className="text-[9px] px-1.5 py-0.5 rounded bg-accent-bg text-accent mr-1.5">{t("card.processing")}</span>
              )}
              {item.title}
            </h3>
          )}
        </div>
        {item.type === "todo" && !dimmed && (
          <span className={`w-2 h-2 rounded-full mt-1.5 shrink-0 ${priorityColors[item.priority]}`} />
        )}
      </div>

      {/* Tags */}
      {item.tags.length > 0 && (
        <div className="flex flex-wrap gap-1 mt-2">
          {item.tags.slice(0, 3).map((tag) => (
            <span key={tag} className="text-[10px] px-1.5 py-0.5 bg-chip text-body rounded">{tag}</span>
          ))}
          {item.tags.length > 3 && (
            <span className="text-[10px] px-1.5 py-0.5 text-faint">+{item.tags.length - 3}</span>
          )}
        </div>
      )}

      {/* Expanded content */}
      {expanded && (
        <div className="mt-2 space-y-2">
          {editing ? (
            <textarea
              className="bg-elevated text-body text-xs w-full rounded px-2 py-1 outline-none resize-none"
              rows={3}
              value={editContent}
              onChange={(e) => setEditContent(e.target.value)}
            />
          ) : (
            item.content && (
              <p className="text-xs text-muted whitespace-pre-wrap">{stripMd(item.content)}</p>
            )
          )}
          {item.suggestion && (
            <div className="text-xs text-accent bg-accent-bg rounded p-2">
              💡 {stripMd(item.suggestion)}
            </div>
          )}
          <div className="flex gap-1.5 text-[10px] text-faint">
            <span>{t(typeLabelKey[item.type] ?? "card.itemTypeTodo")}</span>
            <span>·</span>
            <span>{formatDate(item.created_at, i18n.resolvedLanguage ?? "zh")}</span>
          </div>
        </div>
      )}

      {/* Actions */}
      {!dimmed ? (
        <div className="flex items-center gap-1 mt-2 pt-2 border-t border-line/50">
          <button
            className="text-[10px] px-2 py-0.5 text-muted hover:text-heading hover:bg-elevated rounded transition-colors"
            onClick={() => setEditing(!editing)}
          >
            {editing ? t("card.cancel") : t("card.edit")}
          </button>
          <button
            className="text-[10px] px-2 py-0.5 text-muted hover:text-accent hover:bg-elevated rounded transition-colors"
            onClick={handleCopyRef}
            title={t("card.copyRef")}
          >
            {copied ? t("card.copied") : t("card.ref")}
          </button>
          <div className="relative" ref={moveRef}>
            <button
              className="text-[10px] px-2 py-0.5 text-muted hover:text-heading hover:bg-elevated rounded transition-colors"
              onClick={() => setShowMove(!showMove)}
              title={t("card.moveTo")}
            >
              {t("card.move")}
            </button>
            {showMove && (
              <div className="absolute bottom-full left-0 mb-1 bg-elevated border border-line rounded-lg shadow-xl py-1 z-20 min-w-[100px]">
                {moveOptions.map((o) => (
                  <button
                    key={o.key}
                    className="w-full text-left px-3 py-1 text-[10px] text-body hover:bg-surface transition-colors"
                    onClick={() => handleMoveType(o.key)}
                  >
                    {o.label}
                  </button>
                ))}
              </div>
            )}
          </div>
          {item.type === "todo" && (
            <button
              className="text-[10px] px-2 py-0.5 text-success hover:brightness-125 hover:bg-elevated rounded transition-colors"
              onClick={() => handleStatus("completed")}
            >
              {t("card.done")}
            </button>
          )}
          {item.type === "idea" && (
            <button
              className="text-[10px] px-2 py-0.5 text-accent hover:brightness-125 hover:bg-elevated rounded transition-colors"
              onClick={() => handleStatus("converted")}
            >
              {t("card.convert")}
            </button>
          )}
          <button
            className="text-[10px] px-2 py-0.5 text-faint hover:text-danger hover:bg-elevated rounded transition-colors ml-auto"
            onClick={() => handleStatus("archived")}
          >
            {t("card.archive")}
          </button>
          {editing && (
            <button
              className="text-[10px] px-2 py-0.5 text-accent hover:brightness-125 bg-elevated rounded transition-colors ml-auto"
              onClick={handleSave}
            >
              {t("card.save")}
            </button>
          )}
        </div>
      ) : (
        <div className="flex items-center gap-1 mt-2 pt-2 border-t border-line/50">
          <button
            className="text-[10px] px-2 py-0.5 text-accent hover:brightness-125 hover:bg-elevated rounded transition-colors"
            onClick={() => handleStatus("active")}
          >
            {t("card.restore")}
          </button>
        </div>
      )}
    </div>
  );
}
