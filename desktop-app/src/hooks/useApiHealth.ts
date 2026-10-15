import { useEffect, useRef, useState, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";

export interface ApiHealth {
  mode: "local" | "remote";
  ok: boolean;
  /** 上次成功探针的时刻；离线横条显示它，恢复后自动清。 */
  lastSuccessAt: Date | null;
}

/**
 * 权威端可达性探针（离线指示条的数据源）。
 * 15s 轮询 + 窗口聚焦即刷；远端模式断开时由 App 顶部横条呈现，
 * 本地模式恒 ok 且不显示横条。失败瞬间即标记（探针本身有 3s 连接上界）。
 */
export function useApiHealth(intervalMs = 15_000): ApiHealth {
  const [health, setHealth] = useState<ApiHealth>({ mode: "local", ok: true, lastSuccessAt: new Date() });
  const lastSuccess = useRef<Date | null>(new Date());

  const probe = useCallback(async () => {
    try {
      const res = await invoke<{ ok: boolean; mode: "local" | "remote" }>("get_api_health");
      if (res.mode !== "remote") {
        setHealth({ mode: "local", ok: true, lastSuccessAt: null });
        return;
      }
      if (res.ok) {
        lastSuccess.current = new Date();
        setHealth({ mode: "remote", ok: true, lastSuccessAt: lastSuccess.current });
      } else {
        setHealth((h) => ({ mode: "remote", ok: false, lastSuccessAt: lastSuccess.current ?? h.lastSuccessAt }));
      }
    } catch {
      // 命令层异常（非"权威端断开"事实）按离线呈现——fail-visible
      setHealth((h) => ({ mode: "remote", ok: false, lastSuccessAt: h.lastSuccessAt }));
    }
  }, []);

  useEffect(() => {
    probe();
    const onFocus = () => probe();
    window.addEventListener("focus", onFocus);
    const interval = setInterval(probe, intervalMs);
    return () => {
      window.removeEventListener("focus", onFocus);
      clearInterval(interval);
    };
  }, [probe, intervalMs]);

  return health;
}
