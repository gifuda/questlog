import { useState, useCallback, useEffect, useMemo } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useTranslation } from "react-i18next";
import { useRefreshFallback } from "../hooks/useRefreshFallback";
import { epicStatusBadge } from "../epicConstants";
import type { EpicStatus } from "../types/epics";

/** get_epics_global 的行形状（后端返回的是 Epic 的子集，另带 project 归属）。 */
interface OverviewEpic {
  id: string;
  title: string;
  status: EpicStatus;
  status_label: string;
  priority: string;
  progress: number;
  step_count: number;
  project: string;
}

/** get_all_active 的行形状。 */
interface ActiveItem {
  id: string;
  type: string;
  title: string;
  priority: string;
  project: string;
}

interface ProjectInfo {
  name: string;
  path: string;
}

interface OverviewProps {
  projects: ProjectInfo[];
  /** 点击项目行：跳转该项目的方案追踪页。 */
  onOpenProject: (name: string) => void;
}

/**
 * 跨项目总览 — 主窗口级"今天早上先看哪里"。
 * 每项目一行：在办计数 + 焦点方案（后端已按状态/优先级排序，取第一条）。
 * 数据复用托盘面板同款只读命令，无新增写路径。
 */
export default function Overview({ projects, onOpenProject }: OverviewProps) {
  const { t } = useTranslation();
  const [epics, setEpics] = useState<OverviewEpic[]>([]);
  const [items, setItems] = useState<ActiveItem[]>([]);

  const refresh = useCallback(async () => {
    try {
      const [eg, ai] = await Promise.all([
        invoke<{ epics: OverviewEpic[] }>("get_epics_global"),
        invoke<ActiveItem[]>("get_all_active"),
      ]);
      setEpics(eg.epics);
      setItems(ai);
    } catch {
      // 单项目读取失败时后端已跳过该项目；整体失败保持上次快照。
    }
  }, []);

  useEffect(() => {
    refresh();
    const un1 = listen("todos-changed", refresh);
    const un2 = listen("epics-changed", refresh);
    return () => {
      un1.then((f) => f());
      un2.then((f) => f());
    };
  }, [refresh]);
  useRefreshFallback(refresh);

  const rows = useMemo(() => {
    return projects
      .map((p) => {
        const projItems = items.filter((i) => i.project === p.name);
        // 已完成方案不占焦点位；后端排序（active 优先，再按优先级）直接沿用。
        const projEpics = epics.filter((e) => e.project === p.name && e.status !== "done");
        return {
          name: p.name,
          todoCount: projItems.filter((i) => i.type === "todo").length,
          knowhowCount: projItems.filter((i) => i.type === "knowhow").length,
          ideaCount: projItems.filter((i) => i.type === "idea").length,
          epicCount: projEpics.length,
          blocked: projEpics.some((e) => e.status === "blocked"),
          focus: projEpics[0],
        };
      })
      .sort((a, b) => {
        // 第一阶：空项目沉底；第二阶：阻塞优先；第三阶：在办总量。
        const ea = a.epicCount + a.todoCount + a.knowhowCount + a.ideaCount === 0 ? 1 : 0;
        const eb = b.epicCount + b.todoCount + b.knowhowCount + b.ideaCount === 0 ? 1 : 0;
        if (ea !== eb) return ea - eb;
        if (a.blocked !== b.blocked) return a.blocked ? -1 : 1;
        return b.epicCount + b.todoCount - (a.epicCount + a.todoCount);
      });
  }, [projects, epics, items]);

  const allEmpty = rows.every((r) => r.epicCount === 0 && r.todoCount === 0 && r.knowhowCount === 0 && r.ideaCount === 0);

  return (
    <div className="h-full overflow-y-auto px-4 py-3">
      {allEmpty ? (
        <div className="flex flex-col items-center justify-center h-full text-faint gap-2">
          <span className="text-3xl">🧭</span>
          <span className="text-sm">{t("overview.empty")}</span>
        </div>
      ) : (
        <div className="flex flex-col gap-1.5">
          {rows.map((r) => {
            const empty = r.epicCount === 0 && r.todoCount === 0 && r.knowhowCount === 0 && r.ideaCount === 0;
            const badge = r.focus ? epicStatusBadge[r.focus.status] : null;
            return (
              <button
                key={r.name}
                onClick={() => onOpenProject(r.name)}
                title={t("overview.jumpTitle")}
                className={`w-full text-left px-3 py-2 rounded-lg border border-line bg-surface/60 hover:bg-surface transition-colors ${
                  empty ? "opacity-45" : ""
                }`}
              >
                <div className="flex items-center gap-3 min-w-0">
                  <span className="text-[13px] font-medium text-heading shrink-0">{r.name}</span>
                  <span className="flex items-center gap-2 text-[11px] text-muted shrink-0">
                    {r.todoCount > 0 && <span>✅ {r.todoCount}</span>}
                    {r.knowhowCount > 0 && <span>📘 {r.knowhowCount}</span>}
                    {r.ideaCount > 0 && <span>💡 {r.ideaCount}</span>}
                    {r.epicCount > 0 && (
                      <span className="text-accent">{t("overview.epicCount", { count: r.epicCount })}</span>
                    )}
                  </span>
                  {r.focus && badge && (
                    <span className="flex items-center gap-1.5 text-[11px] text-muted min-w-0 ml-auto">
                      <span className={`w-1.5 h-1.5 rounded-full shrink-0 ${badge.dot}`} />
                      <span className="shrink-0">{r.focus.status_label}</span>
                      <span className="truncate text-body">{r.focus.title}</span>
                      <span className="shrink-0 tabular-nums text-faint">{r.focus.progress}%</span>
                    </span>
                  )}
                </div>
              </button>
            );
          })}
        </div>
      )}
    </div>
  );
}
