import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { useTranslation } from "react-i18next";
import Badge from "./Badge";
import ConfirmButton from "./ConfirmButton";
import UndoBar from "./UndoBar";
import { useUndo } from "../hooks/useUndo";
import { typeIcons, priorityDot, typeOrder, ARCHIVE_DAYS } from "../constants";
import { formatDate } from "../utils";
import type { ArbBoardFile, ArbBrief } from "../types/arb";
import type { FleetHealth } from "../hooks/useFleetHealth";

/** i18n keys for item type labels in the popup (mirrors Card.tsx). */
const typeLabelKey: Record<string, string> = {
  knowhow: "card.itemTypeKnowhow",
  todo: "card.itemTypeTodo",
  idea: "card.itemTypeIdea",
};

/* ---------- types ---------- */

interface ProjectSummary {
  name: string;
  path: string;
  counts: { knowhow: number; todo: number; idea: number };
  last_active_at: string;
}

interface ItemSummary {
  id: string;
  type: string;
  title: string;
  priority: string;
  updated_at: string;
  processing?: boolean;
  project?: string;
  project_path?: string;
}

interface ProjectDetail {
  project: string;
  project_path: string;
  knowhow: ItemSummary[];
  todo: ItemSummary[];
  idea: ItemSummary[];
}

/**
 * get_fleet_health 的托盘读取形状。pending_ack 由 Rust 侧提供（review gate 计数）；
 * FleetHealth 接口尚未声明该字段前按可选容错——缺失/非数字一律按 0（D5 决断总数不因它崩）。
 */
type TrayFleetHealth = FleetHealth & {
  pending_ack?: { count: number } | null;
};

/** 托盘晨报速览数据。任一数据源失败 → 对应字段 null/0，行置灰，绝不白屏。 */
interface BriefingData {
  /** GROS 晨报段（契约 v4）；null = 未产出/读取失败 */
  brief: ArbBrief | null;
  /** 待处置套利决策数（decisions 数组长度） */
  decisionCount: number;
  /** 夜间 digest；null = 昨夜无任务 */
  digest: FleetHealth["digest"];
  /** 夜间待确认（review gate）计数 */
  pendingAck: number;
}

/* ---------- main component ---------- */

type View =
  | { kind: "briefing" }
  | { kind: "projects" }
  | { kind: "all" }
  | { kind: "project"; data: ProjectDetail };

export default function TimelinePanel() {
  const { t } = useTranslation();
  const [view, setView] = useState<View>({ kind: "briefing" });
  const [projects, setProjects] = useState<ProjectSummary[]>([]);
  const [allItems, setAllItems] = useState<ItemSummary[]>([]);
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set());
  const [briefing, setBriefing] = useState<BriefingData | null>(null);
  // 首载门闩：数据未返回前显示加载态，避免空列表闪现"暂无"（参考 KanbanBoard 首载门闩）。
  const [loading, setLoading] = useState(true);

  const refreshProjects = useCallback(async () => {
    try {
      const list = await invoke<ProjectSummary[]>("get_projects");
      setProjects(list);
    } catch {
      // invoke 失败 → 落空数据，由各视图的空态渲染，不白屏。
      setProjects([]);
    } finally {
      setLoading(false);
    }
  }, []);

  const refreshAll = useCallback(async () => {
    try {
      const list = await invoke<ItemSummary[]>("get_all_active");
      setAllItems(list);
    } catch {
      setAllItems([]);
    }
  }, []);

  /**
   * 晨报速览数据（D5）：两个只读源独立降级——get_arb_board 失败只灰晨报/决策行，
   * get_fleet_health 失败只灰夜间行，互不拖垮，也绝不抛出白屏。
   */
  const refreshBriefing = useCallback(async () => {
    const [arb, fleet] = await Promise.allSettled([
      invoke<ArbBoardFile>("get_arb_board"),
      invoke<TrayFleetHealth>("get_fleet_health"),
    ]);
    const arbFile = arb.status === "fulfilled" ? arb.value : null;
    const fleetHealth = fleet.status === "fulfilled" ? fleet.value : null;
    setBriefing({
      brief: arbFile?.brief ?? null,
      decisionCount: Array.isArray(arbFile?.decisions) ? arbFile.decisions.length : 0,
      digest: fleetHealth?.digest ?? null,
      pendingAck:
        typeof fleetHealth?.pending_ack?.count === "number"
          ? fleetHealth.pending_ack.count
          : 0,
    });
  }, []);

  useEffect(() => {
    refreshProjects();
  }, [refreshProjects]);

  useEffect(() => {
    refreshBriefing();
    const un1 = listen("arb-changed", () => {
      refreshBriefing();
    });
    const un2 = listen("todos-changed", () => {
      refreshBriefing();
    });
    return () => {
      un1.then((f) => f());
      un2.then((f) => f());
    };
  }, [refreshBriefing]);

  useEffect(() => {
    const unlisten = listen<string>("todos-changed", () => {
      refreshProjects();
      const openProjectPath = view.kind === "project" ? view.data?.project_path : undefined;
      if (view.kind === "all") {
        refreshAll();
      } else if (openProjectPath) {
        // Re-fetch the open project's detail so changes (e.g. processing flag)
        // appear immediately
        invoke<ProjectDetail>("get_project_items", { projectPath: openProjectPath })
          .then((data) => setView({ kind: "project", data }))
          .catch(() => {});
      }
    });
    const unlisten2 = listen("projects-changed", () => {
      refreshProjects();
    });
    return () => {
      unlisten.then((f) => f());
      unlisten2.then((f) => f());
    };
  }, [refreshProjects, refreshAll, view.kind, view.kind === "project" ? view.data?.project_path : undefined]);

  function openProject(projectPath: string) {
    invoke<ProjectDetail>("get_project_items", { projectPath })
      .then((data) => setView({ kind: "project", data }))
      .catch(() => {
        setView({ kind: "project", data: { project: "", project_path: projectPath, knowhow: [], todo: [], idea: [] } });
      });
  }

  function openAll() {
    refreshAll();
    setView({ kind: "all" });
  }

  function toggleCollapse(type: string) {
    setCollapsed((prev) => {
      const next = new Set(prev);
      if (next.has(type)) next.delete(type);
      else next.add(type);
      return next;
    });
  }

  function openInMain() {
    // navigate_to_project was never a registered command (silent dead link);
    // show_main_window is registered — reveal the main window instead.
    invoke("show_main_window").catch(() => {});
    getCurrentWindow().hide();
  }

  /** 托盘行 → 主窗（D5：处置动作全部留在主窗，弹窗只读）。 */
  function openMain() {
    invoke("show_main_window").catch(() => {});
    getCurrentWindow().hide();
  }

  const totalActive = projects.reduce(
    (s, p) => s + p.counts.knowhow + p.counts.todo + p.counts.idea,
    0
  );

  const { undoInfo, trigger: triggerUndo, undo: doUndo, dismiss: dismissUndo } = useUndo();

  async function handleRemoveProject(name: string) {
    const [removedName, removedPath] = await invoke<[string, string]>("remove_project", { name });
    triggerUndo({ name: removedName, path: removedPath });
    refreshProjects();
  }

  async function handleUndo() {
    await doUndo();
    refreshProjects();
  }

  return (
    <div className="flex flex-col bg-page text-heading text-xs rounded-[16px] overflow-hidden" style={{ maxHeight: "33vh" }}>
      {/* ---- header（晨报速览自带头部；旧视图沿用旧 Header） ---- */}
      {view.kind !== "briefing" && (
        <Header view={view} totalActive={totalActive} goBack={() => setView({ kind: "briefing" })} />
      )}

      {/* ---- undo bar ---- */}
      {undoInfo && (
        <UndoBar name={undoInfo.name} onUndo={handleUndo} onDismiss={dismissUndo} edge="top" />
      )}

      {/* ---- body ---- */}
      <div className="flex-1 overflow-y-auto min-h-0">
        {loading ? (
          <p className="text-[13px] text-muted text-center py-8">{t("tray.loading")}</p>
        ) : (
          <>
            {view.kind === "briefing" && (
              <BriefingView
                data={briefing}
                projects={projects}
                totalActive={totalActive}
                onOpenMain={openMain}
                onProjects={() => setView({ kind: "projects" })}
              />
            )}
            {view.kind === "projects" && (
              <ProjectList projects={projects} onSelect={openProject} onAll={openAll} totalActive={totalActive} onRemove={handleRemoveProject} />
            )}
            {view.kind === "all" && (
              <AllItems items={allItems} onNavigate={openInMain} />
            )}
            {view.kind === "project" && (
              <ProjectDetailList
                data={view.data}
                collapsed={collapsed}
                onToggle={toggleCollapse}
                onNavigate={openInMain}
              />
            )}
          </>
        )}
      </div>
    </div>
  );
}

/* ---------- briefing view（D5 晨报速览，默认视图） ---------- */

function BriefingView({
  data,
  projects,
  totalActive,
  onOpenMain,
  onProjects,
}: {
  data: BriefingData | null;
  projects: ProjectSummary[];
  totalActive: number;
  onOpenMain: () => void;
  onProjects: () => void;
}) {
  const { t } = useTranslation();
  const brief = data?.brief ?? null;
  const digest = data?.digest ?? null;
  const decisionCount = data?.decisionCount ?? 0;
  const pendingAck = data?.pendingAck ?? 0;
  const pendingTotal = decisionCount + pendingAck;
  const projectNames = projects.map((p) => p.name).join(" · ");

  return (
    <div>
      {/* ---- head：☀ 晨报速览 + 待决断总数（大数字，琥珀） ---- */}
      <div className="flex items-center gap-2 px-3 py-2 border-b border-line">
        <span className="text-[13px]">☀</span>
        <span className="text-[12px] font-semibold text-heading">{t("tray.title")}</span>
        <span className="font-mono text-[16px] font-bold text-amber tabular-nums">
          {pendingTotal}
        </span>
        <span className="flex-1 min-w-0 truncate text-[11px] text-faint">
          {t("tray.pendingUnit")}
        </span>
        <button
          onClick={onOpenMain}
          className="text-[11px] text-faint hover:text-heading"
          title={t("timeline.showMain")}
        >
          ⊞
        </button>
        <ConfirmButton
          label="✕"
          confirmLabel="✕"
          onConfirm={() => invoke("quit_app").catch(() => {})}
          className="text-[11px] text-faint hover:text-danger"
          title={t("timeline.quit")}
        />
      </div>

      {/* 📰 套利晨报：headline + brief_date；GROS 缺失 → 置灰等待 */}
      <TrayRow
        icon="📰"
        label={t("tray.briefRow")}
        summary={brief ? (brief.headline ?? "—") : t("tray.briefWaiting")}
        right={brief ? (brief.brief_date ?? "—") : "—"}
        dimmed={!brief}
        onClick={onOpenMain}
      />

      {/* 🌙 夜间管线：digest 摘要 + 日期；昨夜无任务 → 置灰 */}
      <TrayRow
        icon="🌙"
        label={t("tray.nightRow")}
        summary={
          digest && digest.total > 0
            ? t("tray.digestLine", {
                total: digest.total,
                accepted: digest.accepted,
                review: digest.review,
                failed: digest.failed,
              })
            : t("tray.noDigest")
        }
        right={digest ? digest.date : "—"}
        dimmed={!digest || digest.total === 0}
        onClick={onOpenMain}
      />

      {/* ☑ 套利决策 → 主窗套利页处置 */}
      <TrayRow
        icon="☑"
        label={t("tray.decisionsRow")}
        summary=""
        right={decisionCount > 0 ? `${decisionCount} →` : "0"}
        dimmed={decisionCount === 0}
        onClick={onOpenMain}
      />

      {/* 🔵 夜间待确认 → 主窗待确认区处置 */}
      <TrayRow
        icon="🔵"
        label={t("tray.pendingAckRow")}
        summary=""
        right={pendingAck > 0 ? `${pendingAck} →` : "0"}
        dimmed={pendingAck === 0}
        onClick={onOpenMain}
      />

      {/* 📁 各项目在办一行汇总 → 托盘内项目列表 */}
      <TrayRow
        icon="📁"
        label=""
        summary={projectNames || t("tray.noProjects")}
        right={t("tray.activeCount", { count: totalActive })}
        dimmed={projects.length === 0}
        last
        onClick={onProjects}
      />

      {/* 底部小字入口：切回项目/时间线旧视图 */}
      <button
        className="w-full text-left px-3 py-2 text-[11px] text-faint hover:text-accent transition-colors"
        onClick={onProjects}
      >
        {t("tray.oldView")}
      </button>
    </div>
  );
}

/** 晨报速览单行（照抄效果图 .t-row 结构：图标 · 标签 · 摘要 · 右侧 mono）。 */
function TrayRow({
  icon,
  label,
  summary,
  right,
  dimmed = false,
  last = false,
  onClick,
}: {
  icon: string;
  /** 空字符串 = 无标签（📁 行直接放项目名摘要） */
  label: string;
  summary: string;
  right: string;
  dimmed?: boolean;
  last?: boolean;
  onClick: () => void;
}) {
  return (
    <button
      className={`w-full flex items-center gap-2 px-3.5 py-1.5 text-left text-[11px] hover:bg-surface transition-colors cursor-pointer ${
        last ? "" : "border-b"
      } border-line ${dimmed ? "opacity-50" : ""}`}
      onClick={onClick}
    >
      <span className="shrink-0">{icon}</span>
      {label && <span className="shrink-0 text-muted">{label}</span>}
      <span className={`flex-1 min-w-0 truncate ${label ? "text-body" : "text-muted"}`}>
        {summary}
      </span>
      <span className="shrink-0 font-mono text-faint tabular-nums">{right}</span>
    </button>
  );
}

/* ---------- header（旧视图：项目列表 / 全部活跃 / 项目详情） ---------- */

function Header({
  view,
  totalActive,
  goBack,
}: {
  view: View;
  totalActive: number;
  goBack: () => void;
}) {
  const { t } = useTranslation();
  const title =
    view.kind === "all"
      ? t("timeline.allActive")
      : view.kind === "project"
        ? view.data.project
        : t("timeline.projectList");

  function showMain() {
    invoke("show_main_window").catch(() => {});
  }

  return (
    <div className="flex items-center gap-2 px-3 py-2 border-b border-line">
      <button onClick={goBack} className="text-accent hover:opacity-70 transition-opacity">
        ←
      </button>
      <span className="font-bold flex-1 truncate">{title}</span>
      <span className="text-[11px] text-faint">{t("timeline.count", { total: totalActive })}</span>
      <button onClick={showMain} className="text-[11px] text-faint hover:text-heading" title={t("timeline.showMain")}>⊞</button>
      <ConfirmButton
        label="✕"
        confirmLabel="✕"
        onConfirm={() => invoke("quit_app").catch(() => {})}
        className="text-[11px] text-faint hover:text-danger"
        title={t("timeline.quit")}
      />
    </div>
  );
}

/* ---------- level 1: project list ---------- */

function ProjectList({
  projects,
  onSelect,
  onAll,
  totalActive,
  onRemove,
}: {
  projects: ProjectSummary[];
  onSelect: (path: string) => void;
  onAll: () => void;
  totalActive: number;
  onRemove: (name: string) => void;
}) {
  const now = Date.now();
  const cutoff = now - ARCHIVE_DAYS * 24 * 60 * 60 * 1000;

  const active: ProjectSummary[] = [];
  const inactive: ProjectSummary[] = [];

  for (const p of projects) {
    const ts = p.last_active_at ? new Date(p.last_active_at).getTime() : 0;
    (ts >= cutoff ? active : inactive).push(p);
  }

  const { t } = useTranslation();
  const [showInactive, setShowInactive] = useState(false);

  function handleRemove(name: string) {
    onRemove(name);
  }

  return (
    <div>
      {/* all-active entry */}
      <button
        className="w-full text-left px-3 py-2 border-b border-line hover:bg-surface transition-colors"
        onClick={onAll}
      >
        <div className="flex items-center gap-2">
          <span className="text-xs">📋</span>
          <span className="flex-1">{t("timeline.allActive")}</span>
          <span className="text-[11px] text-faint">{totalActive}</span>
        </div>
      </button>
      {projects.length === 0 && (
        <p className="text-faint text-center py-8">{t("tray.noProjects")}</p>
      )}
      {active.map((p) => (
        <ProjectRow key={p.path} project={p} onSelect={onSelect} onRemove={handleRemove} />
      ))}
      {inactive.length > 0 && (
        <>
          <button
            className="w-full text-left px-3 py-2 border-b border-line/30 bg-surface-dim hover:bg-surface transition-colors"
            onClick={() => setShowInactive((v) => !v)}
          >
            <span className="text-[11px] text-faint">
              {showInactive ? "▼" : "▶"} {t("timeline.inactive", { count: inactive.length })}
            </span>
          </button>
          {showInactive && inactive.map((p) => (
            <ProjectRow key={p.path} project={p} onSelect={onSelect} onRemove={handleRemove} dimmed />
          ))}
        </>
      )}
    </div>
  );
}

/* ---------- project row ---------- */

function ProjectRow({
  project,
  onSelect,
  onRemove,
  dimmed = false,
}: {
  project: ProjectSummary;
  onSelect: (path: string) => void;
  onRemove: (name: string) => void;
  dimmed?: boolean;
}) {
  const { t } = useTranslation();
  const total = project.counts.knowhow + project.counts.todo + project.counts.idea;
  return (
    <div
      className={`group flex items-center px-3 py-2 border-b border-line/50 hover:bg-surface transition-colors cursor-pointer ${dimmed ? "opacity-50" : ""}`}
      onClick={() => onSelect(project.path)}
    >
      <div className="flex items-center gap-2 flex-wrap flex-1 min-w-0">
        <span className="truncate text-heading">{project.name}</span>
        <span className="text-[11px] text-faint shrink-0">
          📘{project.counts.knowhow} ✅{project.counts.todo} 💡{project.counts.idea}
        </span>
        <span className="text-[11px] text-faint shrink-0">{total}</span>
      </div>
      <ConfirmButton
        label="✕"
        confirmLabel={t("timeline.confirm")}
        onConfirm={() => onRemove(project.name)}
        className="text-[11px] text-faint hover:text-danger transition-colors shrink-0 ml-1"
        title={t("timeline.removeProject")}
      />
    </div>
  );
}

/* ---------- level 2: all-active timeline ---------- */

function AllItems({
  items,
  onNavigate,
}: {
  items: ItemSummary[];
  onNavigate: () => void;
}) {
  const { t, i18n } = useTranslation();
  if (items.length === 0) {
    return <p className="text-faint text-center py-8">{t("timeline.noActive")}</p>;
  }
  return (
    <div>
      {items.map((item) => (
        <div
          key={item.id}
          className="flex items-center px-3 py-2 border-b border-line/50 hover:bg-surface transition-colors"
        >
          <button
            className="flex-1 text-left min-w-0"
            onClick={() => onNavigate()}
          >
            <div className="flex items-center gap-2">
              <span className="text-xs">{typeIcons[item.type]}</span>
              {item.type === "todo" && item.processing && (
                <Badge tone="accent" className="shrink-0">🔄</Badge>
              )}
              <span className="truncate flex-1">{item.title}</span>
              {item.type === "todo" && (
                <span className={`w-1.5 h-1.5 rounded-full shrink-0 ${priorityDot[item.priority]}`} />
              )}
            </div>
            <div className="flex items-center gap-2 mt-0.5">
              <span className="text-[11px] text-faint">{item.project}</span>
              <span className="text-[11px] text-faint">·</span>
              <span className="text-[11px] text-faint">
                {formatDate(item.updated_at, i18n.resolvedLanguage ?? "zh")}
              </span>
            </div>
          </button>
          <CopyRefButton
            type={item.type}
            title={item.title}
            id={item.id}
            projectPath={item.project_path ?? ""}
            processing={item.processing}
          />
        </div>
      ))}
    </div>
  );
}

/* ---------- level 2: project detail (grouped by type) ---------- */

function ProjectDetailList({
  data,
  collapsed,
  onToggle,
  onNavigate,
}: {
  data: ProjectDetail;
  collapsed: Set<string>;
  onToggle: (type: string) => void;
  onNavigate: () => void;
}) {
  const { t, i18n } = useTranslation();
  const groups = typeOrder.map((type) => ({
    type,
    label: t(typeLabelKey[type] ?? "card.itemTypeTodo"),
    icon: typeIcons[type],
    items: data[type as keyof ProjectDetail] as ItemSummary[],
  }));

  const hasItems = groups.some((g) => g.items.length > 0);

  return (
    <div>
      {!hasItems && (
        <p className="text-faint text-center py-6">{t("timeline.noItems")}</p>
      )}
      {groups.map((g) => {
        if (g.items.length === 0) return null;
        const isCollapsed = collapsed.has(g.type);
        return (
          <div key={g.type}>
            <button
              className="w-full flex items-center gap-2 px-3 py-1.5 border-b border-line/30 bg-surface-dim"
              onClick={() => onToggle(g.type)}
            >
              <span className="text-[11px] text-faint">{isCollapsed ? "▶" : "▼"}</span>
              <span>{g.icon}</span>
              <span className="font-medium">{g.label}</span>
              <span className="text-[11px] text-faint">({g.items.length})</span>
            </button>
            {!isCollapsed &&
              g.items.map((item) => (
                <div
                  key={item.id}
                  className="flex items-center px-3 py-1.5 pl-7 border-b border-line/30 hover:bg-surface transition-colors"
                >
                  <button
                    className="flex-1 text-left min-w-0"
                    onClick={() => onNavigate()}
                  >
                    <div className="flex items-center gap-2">
                      {g.type === "todo" && item.processing && (
                        <Badge tone="accent" className="shrink-0">🔄</Badge>
                      )}
                      <span className="flex-1 truncate">{item.title}</span>
                      {g.type === "todo" && (
                        <span className={`w-1.5 h-1.5 rounded-full shrink-0 ${priorityDot[item.priority]}`} />
                      )}
                      <span className="text-[11px] text-faint shrink-0">
                        {formatDate(item.updated_at, i18n.resolvedLanguage ?? "zh")}
                      </span>
                    </div>
                  </button>
                  <CopyRefButton
                    type={item.type}
                    title={item.title}
                    id={item.id}
                    projectPath={data.project_path}
                    processing={item.processing}
                  />
                </div>
              ))}
          </div>
        );
      })}
    </div>
  );
}

/* ---------- copy reference button ---------- */

function CopyRefButton({
  type,
  title,
  id,
  projectPath,
  processing,
}: {
  type: string;
  title: string;
  id: string;
  projectPath: string;
  processing?: boolean;
}) {
  const { t } = useTranslation();
  const [copied, setCopied] = useState(false);

  function handleCopy() {
    // Protocol text — keep in sync with mcp-server/src/index.ts tool descriptions.
    const label = t(typeLabelKey[type] ?? "card.itemTypeTodo");
    navigator.clipboard.writeText(t("protocol.handle", { type: label, title })).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    });
    // Mark as processing so the board shows this item is being worked on
    if (type === "todo" && !processing && id && projectPath) {
      invoke("update_todo", { projectPath, id, processing: true }).catch(() => {});
    }
  }

  return (
    <button
      className="text-[11px] text-faint hover:text-accent px-1 shrink-0"
      onClick={(e) => { e.stopPropagation(); handleCopy(); }}
      title={t("timeline.copyRef")}
    >
      {copied ? "✓" : "📋"}
    </button>
  );
}
