import { useState, useEffect, useCallback } from "react";
import { useRefreshFallback } from "./useRefreshFallback";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { ArbBoardFile } from "../types/arb";

/**
 * 套利机会栏数据 hook — 全局功能（不按项目区分）。
 * 读 GROS board.json 导出（只读），监听 arb-changed 事件实时刷新。
 */
export function useArbBoard() {
  const [data, setData] = useState<ArbBoardFile | null>(null);
  const [loading, setLoading] = useState(false);

  const refresh = useCallback(async () => {
    setLoading(true);
    try {
      const file = await invoke<ArbBoardFile>("get_arb_board");
      setData(file);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => { refresh(); }, [refresh]);

  useEffect(() => {
    const unlisten = listen<string>("arb-changed", () => { refresh(); });
    return () => { unlisten.then((f) => f()); };
  }, [refresh]);

  // 轮询兜底（S5 后置修复）：arb-changed 事件可能丢失（watcher 配置腿
  // API 失败/启动竞态），挂载单次加载会永久停在旧快照——与 useStore 等
  // 钩子对齐，10s 轮询 + 窗口聚焦刷新。
  useRefreshFallback(refresh);

  return { data, loading, refresh };
}

/** 开/关全局套利机会栏并设置 board.json 路径（写 app 自己的注册表，不碰 GROS 数据）。 */
export async function setArbBoard(enabled: boolean, boardPath: string): Promise<void> {
  await invoke("set_arb_board", { enabled, boardPath });
}

/**
 * 提交自动化控制参数卡（契约 v3 B 控制区）。
 * 走本地薄壳 gros chat-archive → 账房 → 采集机 config-sync 应用 →
 * 下一轮 board.json control 段回显。UI 不本地改值——回显是唯一真值。
 */
export async function submitArbControl(param: string, value: string): Promise<void> {
  await invoke("submit_arb_control", { param, value });
}

/**
 * 执行决策项命令（契约 v4 C：决策点击处置）。
 * cmd 是 GROS 晨报预生成的完整薄壳命令草稿（research / lead-status / settle
 * 三类），app 不组装命令；白名单硬约束在 Rust 侧 run_arb_decision 统一执行
 * （唯一执行口，前端不重复实现）。同一单向双通道纪律：写走薄壳（账房留痕），
 * 读走下一轮 board.json 导出，已处置项自然消失。
 * 成功 resolve（stdout）；失败 reject（原样错误输出，UI 显示不重试）。
 */
export async function runArbDecision(cmd: string): Promise<string> {
  return invoke<string>("run_arb_decision", { cmd });
}
