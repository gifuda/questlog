import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { ProjectInfo, TodosFile, TodoItem, ItemStatus } from "../types";
import { useRefreshFallback } from "./useRefreshFallback";

export function useProjects() {
  const [projects, setProjects] = useState<ProjectInfo[]>([]);
  const [selected, setSelected] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    const list = await invoke<ProjectInfo[]>("get_projects");
    setProjects(list);
    if (!selected && list.length > 0) {
      setSelected(list[0].name);
    }
  }, [selected]);

  useEffect(() => { refresh(); }, [refresh]);

  useEffect(() => {
    const unlisten = listen("projects-changed", () => { refresh(); });
    return () => { unlisten.then((f) => f()); };
  }, [refresh]);

  useRefreshFallback(refresh);

  async function removeProject(name: string): Promise<{ name: string; path: string }> {
    const [removedName, removedPath] = await invoke<[string, string]>("remove_project", { name });
    setProjects((prev) => prev.filter((p) => p.name !== name));
    if (selected === name) {
      setSelected(projects.find((p) => p.name !== name)?.name ?? null);
    }
    return { name: removedName, path: removedPath };
  }

  function reorderProjects(names: string[]) {
    // Optimistic: update state immediately
    setProjects((prev) => {
      const map = new Map(prev.map((p) => [p.name, p]));
      return names.map((n) => map.get(n)).filter(Boolean) as ProjectInfo[];
    });
    // Persist in background
    invoke("reorder_projects", { names });
  }

  return { projects, selected, setSelected, refresh, removeProject, reorderProjects };
}

export function useTodos(projectPath: string | null) {
  const [data, setData] = useState<TodosFile | null>(null);
  const [tags, setTags] = useState<string[]>([]);
  const [tagFilter, setTagFilter] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    if (!projectPath) return;
    const file = await invoke<TodosFile>("get_todos", { projectPath });
    setData(file);
    // Only count tags from active items, top 8 by frequency
    const freq = new Map<string, number>();
    file.items
      .filter((i) => i.status === "active")
      .forEach((i) => i.tags.forEach((t) => freq.set(t, (freq.get(t) ?? 0) + 1)));
    const top = [...freq.entries()]
      .sort((a, b) => b[1] - a[1])
      .slice(0, 8)
      .map(([t]) => t);
    setTags(top);
  }, [projectPath]);

  useEffect(() => { refresh(); }, [refresh]);

  useEffect(() => {
    const unlisten = listen<string>("todos-changed", () => { refresh(); });
    return () => { unlisten.then((f) => f()); };
  }, [refresh]);

  useRefreshFallback(refresh);

  const items = data?.items.filter((i) => i.status === "active") ?? [];
  const filtered = tagFilter ? items.filter((i) => i.tags.includes(tagFilter)) : items;

  const knowhow = filtered.filter((i) => i.type === "knowhow");
  const todos = filtered.filter((i) => i.type === "todo");
  const ideas = filtered.filter((i) => i.type === "idea");

  const doneItems = (data?.items ?? []).filter(
    (i) => i.status === "completed" || i.status === "converted" || i.status === "archived"
  );

  return { knowhow, todos, ideas, doneItems, tags, tagFilter, setTagFilter, refresh };
}

export async function updateItem(
  projectPath: string,
  id: string,
  patch: { status?: ItemStatus; content?: string; tags?: string[]; priority?: string; title?: string; type?: string; processing?: boolean; auto_process?: boolean; auto_ack?: boolean }
) {
  // Tauri maps camelCase JS args to snake_case Rust params — patch keys
  // must be translated explicitly (auto_process → autoProcess; a bare
  // spread silently no-ops, which is exactly the "clicked but nothing
  // happened" bug on the nightly toggle).
  await invoke("update_todo", {
    projectPath,
    id,
    status: patch.status,
    content: patch.content,
    tags: patch.tags,
    priority: patch.priority,
    title: patch.title,
    itemType: patch.type,
    processing: patch.processing,
    autoProcess: patch.auto_process,
    autoAck: patch.auto_ack,
  });
}

export async function addItem(
  projectPath: string,
  itemType: string,
  title: string,
  content?: string,
  suggestion?: string,
  tags?: string[],
  priority?: string
) {
  return invoke<TodoItem>("add_todo", { projectPath, itemType, title, content, suggestion, tags, priority });
}
