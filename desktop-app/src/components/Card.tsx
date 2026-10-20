import { useState, useEffect, useRef } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import type { TodoItem } from "../types";
import type { TodoTaskLink } from "../hooks/useFleetHealth";
import { updateItem } from "../hooks/useStore";
import { stripMd, formatDate } from "../utils";
import { priorityDot } from "../constants";
import Badge from "./Badge";
import ConfirmButton from "./ConfirmButton";

interface CardProps {
  item: TodoItem;
  projectPath: string;
  onRefresh: () => void;
  dimmed?: boolean;
  /** 夜间调度整体是否启用（false = todo_schedule 未配置）。 */
  scheduleEnabled?: boolean | null;
  /** 项目是否在夜间调度名单内；false 时 auto 徽章降级为琥珀色警示。 */
  nightlyScheduled?: boolean | null;
  /** 该待办的夜间任务链路（存在说明已被产包/执行过）。 */
  taskLink?: TodoTaskLink;
  /** 跳转到方案追踪页（查看自动生成的方案/执行记录）。 */
  onOpenEpics?: () => void;
}

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

export default function Card({ item, projectPath, onRefresh, dimmed, scheduleEnabled, nightlyScheduled, taskLink, onOpenEpics }: CardProps) {
  const { t, i18n } = useTranslation();
  const [expanded, setExpanded] = useState(false);
  const [editing, setEditing] = useState(false);
  const [editTitle, setEditTitle] = useState(item.title);
  const [editContent, setEditContent] = useState(item.content);
  const [copied, setCopied] = useState(false);
  const [showMove, setShowMove] = useState(false);
  const moveRef = useRef<HTMLDivElement>(null);
  // 夜间报告复核弹层：open 控制，text/error/loading 三态
  const [report, setReport] = useState<{ open: boolean; text?: string; error?: string; loading?: boolean }>({ open: false });

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

  /** 打开夜间执行报告弹层（只读归档，报告根在 todo-reports/<项目名>/ 下）。 */
  async function handleViewReport() {
    const ref = item.auto_result?.report_ref;
    if (!ref) return;
    setReport({ open: true, loading: true });
    const tenant = projectPath.split("/").filter(Boolean).pop() ?? "";
    try {
      const text = await invoke<string>("read_auto_report", { tenant, reportRef: ref });
      setReport({ open: true, text });
    } catch (e) {
      setReport({ open: true, error: String(e) });
    }
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

  const [saving, setSaving] = useState(false);

  async function handleSave() {
    if (saving || !editTitle.trim()) return;
    setSaving(true);
    try {
      await updateItem(projectPath, item.id, { title: editTitle.trim(), content: editContent });
      setEditing(false);
      onRefresh();
    } finally {
      setSaving(false);
    }
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
                <Badge tone="accent" className="mr-1.5">{t("card.processing")}</Badge>
              )}
              {!dimmed && item.type === "todo" && item.auto_process && !item.auto_result && (
                <Badge
                  tone={scheduleEnabled === false || nightlyScheduled === false ? "amber" : "info"}
                  className="mr-1.5"
                  title={
                    (scheduleEnabled === false
                      ? `${t("card.autoNoSchedule")} · `
                      : nightlyScheduled === false
                        ? `${t("card.autoNotScheduled")} · `
                        : "") + t("card.autoBadgeToggle")
                  }
                  onClick={() =>
                    updateItem(projectPath, item.id, { auto_process: false }).then(onRefresh)
                  }
                >
                  {t("card.autoProcess")}
                </Badge>
              )}
              {!dimmed && item.auto_result && (
                <span title={item.auto_result.reason_code ?? undefined} className="inline-flex">
                  <Badge
                    tone={item.status === "completed" && item.auto_completed_at ? "success" : "amber"}
                    className="mr-1.5"
                  >
                    {t(`card.autoOutcome.${item.auto_result.outcome}`, item.auto_result.outcome)}
                  </Badge>
                </span>
              )}
              {item.title}
            </h3>
          )}
        </div>
        {item.type === "todo" && !dimmed && (
          <span className={`w-2 h-2 rounded-full mt-1.5 shrink-0 ${priorityDot[item.priority]}`} />
        )}
      </div>

      {/* 夜间任务链路：这条待办被产包/执行后的去向，一键跳到方案追踪 */}
      {taskLink && (
        <div className="flex items-center gap-1.5 mt-1.5 text-[11px] text-faint">
          <span>🌙 {t(`card.autoState.${taskLink.state}`, taskLink.state)}</span>
          {taskLink.outcome && (
            <span>· {t(`card.autoOutcome.${taskLink.outcome}`, taskLink.outcome)}</span>
          )}
          {taskLink.has_epic && onOpenEpics && (
            <button
              className="text-accent hover:underline"
              onClick={(e) => {
                e.stopPropagation();
                onOpenEpics();
              }}
            >
              {t("card.viewPlan")} →
            </button>
          )}
          {/* dimmed（待确认/已完成区）卡片也保留取消入口——授权撤销永远可达 */}
          {dimmed && item.auto_process && (
            <button
              className="text-amber hover:underline"
              onClick={(e) => {
                e.stopPropagation();
                updateItem(projectPath, item.id, { auto_process: false }).then(onRefresh);
              }}
            >
              {t("card.autoCancelShort")}
            </button>
          )}
        </div>
      )}

      {/* 夜间复核出口：needs_review 报告的人工裁决（查看报告 / 通过 / 驳回），
          裁决后展示结论；通过走「待确认区→确认」两步，与机器 accepted 对称 */}
      {item.auto_result && item.auto_result.outcome === "needs_review" && !dimmed && (
        <div className="flex items-center gap-2 mt-1.5 text-[11px]">
          {item.auto_result.review_verdict ? (
            <span
              className={
                item.auto_result.review_verdict === "accepted" ? "text-success" : "text-amber"
              }
            >
              {item.auto_result.review_verdict === "accepted"
                ? t("card.reviewAccepted")
                : t("card.reviewRejected")}
            </span>
          ) : (
            item.status === "active" && (
              <>
                {item.auto_result.report_ref && (
                  <button
                    className="text-accent hover:underline"
                    onClick={(e) => {
                      e.stopPropagation();
                      handleViewReport();
                    }}
                  >
                    {t("card.reviewReport")}
                  </button>
                )}
                <button
                  className="text-success hover:underline"
                  onClick={(e) => {
                    e.stopPropagation();
                    updateItem(projectPath, item.id, { auto_review: "accepted" }).then(onRefresh);
                  }}
                >
                  {t("card.reviewAccept")}
                </button>
                <button
                  className="text-amber hover:underline"
                  onClick={(e) => {
                    e.stopPropagation();
                    updateItem(projectPath, item.id, { auto_review: "rejected" }).then(onRefresh);
                  }}
                >
                  {t("card.reviewReject")}
                </button>
              </>
            )
          )}
        </div>
      )}

      {/* Tags */}
      {item.tags.length > 0 && (
        <div className="flex flex-wrap gap-1 mt-2">
          {item.tags.slice(0, 3).map((tag) => (
            <span key={tag} className="text-[11px] px-1.5 py-0.5 bg-chip text-body rounded">{tag}</span>
          ))}
          {item.tags.length > 3 && (
            <span className="text-[11px] px-1.5 py-0.5 text-faint">+{item.tags.length - 3}</span>
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
              <p className="text-[13px] text-muted whitespace-pre-wrap">{stripMd(item.content)}</p>
            )
          )}
          {item.suggestion && (
            <div className="text-[13px] text-accent bg-accent-bg rounded p-2">
              💡 {stripMd(item.suggestion)}
            </div>
          )}
          <div className="flex gap-1.5 text-[11px] text-faint">
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
            className="text-[11px] px-2 py-0.5 text-muted hover:text-heading hover:bg-elevated rounded transition-colors"
            onClick={() => setEditing(!editing)}
          >
            {editing ? t("card.cancel") : t("card.edit")}
          </button>
          <button
            className="text-[11px] px-2 py-0.5 text-muted hover:text-accent hover:bg-elevated rounded transition-colors"
            onClick={handleCopyRef}
            title={t("card.copyRef")}
          >
            {copied ? t("card.copied") : t("card.ref")}
          </button>
          {item.type === "todo" && item.status === "active" && (
            item.auto_process ? (
              /* 已授权：单击即关（关闭无风险）。 */
              <button
                className="text-[11px] px-2 py-0.5 rounded transition-colors bg-accent-bg text-accent"
                onClick={() =>
                  updateItem(projectPath, item.id, { auto_process: false }).then(onRefresh)
                }
                title={t("card.autoToggleTitle")}
              >
                {t("card.autoOn")}
              </button>
            ) : (
              /* 未授权：两段确认并明示适用条件（仅只读研究类任务）。 */
              <ConfirmButton
                tone="accent"
                label={t("card.autoOff")}
                confirmLabel={t("card.autoConfirmOn")}
                onConfirm={() =>
                  updateItem(projectPath, item.id, { auto_process: true }).then(onRefresh)
                }
                title={t("card.autoToggleTitle")}
                className="text-[11px] px-2 py-0.5 rounded transition-colors text-muted hover:text-heading hover:bg-elevated"
              />
            )
          )}
          <div className="relative" ref={moveRef}>
            <button
              className="text-[11px] px-2 py-0.5 text-muted hover:text-heading hover:bg-elevated rounded transition-colors"
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
                    className="w-full text-left px-3 py-1 text-[11px] text-body hover:bg-surface transition-colors"
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
              className="text-[11px] px-2 py-0.5 text-success hover:brightness-125 hover:bg-elevated rounded transition-colors"
              onClick={() => handleStatus("completed")}
            >
              {t("card.done")}
            </button>
          )}
          {item.type === "idea" && (
            <button
              className="text-[11px] px-2 py-0.5 text-accent hover:brightness-125 hover:bg-elevated rounded transition-colors"
              onClick={() => handleStatus("converted")}
            >
              {t("card.convert")}
            </button>
          )}
          <button
            className="text-[11px] px-2 py-0.5 text-faint hover:text-danger hover:bg-elevated rounded transition-colors ml-auto"
            onClick={() => handleStatus("archived")}
          >
            {t("card.archive")}
          </button>
          {editing && (
            <button
              className="text-[11px] px-2 py-0.5 text-accent hover:brightness-125 bg-elevated rounded transition-colors ml-auto disabled:opacity-40"
              onClick={handleSave}
              disabled={saving || !editTitle.trim()}
            >
              {t("card.save")}
            </button>
          )}
        </div>
      ) : (
        <div className="flex items-center gap-1 mt-2 pt-2 border-t border-line/50">
          <button
            className="text-[11px] px-2 py-0.5 text-accent hover:brightness-125 hover:bg-elevated rounded transition-colors"
            onClick={() => handleStatus("active")}
          >
            {t("card.restore")}
          </button>
        </div>
      )}

      {/* 夜间执行报告弹层（只读） */}
      {report.open && (
        <div
          className="fixed inset-0 z-50 bg-black/40 flex items-center justify-center p-6"
          onClick={() => setReport({ open: false })}
        >
          <div
            className="max-w-3xl w-full max-h-[80vh] flex flex-col rounded-lg border border-line bg-elevated shadow-2xl"
            onClick={(e) => e.stopPropagation()}
          >
            <div className="flex items-center justify-between px-4 py-2.5 border-b border-line shrink-0">
              <span className="text-[13px] font-medium text-heading">{t("card.reportTitle")}</span>
              <button
                className="text-[11px] text-muted hover:text-heading px-2 py-0.5 rounded transition-colors"
                onClick={() => setReport({ open: false })}
              >
                {t("card.cancel")}
              </button>
            </div>
            <div className="overflow-auto px-4 py-3 text-[13px] text-body whitespace-pre-wrap break-words">
              {report.loading
                ? t("card.reportLoading")
                : report.error ?? report.text ?? ""}
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
