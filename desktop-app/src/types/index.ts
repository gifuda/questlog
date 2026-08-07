export type ItemType = "knowhow" | "todo" | "idea";
export type ItemStatus = "active" | "completed" | "archived" | "converted";
export type Priority = "high" | "medium" | "low";

export interface TodoItem {
  id: string;
  type: ItemType;
  title: string;
  content: string;
  suggestion: string;
  tags: string[];
  status: ItemStatus;
  priority: Priority;
  source_session: string;
  processing?: boolean;
  created_at: string;
  updated_at: string;
}

export interface TodosFile {
  version: number;
  project: string;
  items: TodoItem[];
}

export interface ProjectInfo {
  name: string;
  path: string;
  counts: { knowhow: number; todo: number; idea: number };
  last_active_at: string;
}
