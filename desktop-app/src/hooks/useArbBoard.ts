import { useState, useEffect, useCallback } from "react";
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

  return { data, loading, refresh };
}

/** 开/关全局套利机会栏并设置 board.json 路径（写 app 自己的注册表，不碰 GROS 数据）。 */
export async function setArbBoard(enabled: boolean, boardPath: string): Promise<void> {
  await invoke("set_arb_board", { enabled, boardPath });
}
