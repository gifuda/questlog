import { useState, useCallback, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useTranslation } from "react-i18next";
import { useProjects } from "./hooks/useStore";
import { useTheme } from "./hooks/useTheme";
import { useUndo } from "./hooks/useUndo";
import KanbanBoard from "./components/KanbanBoard";
import EpicBoard from "./components/EpicBoard";
import Logo from "./components/Logo";
import Settings from "./components/Settings";
import ConfirmButton from "./components/ConfirmButton";

type ModuleTab = "todos" | "epics";

function App() {
  const { t } = useTranslation();
  const { projects, selected, setSelected, removeProject, reorderProjects, refresh } = useProjects();
  const { theme, setTheme } = useTheme();
  const [activeTab, setActiveTab] = useState<ModuleTab>("todos");
  const activeProject = projects.find((p) => p.name === selected);

  // ── Undo ──
  const { undoInfo, trigger: triggerUndo, undo: doUndo } = useUndo();

  async function handleRemove(name: string) {
    const info = await removeProject(name);
    triggerUndo(info);
    setCtxMenu(null);
  }

  async function handleUndo() {
    await doUndo();
    refresh();
  }

  // ── Context menu ──
  const [ctxMenu, setCtxMenu] = useState<{ x: number; y: number; name: string } | null>(null);

  const handleContextMenu = useCallback((e: React.MouseEvent, name: string) => {
    e.preventDefault();
    e.stopPropagation();
    let x = e.clientX;
    let y = e.clientY;
    if (x + 140 > window.innerWidth) x = window.innerWidth - 144;
    if (y + 100 > window.innerHeight) y = e.clientY - 100;
    setCtxMenu({ x, y, name });
  }, []);

  useEffect(() => {
    if (!ctxMenu) return;
    const close = () => setCtxMenu(null);
    window.addEventListener("scroll", close, true);
    window.addEventListener("resize", close);
    return () => {
      window.removeEventListener("scroll", close, true);
      window.removeEventListener("resize", close);
    };
  }, [ctxMenu]);

  function moveProject(name: string, dir: -1 | 1) {
    const names = projects.map((p) => p.name);
    const i = names.indexOf(name);
    const j = i + dir;
    if (j < 0 || j >= names.length) return;
    [names[i], names[j]] = [names[j], names[i]];
    reorderProjects(names);
  }

  const ctxIdx = ctxMenu ? projects.findIndex((p) => p.name === ctxMenu.name) : -1;

  // ── Trash ──
  const [trash, setTrash] = useState<{ name: string; path: string; deleted_at: string }[]>([]);
  const [showTrash, setShowTrash] = useState(false);

  const refreshTrash = useCallback(async () => {
    const list = await invoke<{ name: string; path: string; deleted_at: string }[]>("get_trash");
    setTrash(list);
  }, []);

  useEffect(() => { refreshTrash(); }, [refreshTrash]);

  useEffect(() => {
    const unlisten = listen("projects-changed", () => { refreshTrash(); });
    return () => { unlisten.then((f) => f()); };
  }, [refreshTrash]);

  async function handleRestore(name: string) {
    await invoke("restore_project", { name });
    refresh();
    refreshTrash();
  }

  async function handlePermanentDelete(name: string) {
    await invoke("permanently_delete_project", { name });
    refreshTrash();
  }

  return (
    <div className="h-screen flex flex-col bg-page text-heading select-none" onClick={() => setCtxMenu(null)}>
      {/* Header */}
      <div className="flex items-center justify-between px-4 py-2 border-b border-line">
        <div className="flex items-center gap-2.5">
          <Logo size={20} />
          {/* Module tabs */}
          <div className="flex items-center gap-0.5 ml-3">
            <button
              className={`text-[11px] px-2.5 py-1 rounded-md transition-colors ${
                activeTab === "todos" ? "bg-surface text-heading" : "text-faint hover:text-body"
              }`}
              onClick={() => setActiveTab("todos")}
            >
              {t("app.boardTab")}
            </button>
            <button
              className={`text-[11px] px-2.5 py-1 rounded-md transition-colors ${
                activeTab === "epics" ? "bg-surface text-heading" : "text-faint hover:text-body"
              }`}
              onClick={() => setActiveTab("epics")}
            >
              {t("app.planTab")}
            </button>
          </div>
        </div>
        {activeProject && activeTab === "todos" && (
          <div className="flex items-center gap-3 text-[10px] text-faint">
            <span>📘 {activeProject.counts.knowhow}</span>
            <span>✅ {activeProject.counts.todo}</span>
            <span>💡 {activeProject.counts.idea}</span>
          </div>
        )}
        <Settings theme={theme} onThemeChange={setTheme} />
      </div>

      {/* Board */}
      <div className="flex-1 overflow-hidden">
        {activeProject ? (
          activeTab === "todos" ? (
            <KanbanBoard projectPath={activeProject.path} />
          ) : (
            <EpicBoard projectPath={activeProject.path} />
          )
        ) : (
          <div className="flex items-center justify-center h-full text-faint">
            {projects.length === 0 ? t("app.noProject") : t("app.selectProject")}
          </div>
        )}
      </div>

      {/* Bottom project tabs */}
      {projects.length > 0 && (
        <div className="flex items-center gap-1 px-3 py-2 border-t border-line overflow-x-auto">
          {projects.map((p) => (
            <span key={p.name} className="relative group/tab">
              <button
                className={`text-[11px] px-3 py-1 rounded-full whitespace-nowrap shrink-0 transition-colors ${
                  selected === p.name
                    ? "bg-surface text-heading shadow-sm"
                    : "text-faint hover:text-body hover:bg-surface"
                }`}
                onClick={() => setSelected(p.name)}
                onContextMenu={(e) => handleContextMenu(e, p.name)}
              >
                {p.name}
                {p.counts.todo > 0 && (
                  <span className="ml-1 bg-danger text-white text-[9px] px-1 rounded-full">
                    {p.counts.todo}
                  </span>
                )}
              </button>
              <span className="absolute -top-1 -right-1 opacity-0 group-hover/tab:opacity-100 transition-opacity">
                <ConfirmButton
                  label="×"
                  confirmLabel="✓"
                  onConfirm={() => handleRemove(p.name)}
                  className="w-3.5 h-3.5 flex items-center justify-center text-[8px] bg-surface text-faint hover:text-danger rounded-full border border-line"
                  title={t("app.removeProject")}
                />
              </span>
            </span>
          ))}
        </div>
      )}

      {/* Undo bar */}
      {undoInfo && (
        <div className="flex items-center gap-2 px-4 py-1.5 bg-yellow-100 border-t border-yellow-300">
          <span className="text-[11px] text-yellow-800 flex-1">{t("app.undoRemove", { name: undoInfo.name })}</span>
          <button
            className="text-[11px] text-yellow-800 font-bold underline"
            onClick={handleUndo}
          >
            {t("app.undo")}
          </button>
        </div>
      )}

      {/* Trash */}
      {trash.length > 0 && (
        <div className="border-t border-line">
          <button
            className="w-full flex items-center gap-2 px-4 py-1.5 text-[11px] text-faint hover:text-body transition-colors"
            onClick={() => setShowTrash(!showTrash)}
          >
            <span className={`transition-transform ${showTrash ? "rotate-90" : ""}`}>▶</span>
            {t("app.trash")}
            <span className="bg-chip px-1.5 py-0.5 rounded text-[10px]">{trash.length}</span>
          </button>
          {showTrash && (
            <div className="px-4 pb-2 flex flex-col gap-1 max-h-40 overflow-y-auto">
              {trash.map((tr) => (
                <div key={tr.name} className="flex items-center gap-2 py-1 px-2 rounded bg-surface/50">
                  <span className="text-[11px] text-muted flex-1 truncate">{tr.name}</span>
                  <button
                    className="text-[10px] text-accent hover:underline"
                    onClick={() => handleRestore(tr.name)}
                  >
                    {t("app.restore")}
                  </button>
                  <ConfirmButton
                    label={t("app.delete")}
                    confirmLabel={t("app.confirm")}
                    onConfirm={() => handlePermanentDelete(tr.name)}
                    className="text-[10px] text-faint hover:text-danger"
                  />
                </div>
              ))}
            </div>
          )}
        </div>
      )}

      {/* Context menu: move up / move down / remove */}
      {ctxMenu && (
        <div
          className="fixed bg-surface border border-line rounded-lg shadow-xl py-1 z-50 min-w-[110px]"
          style={{ left: ctxMenu.x, top: ctxMenu.y }}
          onClick={(e) => e.stopPropagation()}
        >
          <button
            className="w-full text-left px-3 py-1.5 text-xs text-body hover:bg-elevated transition-colors disabled:opacity-30"
            disabled={ctxIdx <= 0}
            onClick={() => { moveProject(ctxMenu.name, -1); setCtxMenu(null); }}
          >
            {t("app.moveUp")}
          </button>
          <button
            className="w-full text-left px-3 py-1.5 text-xs text-body hover:bg-elevated transition-colors disabled:opacity-30"
            disabled={ctxIdx < 0 || ctxIdx >= projects.length - 1}
            onClick={() => { moveProject(ctxMenu.name, 1); setCtxMenu(null); }}
          >
            {t("app.moveDown")}
          </button>
          <div className="my-1 border-t border-line" />
          <ConfirmButton
            label={t("app.removeProject")}
            confirmLabel={t("app.confirmRemove")}
            onConfirm={() => handleRemove(ctxMenu.name)}
            className="w-full text-left px-3 py-1.5 text-xs text-danger hover:bg-elevated transition-colors"
          />
        </div>
      )}

    </div>
  );
}

export default App;
