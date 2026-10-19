import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useRefreshFallback } from "./useRefreshFallback";
import type { InfraSnapshotEnvelope } from "../types/infra";

/**
 * 服务器底座快照 hook — 全局功能（不按项目区分）。
 * 读家庭服务器侧 launchd 作业（com.agent-base.snapshot-pull，约 5 分钟节奏）
 * 落盘的 snapshot-local.json，经 Rust get_infra_snapshot 状态信封透传（只读）。
 * 该文件不在 watcher 监听面（注册项目之外）→ 轮询是唯一刷新源：
 * 10s 共享节拍 + 窗口聚焦（useRefreshFallback，远小于数据源 5 分钟节奏，
 * 读本地文件开销可忽略）+ 页内手动刷新按钮（F4，由调用方渲染）。
 *
 * invoke 失败必须 catch 干净（useFleetHealth 同款，不同于 useArbBoard 的
 * try/finally）：1420 web dev 形态无 Tauri IPC，invoke 必败 → null 即
 * 「数据源未就绪」降级态，绝不留未处理 rejection。
 *
 * loaded：首次 invoke 落定（成败皆算）——落定前组件显示 "…"，
 * 不把加载中的 null 误判成「数据源未就绪」。
 */
export function useInfraSnapshot() {
  const [envelope, setEnvelope] = useState<InfraSnapshotEnvelope | null>(null);
  const [loaded, setLoaded] = useState(false);

  const refresh = useCallback(async () => {
    try {
      setEnvelope(await invoke<InfraSnapshotEnvelope>("get_infra_snapshot"));
    } catch {
      setEnvelope(null);
    } finally {
      setLoaded(true);
    }
  }, []);

  useEffect(() => { refresh(); }, [refresh]);

  useRefreshFallback(refresh);

  return { envelope, loaded, refresh };
}
