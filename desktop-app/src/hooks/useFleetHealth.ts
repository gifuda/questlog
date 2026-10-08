import { useState, useCallback, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

export interface NightDigest {
  date: string;
  total: number;
  accepted: number;
  review: number;
  failed: number;
  other: number;
}

export interface FleetHealth {
  schedule_enabled: boolean;
  sweep_state: { last_run_date: string; last_missed_date: string; updated_at: string };
  window_missed: boolean;
  outbox_pending: number;
  receipts_pending: number;
  receipts_unmatched: number;
  in_flight: number;
  last_applied_at: string | null;
  digest: NightDigest | null;
}

/** 待办 → 夜间任务链路（账本 todo-tasks.json 的读时投影）。 */
export interface TodoTaskLink {
  state: "preparing" | "published" | "closed" | "superseded";
  /** 是否有关联的方案（跳转用，不暴露内部 id）。 */
  has_epic: boolean;
  outcome: string | null;
}

/**
 * useTodoStatus — 项目级夜间管线状态：调度是否启用、是否在名单、每条待办的任务链路。
 * 链路数据随 todos/epics 事件刷新（产包会建 epic，回写会改 todo）。
 */
export function useTodoStatus(projectPath: string | null) {
  const [status, setStatus] = useState<{
    schedule_enabled: boolean;
    nightly_scheduled: boolean | null;
    links: Record<string, TodoTaskLink>;
  } | null>(null);

  const refresh = useCallback(async () => {
    if (!projectPath) {
      setStatus(null);
      return;
    }
    try {
      setStatus(
        await invoke<{
          schedule_enabled: boolean;
          nightly_scheduled: boolean | null;
          links: Record<string, TodoTaskLink>;
        }>("get_todo_status", { projectPath }),
      );
    } catch {
      setStatus(null);
    }
  }, [projectPath]);

  useEffect(() => {
    refresh();
    const un1 = listen("todos-changed", refresh);
    const un2 = listen("epics-changed", refresh);
    return () => {
      un1.then((f) => f());
      un2.then((f) => f());
    };
  }, [refresh]);

  return {
    scheduleEnabled: status?.schedule_enabled ?? null,
    scheduled: status?.nightly_scheduled ?? null,
    links: status?.links ?? {},
  };
}
/**
 * useFleetHealth — 轮询跨机链路健康快照（夜间管线是低频链路，15s 足够；
 * watcher 对 todo-outbox/todo-receipts 无事件，轮询是唯一刷新源）。
 * 命令失败（如 ledger 损坏 fail-closed）时返回 null，调用方隐藏灯带。
 */
export function useFleetHealth() {
  const [health, setHealth] = useState<FleetHealth | null>(null);

  const refresh = useCallback(async () => {
    try {
      setHealth(await invoke<FleetHealth>("get_fleet_health"));
    } catch {
      setHealth(null);
    }
  }, []);

  useEffect(() => {
    refresh();
    const interval = setInterval(refresh, 15000);
    return () => clearInterval(interval);
  }, [refresh]);

  return { health, refresh };
}
