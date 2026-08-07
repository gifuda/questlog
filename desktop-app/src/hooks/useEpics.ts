import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { EpicsFile, EpicStatus, GlobalEpic, StepStage } from "../types/epics";

export function useEpics(projectPath: string | null) {
  const [data, setData] = useState<EpicsFile | null>(null);

  const refresh = useCallback(async () => {
    if (!projectPath) return;
    const file = await invoke<EpicsFile>("get_epics", { projectPath });
    setData(file);
  }, [projectPath]);

  useEffect(() => { refresh(); }, [refresh]);

  // 实时刷新：监听 .epics.json 变化
  useEffect(() => {
    const unlisten = listen<string>("epics-changed", () => { refresh(); });
    return () => { unlisten.then((f) => f()); };
  }, [refresh]);

  // ── Mutations ──
  async function addEpic(title: string, opts?: { description?: string; priority?: string; dependsOn?: string[]; parentId?: string }) {
    if (!projectPath) return;
    const file = await invoke<EpicsFile>("add_epic", {
      projectPath,
      title,
      description: opts?.description,
      priority: opts?.priority,
      dependsOn: opts?.dependsOn,
      parentId: opts?.parentId,
    });
    setData(file);
  }

  async function updateEpic(id: string, patch: {
    title?: string; description?: string; status?: EpicStatus; priority?: string;
    dependsOn?: string[]; linksDoc?: string; linksBranch?: string; linksNote?: string;
  }) {
    if (!projectPath) return;
    const file = await invoke<EpicsFile>("update_epic", {
      projectPath, id,
      title: patch.title, description: patch.description, status: patch.status,
      priority: patch.priority, dependsOn: patch.dependsOn,
      linksDoc: patch.linksDoc, linksBranch: patch.linksBranch, linksNote: patch.linksNote,
    });
    setData(file);
  }

  async function deleteEpic(id: string) {
    if (!projectPath) return;
    const file = await invoke<EpicsFile>("delete_epic", { projectPath, id });
    setData(file);
  }

  async function addStep(epicId: string, title: string, description?: string, dependsOn?: string[]) {
    if (!projectPath) return;
    const file = await invoke<EpicsFile>("add_step", { projectPath, epicId, title, description, dependsOn });
    setData(file);
  }

  async function updateStep(id: string, patch: {
    title?: string; description?: string; stage?: StepStage; order?: number; todoRef?: string;
  }) {
    if (!projectPath) return;
    const file = await invoke<EpicsFile>("update_step", {
      projectPath, id,
      title: patch.title, description: patch.description, stage: patch.stage,
      order: patch.order, todoRef: patch.todoRef,
    });
    setData(file);
  }

  async function deleteStep(id: string) {
    if (!projectPath) return;
    const file = await invoke<EpicsFile>("delete_step", { projectPath, id });
    setData(file);
  }

  return { data, epics: data?.epics ?? [], summary: data?.summary ?? null, refresh, addEpic, updateEpic, deleteEpic, addStep, updateStep, deleteStep };
}

/** Global aggregated epics across all projects. */
export function useEpicsGlobal() {
  const [epics, setEpics] = useState<GlobalEpic[]>([]);

  const refresh = useCallback(async () => {
    const result = await invoke<{ epics: GlobalEpic[] }>("get_epics_global");
    setEpics(result.epics);
  }, []);

  useEffect(() => { refresh(); }, [refresh]);

  useEffect(() => {
    const unlisten = listen<string>("epics-changed", () => { refresh(); });
    return () => { unlisten.then((f) => f()); };
  }, [refresh]);

  return { epics, refresh };
}
