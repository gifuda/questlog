import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { ArbBoardFile } from "../types/arb";

/**
 * 套利机会栏数据 hook — 读 GROS board.json 导出（只读）。
 * 监听 arb-changed 事件（watcher 对 board.json 父目录的监听）实时刷新。
 */
export function useArbBoard(projectPath: string | null) {
  const [data, setData] = useState<ArbBoardFile | null>(null);
  const [loading, setLoading] = useState(false);

  const refresh = useCallback(async () => {
    if (!projectPath) return;
    setLoading(true);
    try {
      const file = await invoke<ArbBoardFile>("get_arb_board", { projectPath });
      setData(file);
    } finally {
      setLoading(false);
    }
  }, [projectPath]);

  useEffect(() => { refresh(); }, [refresh]);

  useEffect(() => {
    const unlisten = listen<string>("arb-changed", () => { refresh(); });
    return () => { unlisten.then((f) => f()); };
  }, [refresh]);

  return { data, loading, refresh };
}

/** 开/关项目的套利机会栏（写 app 自己的注册表，不碰 GROS 数据）。 */
export async function setArbBoard(projectPath: string, enabled: boolean, boardPath: string): Promise<void> {
  await invoke("set_arb_board", { projectPath, enabled, boardPath });
}
