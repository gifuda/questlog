import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { useTranslation } from "react-i18next";
import ConfirmButton from "./ConfirmButton";
import { useUndo } from "../hooks/useUndo";
import { typeIcons, priorityDot, typeOrder, ARCHIVE_DAYS } from "../constants";
import { formatDate } from "../utils";

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

/* ---------- main component ---------- */

type View =
  | { kind: "projects" }
  | { kind: "all" }
  | { kind: "project"; data: ProjectDetail };

export default function TimelinePanel() {
  const { t } = useTranslation();
  const [view, setView] = useState<View>({ kind: "projects" });
  const [projects, setProjects] = useState<ProjectSummary[]>([]);
  const [allItems, setAllItems] = useState<ItemSummary[]>([]);
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set());

  const refreshProjects = useCallback(async () => {
    const list = await invoke<ProjectSummary[]>("get_projects");
    setProjects(list);
  }, []);

  const refreshAll = useCallback(async () => {
    const list = await invoke<ItemSummary[]>("get_all_active");
    setAllItems(list);
  }, []);

  useEffect(() => {
    refreshProjects();
  }, [refreshProjects]);

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

  function openInMain(projectPath: string) {
    invoke("navigate_to_project", { projectPath }).catch(() => {});
    getCurrentWindow().hide();
  }

  const totalActive = projects.reduce(
    (s, p) => s + p.counts.knowhow + p.counts.todo + p.counts.idea,
    0
  );

  const { undoInfo, trigger: triggerUndo, undo: doUndo } = useUndo();

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
      {/* ---- header ---- */}
      <Header view={view} totalActive={totalActive} goBack={() => setView({ kind: "projects" })} />

      {/* ---- undo bar ---- */}
      {undoInfo && (
        <div className="flex items-center gap-2 px-3 py-2 bg-yellow-100 border-b border-yellow-300">
          <span className="text-xs text-yellow-800 flex-1">{t("timeline.undoRemove", { name: undoInfo.name })}</span>
          <button
            className="text-xs text-yellow-800 font-bold underline"
            onClick={handleUndo}
          >
            {t("timeline.undo")}
          </button>
        </div>
      )}

      {/* ---- body ---- */}
      <div className="flex-1 overflow-y-auto min-h-0">
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
      </div>
    </div>
  );
}

/* ---------- header ---------- */

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
    view.kind === "projects"
      ? t("timeline.projectList")
      : view.kind === "all"
        ? t("timeline.allActive")
        : view.data.project;

  function showMain() {
    invoke("show_main_window").catch(() => {});
  }

  return (
    <div className="flex items-center gap-2 px-3 py-2 border-b border-line">
      {view.kind !== "projects" && (
        <button onClick={goBack} className="text-accent hover:opacity-70 transition-opacity">
          ←
        </button>
      )}
      <span className="font-bold flex-1 truncate">{title}</span>
      <span className="text-[10px] text-faint">{t("timeline.count", { total: totalActive })}</span>
      <button onClick={showMain} className="text-[10px] text-faint hover:text-heading" title={t("timeline.showMain")}>⊞</button>
      <ConfirmButton
        label="✕"
        confirmLabel="✕"
        onConfirm={() => invoke("quit_app").catch(() => {})}
        className="text-[10px] text-faint hover:text-danger"
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
          <span className="text-[10px] text-faint">{totalActive}</span>
        </div>
      </button>
      {active.map((p) => (
        <ProjectRow key={p.path} project={p} onSelect={onSelect} onRemove={handleRemove} />
      ))}
      {inactive.length > 0 && (
        <>
          <button
            className="w-full text-left px-3 py-1.5 border-b border-line/30 bg-surface/30 hover:bg-surface/50 transition-colors"
            onClick={() => setShowInactive((v) => !v)}
          >
            <span className="text-[10px] text-faint">
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
        <span className="text-[10px] text-faint shrink-0">
          📘{project.counts.knowhow} ✅{project.counts.todo} 💡{project.counts.idea}
        </span>
        <span className="text-[10px] text-faint shrink-0">{total}</span>
      </div>
      <ConfirmButton
        label="✕"
        confirmLabel={t("timeline.confirm")}
        onConfirm={() => onRemove(project.name)}
        className="text-[10px] text-faint hover:text-danger transition-colors shrink-0 ml-1"
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
  onNavigate: (projectPath: string) => void;
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
            onClick={() => onNavigate(item.project_path!)}
          >
            <div className="flex items-center gap-2">
              <span className="text-xs">{typeIcons[item.type]}</span>
              {item.type === "todo" && item.processing && (
                <span className="text-[9px] px-1 py-0.5 rounded bg-accent-bg text-accent shrink-0">🔄</span>
              )}
              <span className="truncate flex-1">{item.title}</span>
              {item.type === "todo" && (
                <span className={`w-1.5 h-1.5 rounded-full shrink-0 ${priorityDot[item.priority]}`} />
              )}
            </div>
            <div className="flex items-center gap-2 mt-0.5">
              <span className="text-[10px] text-faint">{item.project}</span>
              <span className="text-[10px] text-faint">·</span>
              <span className="text-[10px] text-faint">
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
  onNavigate: (projectPath: string) => void;
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
              className="w-full flex items-center gap-2 px-3 py-1.5 border-b border-line/30 bg-surface/50"
              onClick={() => onToggle(g.type)}
            >
              <span className="text-[10px] text-faint">{isCollapsed ? "▶" : "▼"}</span>
              <span>{g.icon}</span>
              <span className="font-medium">{g.label}</span>
              <span className="text-[10px] text-faint">({g.items.length})</span>
            </button>
            {!isCollapsed &&
              g.items.map((item) => (
                <div
                  key={item.id}
                  className="flex items-center px-3 py-1.5 pl-7 border-b border-line/30 hover:bg-surface transition-colors"
                >
                  <button
                    className="flex-1 text-left min-w-0"
                    onClick={() => onNavigate(data.project_path)}
                  >
                    <div className="flex items-center gap-2">
                      {g.type === "todo" && item.processing && (
                        <span className="text-[9px] px-1 py-0.5 rounded bg-accent-bg text-accent shrink-0">🔄</span>
                      )}
                      <span className="flex-1 truncate">{item.title}</span>
                      {g.type === "todo" && (
                        <span className={`w-1.5 h-1.5 rounded-full shrink-0 ${priorityDot[item.priority]}`} />
                      )}
                      <span className="text-[10px] text-faint shrink-0">
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
      className="text-[10px] text-faint hover:text-accent px-1 shrink-0"
      onClick={(e) => { e.stopPropagation(); handleCopy(); }}
      title={t("timeline.copyRef")}
    >
      {copied ? "✓" : "📋"}
    </button>
  );
}
