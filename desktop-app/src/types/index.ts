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
  /** 未完成（非 done）的顶级方案数，方案追踪 tab 的徽标。 */
  active_epics: number;
  /** 套利机会栏是否已为该项目开启（项目级开关，默认关）。 */
  arb_enabled: boolean;
  last_active_at: string;
}
