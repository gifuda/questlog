import { useMemo, useState, useEffect } from "react";
import { useTranslation } from "react-i18next";
import type { TFunction } from "i18next";
import { getEpicAutoExecute, setEpicAutoExecute } from "../../hooks/useExecutions";
import { epicStatusBadge } from "../../epicConstants";
import { formatDate } from "../../utils";
import type { Epic, EpicStatus, GlobalEpic, StepStage } from "../../types/epics";
import Badge, { type BadgeTone } from "../Badge";
import ConfirmButton from "../ConfirmButton";
import { GRID, IN_PROGRESS_STAGES, epicStatusKey, type EpicPatch } from "./helpers";
import { SignalCell } from "./SignalCell";
import { StepRow } from "./StepRow";
import { EpicEditForm, NewEpicForm, StepAddForm } from "./forms";

/** 方案状态 → Badge tone（与 epicStatusBadge 的 dot/bg/text 语义就近映射）。 */
const statusTone: Record<EpicStatus, BadgeTone> = {
  draft: "muted",
  active: "accent",
  verifying: "info",
  observing: "amber",
  done: "success",
  blocked: "danger",
  paused: "muted",
};

export interface EpicRowProps {
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
  /** seal 成功后立即拉板——远端模式 watcher 不触发，10s 兜底太慢（S4）。 */
  onRefresh?: () => void;
  /** S5: 上层开关状态——三态阻断原因推导（总闸急停 > 项目信任 > 方案授权）。 */
  globalEnabled?: boolean;
  projectEnabled?: boolean;
}

export function EpicRow({ epic, depth, allEpics, lang, onUpdate, onDelete, onAddStep, onUpdateStep, onDeleteStep, onAddChild, projectPath, onRefresh, globalEnabled, projectEnabled }: EpicRowProps) {
  const { t } = useTranslation();
  const badge = epicStatusBadge[epic.status];
  const [expanded, setExpanded] = useState(false);
  const [showAddStep, setShowAddStep] = useState(false);
  const [showAddChild, setShowAddChild] = useState(false);
  const [editing, setEditing] = useState(false);
  const [pkgCopied, setPkgCopied] = useState(false);
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
        className={`${GRID} py-2.5 border-b border-line/60 cursor-pointer hover:bg-surface-dim transition-colors ${
          isDone ? "opacity-50" : isBlocked ? "opacity-70" : ""
        } ${depth === 1 ? "bg-heading/[.02]" : ""}`}
        onClick={() => setExpanded(!expanded)}
      >
        {/* 方案 */}
        <div className="flex items-center gap-2 min-w-0">
          {depth === 1 && <span className="text-faint shrink-0 w-4 text-center text-[11px]">└</span>}
          <span className={`text-faint shrink-0 w-4 text-center text-[11px] transition-transform ${expanded ? "rotate-90" : ""}`}>▶</span>
          <div className="min-w-0">
            <div className={`break-words leading-snug ${depth === 0 ? "text-sm font-semibold" : "text-[13px] font-medium"} ${isDone ? "text-faint line-through" : "text-heading"}`}>
              {/* 夜间待办自动产出的方案：标题由 producer 固定以"自动处理·"开头 */}
              {epic.title.startsWith("自动处理·") && (
                <span className="mr-1" title={t("epic.autoOrigin")}>🌙</span>
              )}
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
          <Badge tone={priorityTone(epic.priority)} className="font-mono shrink-0">
            {epic.priority}
          </Badge>
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
            <Badge
              tone="amber"
              className="shrink-0"
              title={`${t("epic.earlyClose")}: ${epic.close_reason}`}
            >
              ⚠{t("epic.earlyClose")}
            </Badge>
          )}
        </div>

        {/* 状态 */}
        <div>
          <Badge tone={statusTone[epic.status]} className="inline-flex items-center gap-1.5">
            <span className={`w-1.5 h-1.5 rounded-full ${badge.dot}`} />
            {t(epicStatusKey[epic.status])}
          </Badge>
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
        <div className={`border-b border-line/60 bg-surface-dim px-4 py-3 space-y-3 ${depth === 1 ? "pl-12" : "pl-10"}`}>
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
                  <div className="font-mono text-[11px] tracking-widest uppercase text-faint mb-1.5">{t("epic.secDetails")}</div>
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
                <div className="font-mono text-[11px] tracking-widest uppercase text-faint mb-1">{t("epic.secSteps")}</div>
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
                      projectPath={projectPath}
                      onExecute={async () => {
                        const { invoke } = await import("@tauri-apps/api/core");
                        await invoke("seal_step_for_execution", {
                          stepId: step.id,
                          projectPath: projectPath ?? "",
                        });
                        // 远端模式 watcher 不触发（板文件在 m5p），封包回写后
                        // 只有 10s 兜底轮询会刷新——seal 成功即拉一次，让
                        // "执行中"状态秒级可见（S4 延迟链的 UI 等待段）。
                        onRefresh?.();
                      }}
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
                <button className="text-accent hover:underline" onClick={(e) => {
                  e.stopPropagation();
                  copyEpicPackage(epic, projectPath, t);
                  setPkgCopied(true);
                  setTimeout(() => setPkgCopied(false), 1500);
                }}>
                  {pkgCopied ? t("epic.copied") : t("epic.copyPackage")}
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
// StepRow — 简化三态（⬜🔄✅/⚠️）+ 白盒详情可展开 + ▶ 启动 + ⚠️ 内联处置
// ══════════════════════════════════════════════

/** 优先级徽章 tone（P0 红 / P1 蓝 / P2 黄 / P3 灰，语义就近映射 Badge）。 */
export function priorityTone(priority: string): BadgeTone {
  switch ((priority || "").toUpperCase()) {
    case "P0": return "danger";
    case "P1": return "accent";
    case "P2": return "amber";
    default: return "muted";
  }
}

// ══════════════════════════════════════════════
// copyEpicRef — copies the "continue plan" reference to the clipboard.
// Outputs the actionable-step set; pasting it into an AI session lets the
// assistant call list_epics to match the title and continue.
// Protocol text stays in sync with mcp-server/src/index.ts tool descriptions.
// ══════════════════════════════════════════════
export function copyEpicRef(epic: Epic, t: TFunction) {
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
// copyEpicPackage — copies the FULL handoff package: everything a fresh
// conversation needs to continue the plan without re-discovery (epic info,
// per-step state incl. sealed-package summaries, continuation protocol,
// MCP project_dir addressing). Keeps the short "继续方案" line for
// protocol compatibility with mcp-server tool descriptions.
// ══════════════════════════════════════════════
export function copyEpicPackage(epic: Epic, projectPath: string | undefined, t: TFunction) {
  const g = epic as GlobalEpic;
  const projPath = g.project_path || projectPath || "";
  const projName = g.project || "";
  // Clip long free-text fields; defuse i18next interpolation markers.
  const clip = (s: string | null | undefined, n = 600) => {
    const v = (s || "").trim().replace(/\{\{/g, "（（");
    if (!v) return "—";
    return v.length > n ? v.slice(0, n) + "…" : v;
  };
  const now = new Date();
  const pad = (x: number) => String(x).padStart(2, "0");
  const time = `${now.getFullYear()}-${pad(now.getMonth() + 1)}-${pad(now.getDate())} ${pad(now.getHours())}:${pad(now.getMinutes())}`;

  const lines: string[] = [];
  lines.push(t("protocol.pkg.title", { title: epic.title }));
  lines.push(t("protocol.pkg.exportedAt", { time }));
  lines.push("");
  lines.push(t("protocol.pkg.info"));
  lines.push(t("protocol.pkg.idLine", {
    id: epic.id, status: epic.status_label, priority: epic.priority,
    progress: epic.progress, steps: epic.step_count,
  }));
  if (projName || projPath) {
    lines.push(t("protocol.pkg.projectLine", { project: projName || "—", path: projPath }));
  }
  lines.push(t("protocol.pkg.desc", { desc: clip(epic.description) }));
  lines.push(t("protocol.pkg.acceptance", { text: clip(epic.acceptance, 400) }));
  lines.push(t("protocol.pkg.depends", {
    names: epic.depends_on_titles?.length ? epic.depends_on_titles.join("、") : "—",
  }));
  lines.push(t("protocol.pkg.links", {
    doc: clip(epic.links?.doc, 200),
    branch: clip(epic.links?.branch, 120),
    note: clip(epic.links?.note, 300),
  }));
  lines.push("");
  lines.push(t("protocol.pkg.steps"));
  const steps = [...epic.steps].sort(
    (a, b) => a.order - b.order || a.created_at.localeCompare(b.created_at),
  );
  steps.forEach((s, i) => {
    const mark = s.actionable
      ? t("protocol.pkg.stepReady")
      : s.depends_on_titles?.length
        ? t("protocol.pkg.stepPending", { names: s.depends_on_titles.join("、") })
        : "";
    lines.push(t("protocol.pkg.stepLine", { no: i + 1, title: s.title, stage: s.stage_label }) + mark);
    const d = (s.description || "").trim();
    if (d) lines.push(t("protocol.pkg.stepDesc", { desc: clip(d, 300) }));
    if (s.package) {
      lines.push(t("protocol.pkg.stepPkg", {
        status: s.package.status,
        pid: s.package.package_id.slice(0, 8),
        objective: clip(s.package.objective, 200),
      }));
    }
  });
  if (epic.children?.length) {
    lines.push("");
    lines.push(t("protocol.pkg.children"));
    epic.children.forEach((c) =>
      lines.push(t("protocol.pkg.childLine", {
        title: c.title, status: c.status_label, progress: c.progress, steps: c.step_count,
      })),
    );
  }
  lines.push("");
  lines.push(t("protocol.pkg.protocol"));
  for (let i = 1; i <= 5; i++) lines.push(t(`protocol.pkg.rule${i}`));
  lines.push("");
  lines.push(t("protocol.continuePlan", { title: epic.title }));
  navigator.clipboard.writeText(lines.join("\n")).then(() => {}).catch(() => {});
}

// ══════════════════════════════════════════════
