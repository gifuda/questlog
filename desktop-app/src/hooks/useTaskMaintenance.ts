import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useRefreshFallback } from "./useRefreshFallback";
import { parseTaskMaintenance } from "../taskMaintenanceLogic";
import type { TaskMaintenance } from "../types/taskMaintenance";

let inFlight: Promise<TaskMaintenance | null> | null = null;

function fetchTaskMaintenance(): Promise<TaskMaintenance | null> {
  if (!inFlight) {
    inFlight = invoke<unknown>("get_task_maintenance")
      .then((value) => parseTaskMaintenance(value))
      .catch(() => null)
      .finally(() => { inFlight = null; });
  }
  return inFlight;
}

/** Shared read-only poll for all three desktop surfaces; a failed read clears prior data. */
export function useTaskMaintenance() {
  const [projection, setProjection] = useState<TaskMaintenance | null>(null);
  const [loading, setLoading] = useState(true);
  const refresh = useCallback(async () => {
    const result = await fetchTaskMaintenance();
    setProjection(result);
    setLoading(false);
  }, []);

  useEffect(() => { void refresh(); }, [refresh]);
  useRefreshFallback(refresh);

  return { projection, loading, refresh };
}
